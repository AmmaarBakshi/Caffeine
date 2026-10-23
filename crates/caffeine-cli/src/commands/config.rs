//! `caffeine config ...`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use caffeine_core::config::{self, ConfigFile, settings};
use caffeine_core::{Error, ErrorKind, Result, fsutil};

use crate::cli::ConfigAction;
use crate::context::Ctx;
use crate::output::{Style, confirm};

pub fn run(ctx: &mut Ctx, action: &ConfigAction, secret_paths: &[String]) -> Result<ExitCode> {
    match action {
        ConfigAction::Path => {
            ctx.out.json(&serde_json::json!({
                "config_file": ctx.paths.config_file,
                "exists": ctx.paths.config_file.exists(),
                "paths": ctx.paths,
            }));
            ctx.out.answer(ctx.paths.config_file.display().to_string());
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Show => show(ctx, secret_paths),
        ConfigAction::Get { key } => {
            let cfg = loaded(ctx)?;
            let value = cfg.get(key)?;
            ctx.out.json(&serde_json::json!({ "key": key, "value": value }));
            ctx.out.answer(value);
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Set { key, value } => modify(ctx, |cfg| cfg.set(key, value), key),
        ConfigAction::Unset { key } => modify(ctx, |cfg| cfg.unset(key), key),
        ConfigAction::Edit => edit(ctx),
        ConfigAction::Validate => validate(ctx),
        ConfigAction::Init => init(ctx),
        ConfigAction::Reset { yes } => reset(ctx, *yes),
        ConfigAction::List => list(ctx),
    }
}

/// The configuration, or the load error for commands that must not silently
/// work on defaults.
fn loaded(ctx: &Ctx) -> Result<&ConfigFile> {
    match &ctx.config_error {
        Some(e) => Err(Error::config(e.message().to_string())
            .with_hint(e.hint().unwrap_or("run `caffeine config validate`").to_string())),
        None => Ok(&ctx.config),
    }
}

fn show(ctx: &Ctx, secret_paths: &[String]) -> Result<ExitCode> {
    ctx.warn_config_problems();
    let json = config::effective_json(ctx.config.config(), secret_paths);
    ctx.out.json(&serde_json::json!({ "config_file": ctx.config.path(), "effective": json }));
    if ctx.out.is_json() {
        return Ok(ExitCode::SUCCESS);
    }
    ctx.out.line(ctx.out.paint(Style::Dim, format!("# effective configuration ({})", ctx.config.path().display())));
    ctx.out.answer(render_toml(&json).trim_end());
    Ok(ExitCode::SUCCESS)
}

/// Render the effective (JSON) configuration as TOML. TOML has no null, so
/// unset optional settings are omitted.
fn render_toml(json: &serde_json::Value) -> String {
    fn strip_nulls(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), strip_nulls(v))).collect(),
            ),
            other => other.clone(),
        }
    }
    toml_edit::ser::to_string_pretty(&strip_nulls(json)).unwrap_or_else(|e| format!("# cannot render: {e}"))
}

fn modify(ctx: &mut Ctx, change: impl FnOnce(&mut ConfigFile) -> Result<()>, key: &str) -> Result<ExitCode> {
    loaded(ctx)?;
    let created = !ctx.config.exists_on_disk();
    change(&mut ctx.config)?;
    ctx.config.save()?;
    let value = ctx.config.get(key).unwrap_or_default();
    ctx.out.json(&serde_json::json!({ "key": key, "value": value, "config_file": ctx.config.path() }));
    if created {
        ctx.out.note(format!("created {}", ctx.config.path().display()));
    }
    ctx.out.line(format!("{key} = {value}"));
    Ok(ExitCode::SUCCESS)
}

fn validate(ctx: &Ctx) -> Result<ExitCode> {
    let path = ctx.config.path();
    let result = ConfigFile::load(path);
    let (ok, problems, notes) = match &result {
        Ok(cfg) => (true, Vec::new(), cfg.notes().to_vec()),
        Err(e) => (false, vec![e.message().to_string()], Vec::new()),
    };
    ctx.out.json(&serde_json::json!({
        "config_file": path,
        "exists": path.exists(),
        "valid": ok,
        "errors": problems,
        "warnings": notes,
    }));
    if !path.exists() {
        ctx.out.line(format!("{} does not exist; defaults are in effect.", path.display()));
    } else if ok {
        ctx.out.line(format!("{} {}", ctx.out.paint(Style::Good, "valid:"), path.display()));
    }
    for n in &notes {
        ctx.out.warn(n.clone());
    }
    match result {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) if ctx.out.is_json() => Ok(ExitCode::from(e.exit_code() as u8)),
        Err(e) => Err(e),
    }
}

fn init(ctx: &Ctx) -> Result<ExitCode> {
    let path = ctx.config.path();
    if path.exists() {
        ctx.out.json(&serde_json::json!({ "created": false, "config_file": path }));
        ctx.out.line(format!("{} already exists; nothing was changed.", path.display()));
        return Ok(ExitCode::SUCCESS);
    }
    fsutil::write_atomic(path, config::template().as_bytes(), false)?;
    ctx.out.json(&serde_json::json!({ "created": true, "config_file": path }));
    ctx.out.line(format!("{} {}", ctx.out.paint(Style::Good, "created"), path.display()));
    Ok(ExitCode::SUCCESS)
}

fn reset(ctx: &Ctx, yes: bool) -> Result<ExitCode> {
    let path = ctx.config.path();
    if !yes && path.exists() {
        match confirm(&format!("Replace {} with defaults? (a backup is kept)", path.display())) {
            Some(true) => {}
            Some(false) => return Err(Error::failure("cancelled; nothing was changed")),
            None => return Err(Error::usage("refusing to reset without confirmation").with_hint("re-run with --yes")),
        }
    }
    let backup = ConfigFile::reset(path)?;
    ctx.out.json(&serde_json::json!({ "config_file": path, "backup": backup }));
    ctx.out.line(format!("{} {}", ctx.out.paint(Style::Good, "reset"), path.display()));
    if let Some(b) = backup {
        ctx.out.line(format!("  previous file saved as {}", b.display()));
    }
    Ok(ExitCode::SUCCESS)
}

fn list(ctx: &Ctx) -> Result<ExitCode> {
    let defs = settings::all();
    let rows: Vec<_> = defs
        .iter()
        .map(|d| {
            serde_json::json!({
                "key": d.key,
                "section": d.section.title(),
                "label": d.label,
                "type": d.kind.describe(),
                "value": ctx.config.get(d.key).unwrap_or_default(),
                "help": d.help,
            })
        })
        .collect();
    ctx.out.json(&serde_json::json!({ "settings": rows }));
    let mut section = None;
    for d in &defs {
        if section != Some(d.section) {
            section = Some(d.section);
            ctx.out.line(format!("\n{}", ctx.out.paint(Style::Bold, d.section.title())));
        }
        ctx.out.line(format!(
            "  {:<30} {}",
            d.key,
            ctx.out.paint(Style::Accent, ctx.config.get(d.key).unwrap_or_default())
        ));
        ctx.out.line(format!("  {:<30} {}", "", ctx.out.paint(Style::Dim, d.kind.describe())));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------- edit --

/// Split an `$EDITOR` value into program and arguments, honoring simple
/// single and double quotes. No shell is involved.
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    args.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        args.push(cur);
    }
    args
}

fn find_editor() -> Option<Vec<String>> {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            let parts = split_command_line(&v);
            if !parts.is_empty() {
                return Some(parts);
            }
        }
    }
    let candidates: &[&str] = if cfg!(windows) { &["notepad.exe"] } else { &["nano", "vim", "vi"] };
    candidates.iter().find(|c| which(c).is_some()).map(|c| vec![c.to_string()])
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

fn edit(ctx: &mut Ctx) -> Result<ExitCode> {
    if ctx.out.is_json() {
        return Err(Error::usage("`config edit` is interactive and does not support --json"));
    }
    let editor = find_editor().ok_or_else(|| {
        Error::not_found("no editor found (set $VISUAL or $EDITOR)")
            .with_hint("or use the built-in editor: `caffeine --config`")
    })?;
    let path = ctx.config.path().to_path_buf();
    let original = fsutil::read_optional(&path)?;
    let mut text = original.clone().unwrap_or_else(config::template);
    let scratch = scratch_path(&path);
    loop {
        fsutil::write_atomic(&scratch, text.as_bytes(), false)?;
        let status = std::process::Command::new(&editor[0])
            .args(&editor[1..])
            .arg(&scratch)
            .status()
            .map_err(|e| Error::io(format!("starting editor `{}`", editor[0]), e))?;
        if !status.success() {
            return Err(Error::failure(format!("editor exited with {status}; your edits are in {}", scratch.display())));
        }
        let edited = fsutil::read_optional(&scratch)?.unwrap_or_default();
        if Some(&edited) == original.as_ref() || (original.is_none() && edited == config::template()) {
            fsutil::remove_if_exists(&scratch)?;
            ctx.out.line("No changes.");
            return Ok(ExitCode::SUCCESS);
        }
        match ConfigFile::from_text(&path, &edited) {
            Ok(_) => {
                if fsutil::read_optional(&path)? != original {
                    return Err(Error::conflict(format!(
                        "{} changed on disk while you were editing; your version is in {}",
                        path.display(),
                        scratch.display()
                    )));
                }
                fsutil::write_atomic(&path, edited.as_bytes(), true)?;
                fsutil::remove_if_exists(&scratch)?;
                ctx.out.line(format!("{} {}", ctx.out.paint(Style::Good, "saved"), path.display()));
                return Ok(ExitCode::SUCCESS);
            }
            Err(e) => {
                ctx.out.error(&e);
                match confirm("Re-open the editor to fix it?") {
                    Some(false) | None => {
                        return Err(Error::new(
                            ErrorKind::Config,
                            format!("configuration not saved; your edits are in {}", scratch.display()),
                        ));
                    }
                    Some(true) => text = edited,
                }
            }
        }
    }
}

fn scratch_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "config.toml".into());
    path.with_file_name(format!(".{name}.edit-{}.toml", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::split_command_line;

    #[test]
    fn editor_command_lines() {
        assert_eq!(split_command_line("nano"), ["nano"]);
        assert_eq!(split_command_line("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_command_line(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst"#),
            [r"C:\Program Files\Notepad++\notepad++.exe", "-multiInst"]
        );
        assert_eq!(split_command_line("  vim  -u 'my vimrc' "), ["vim", "-u", "my vimrc"]);
        assert!(split_command_line("   ").is_empty());
    }
}
