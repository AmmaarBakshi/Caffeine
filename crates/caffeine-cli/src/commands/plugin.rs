//! `caffeine plugin ...` and plugin-provided commands.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::Value;

use caffeine_core::power::{CheckStatus, DiagnosticCheck};
use caffeine_core::{Error, Result};
use caffeine_plugin_api::{Host, LogLevel, Permission, Problem};
use caffeine_plugin_manager::scaffold::{self, ApiSource};
use caffeine_plugin_manager::{
    Builtin, Manager, ManagerOptions, Origin, PluginEntry, RunRequest, TrustStatus,
    ungranted_permissions,
};

use crate::cli::PluginAction;
use crate::context::Ctx;
use crate::output::{Style, confirm};

/// Plugins compiled into this binary.
pub fn builtins() -> Vec<Builtin> {
    vec![Builtin {
        factory: caffeine_plugin_build::new,
    }]
}

pub fn host_version() -> semver::Version {
    env!("CARGO_PKG_VERSION")
        .parse()
        .expect("package version is semver")
}

/// Discover plugins with the current configuration.
pub fn manager(ctx: &Ctx) -> Result<Manager> {
    let mut options = ManagerOptions::new(&ctx.paths, ctx.config.config(), host_version());
    options.builtins = builtins();
    options.reserved_commands = crate::cli::reserved_command_names();
    Manager::discover(options)
}

pub fn run(ctx: &mut Ctx, action: &PluginAction) -> Result<ExitCode> {
    match action {
        PluginAction::List => list(ctx),
        PluginAction::Info { name } => info(ctx, name),
        PluginAction::Enable { name, yes } => enable(ctx, name, *yes),
        PluginAction::Disable { name } => disable(ctx, name),
        PluginAction::Install { path, upgrade } => install(ctx, path, *upgrade),
        PluginAction::Remove { name, yes } => remove(ctx, name, *yes),
        PluginAction::Create {
            name,
            path,
            api_path,
        } => create(ctx, name, path.as_deref(), api_path.as_deref()),
        PluginAction::Validate { path } => validate(ctx, path),
        PluginAction::Run {
            name,
            command,
            args,
        } => {
            let m = manager(ctx)?;
            let entry = m.require(name)?;
            let command = match command {
                Some(c) => c.clone(),
                None => entry
                    .manifest
                    .commands
                    .first()
                    .map(|c| c.name.clone())
                    .ok_or_else(|| Error::plugin(format!("plugin \"{name}\" has no commands")))?,
            };
            invoke(ctx, &m, name, &command, args)
        }
    }
}

// ----------------------------------------------------------------- list --

#[derive(Debug, Serialize)]
struct ListRow<'a> {
    name: &'a str,
    version: String,
    origin: Origin,
    state: &'a str,
    description: &'a str,
    commands: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<&'a Path>,
    problems: &'a [Problem],
}

fn list(ctx: &Ctx) -> Result<ExitCode> {
    let m = manager(ctx)?;
    let rows: Vec<_> = m
        .entries()
        .iter()
        .map(|e| ListRow {
            name: e.name(),
            version: e.manifest.plugin.version.to_string(),
            origin: e.origin,
            state: e.state(),
            description: &e.manifest.plugin.description,
            commands: e
                .manifest
                .commands
                .iter()
                .map(|c| c.name.as_str())
                .collect(),
            dir: e.dir.as_deref(),
            problems: &e.problems,
        })
        .collect();
    ctx.out.json(&serde_json::json!({
        "plugins": rows,
        "broken": m.broken(),
        "missing_enabled": m.missing_enabled(),
    }));
    let out = &ctx.out;
    out.line(out.paint(
        Style::Dim,
        format!(
            "{:<16} {:<9} {:<10} {:<10} DESCRIPTION",
            "NAME", "VERSION", "ORIGIN", "STATE"
        ),
    ));
    for e in m.entries() {
        let state = format!("{:<10}", e.state());
        let style = match e.state() {
            "enabled" => Style::Good,
            "disabled" => Style::Dim,
            _ => Style::Warn,
        };
        out.line(format!(
            "{:<16} {:<9} {:<10} {} {}",
            e.name(),
            e.manifest.plugin.version.to_string(),
            e.origin.as_str(),
            out.paint(style, state),
            e.manifest.plugin.description
        ));
    }
    for b in m.broken() {
        out.warn(format!(
            "unreadable plugin in {}: {}",
            b.dir.display(),
            b.error
        ));
    }
    for name in m.missing_enabled() {
        out.warn(format!(
            "plugins.enabled lists \"{name}\", but no such plugin exists"
        ));
    }
    Ok(ExitCode::SUCCESS)
}

// ----------------------------------------------------------------- info --

fn info(ctx: &Ctx, name: &str) -> Result<ExitCode> {
    let m = manager(ctx)?;
    let e = m.require(name)?;
    let settings = m.resolved_settings(e);
    let diagnostics = if e.enabled && !e.has_errors() {
        Some(m.diagnose(name).map_err(|e| e.to_report()))
    } else {
        None
    };
    let ungranted = ungranted_permissions(&m, e);
    let redacted = |key: &str, v: &Value| {
        if e.manifest.setting(key).is_some_and(|s| s.secret) {
            Value::String("<redacted>".into())
        } else {
            v.clone()
        }
    };
    ctx.out.json(&serde_json::json!({
        "plugin": e,
        "state": e.state(),
        "settings": settings.iter().map(|(k, v)| (k.clone(), redacted(k, v))).collect::<serde_json::Map<_, _>>(),
        "ungranted_permissions": ungranted,
        "diagnostics": diagnostics,
    }));
    if ctx.out.is_json() {
        return Ok(ExitCode::SUCCESS);
    }
    let out = &ctx.out;
    let p = &e.manifest.plugin;
    out.line(format!(
        "{} {} — {}",
        out.paint(Style::Bold, p.name.as_str()),
        p.version,
        p.description
    ));
    out.field("State", format!("{} ({})", e.state(), e.trust.as_str()));
    out.field("Origin", e.origin.as_str());
    if let Some(dir) = &e.dir {
        out.field("Directory", dir.display().to_string());
    }
    if let Some(exe) = &e.executable {
        out.field("Executable", exe.display().to_string());
    }
    if !p.authors.is_empty() {
        out.field("Authors", p.authors.join(", "));
    }
    if let Some(l) = &p.license {
        out.field("License", l);
    }
    if let Some(h) = &p.homepage {
        out.field("Homepage", h);
    }
    out.field(
        "Protocol",
        format!(
            "{} (caffeine speaks {})",
            p.api,
            caffeine_plugin_api::PROTOCOL_VERSION
        ),
    );
    if let Some(req) = &p.caffeine {
        out.field("Requires", format!("caffeine {req}"));
    }
    if !p.platforms.is_empty() {
        out.field("Platforms", p.platforms.join(", "));
    }

    out.line(format!("\n{}", out.paint(Style::Bold, "Permissions")));
    if p.permissions.is_empty() {
        out.line("  none");
    }
    for perm in &p.permissions {
        let enforcement = if perm.is_enforced() {
            "enforced"
        } else {
            "declared"
        };
        let flag = if ungranted.contains(perm) {
            " (not granted)"
        } else {
            ""
        };
        out.line(format!(
            "  {:<14} {}{} {}",
            perm.as_str(),
            perm.description(),
            flag,
            out.paint(Style::Dim, format!("[{enforcement}]"))
        ));
    }

    out.line(format!("\n{}", out.paint(Style::Bold, "Commands")));
    for c in &e.manifest.commands {
        let awake = if c.keep_awake {
            "  [keeps the system awake]"
        } else {
            ""
        };
        out.line(format!(
            "  caffeine {:<22} {}{}",
            c.usage.as_deref().unwrap_or(&c.name),
            c.summary,
            awake
        ));
    }

    if !e.manifest.settings.is_empty() {
        out.line(format!(
            "\n{} {}",
            out.paint(Style::Bold, "Settings"),
            out.paint(Style::Dim, format!("[plugins.settings.{name}]"))
        ));
        for s in &e.manifest.settings {
            let value = settings
                .get(&s.key)
                .map(|v| display_value(&redacted(&s.key, v)))
                .unwrap_or_else(|| "(unset)".into());
            out.line(format!(
                "  {:<16} {} {}",
                s.key,
                out.paint(Style::Accent, value),
                out.paint(Style::Dim, s.describe_type())
            ));
            out.line(format!("  {:<16} {}", "", s.description));
        }
    }

    if !e.problems.is_empty() {
        out.line(format!("\n{}", out.paint(Style::Bold, "Problems")));
        for prob in &e.problems {
            let style = if prob.is_error() {
                Style::Bad
            } else {
                Style::Warn
            };
            out.line(format!("  {}", out.paint(style, prob.to_string())));
        }
    }
    match &diagnostics {
        Some(Ok(checks)) if !checks.is_empty() => {
            out.line(format!("\n{}", out.paint(Style::Bold, "Health")));
            for c in checks {
                out.line(format!("  [{}] {}", c_status(out, c.status), c.summary));
                if let Some(h) = &c.hint {
                    out.line(format!(
                        "         {}",
                        out.paint(Style::Dim, format!("hint: {h}"))
                    ));
                }
            }
        }
        Some(Err(err)) => {
            out.line(format!("\n{}", out.paint(Style::Bold, "Health")));
            out.line(format!(
                "  {} {}",
                out.paint(Style::Bad, "diagnostics failed:"),
                err.message
            ));
        }
        _ => {}
    }
    if !e.enabled && !e.has_errors() {
        out.line(format!("\nEnable it with `caffeine plugin enable {name}`."));
    }
    Ok(ExitCode::SUCCESS)
}

fn c_status(out: &crate::output::Output, s: caffeine_plugin_api::CheckStatus) -> String {
    use caffeine_plugin_api::CheckStatus as S;
    match s {
        S::Ok => out.paint(Style::Good, " ok "),
        S::Info => out.paint(Style::Accent, "info"),
        S::Warn => out.paint(Style::Warn, "warn"),
        S::Fail => out.paint(Style::Bad, "FAIL"),
    }
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) if s.is_empty() => "\"\"".into(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Object(map) => format!("{{{}}}", map.keys().cloned().collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

// ------------------------------------------------------ enable / disable --

fn save_enabled(ctx: &mut Ctx, enabled: &[String]) -> Result<()> {
    if let Some(e) = &ctx.config_error {
        return Err(Error::config(format!(
            "cannot update plugins.enabled: {}",
            e.message()
        )));
    }
    ctx.config.set("plugins.enabled", &enabled.join(","))?;
    ctx.config.save()
}

fn enable(ctx: &mut Ctx, name: &str, yes: bool) -> Result<ExitCode> {
    let mut m = manager(ctx)?;
    let entry = m.require(name)?.clone();
    if entry.enabled && matches!(entry.trust, TrustStatus::BuiltIn | TrustStatus::Trusted) {
        ctx.out
            .json(&serde_json::json!({ "enabled": true, "changed": false, "plugin": name }));
        ctx.out.line(format!("Plugin {name} is already enabled."));
        return Ok(ExitCode::SUCCESS);
    }
    if entry.origin.is_third_party() && !entry.has_errors() {
        review(ctx, &entry);
        if !yes {
            match confirm(&format!(
                "Trust and enable {name} {}?",
                entry.manifest.plugin.version
            )) {
                Some(true) => {}
                Some(false) => return Err(Error::failure("cancelled; nothing was changed")),
                None => {
                    return Err(Error::usage(
                        "refusing to trust a third-party plugin without confirmation",
                    )
                    .with_hint("review it with `caffeine plugin info`, then re-run with --yes"));
                }
            }
        }
    }
    let enabled = m.enable(name)?;
    save_enabled(ctx, &enabled)?;
    ctx.out
        .json(&serde_json::json!({ "enabled": true, "changed": true, "plugin": name }));
    ctx.out
        .line(format!("{} {name}", ctx.out.paint(Style::Good, "Enabled")));
    let commands: Vec<_> = entry
        .manifest
        .commands
        .iter()
        .map(|c| format!("caffeine {}", c.name))
        .collect();
    ctx.out.line(format!("  commands: {}", commands.join(", ")));
    Ok(ExitCode::SUCCESS)
}

/// Show what the user is about to trust.
fn review(ctx: &Ctx, entry: &PluginEntry) {
    if ctx.out.is_json() {
        return;
    }
    let out = &ctx.out;
    let p = &entry.manifest.plugin;
    out.line(format!(
        "{} {} {} from {}",
        out.paint(Style::Bold, "Third-party plugin"),
        p.name,
        p.version,
        entry
            .dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
    ));
    out.line(
        "  It runs as a separate program with your user's privileges; Caffeine cannot sandbox it.",
    );
    if p.permissions.is_empty() {
        out.line("  Declared permissions: none");
    } else {
        out.line("  Declared permissions:");
        for perm in &p.permissions {
            out.line(format!("    - {}: {}", perm.as_str(), perm.description()));
        }
    }
    if !p.permissions.contains(&Permission::EnvInherit) {
        out.line("  It will receive a minimal environment (no tokens or credentials).");
    }
    if let Some(exe) = &entry.executable {
        out.line(format!("  Executable: {}", exe.display()));
    }
    out.line("  Trust is recorded for these exact files; any change requires re-enabling.");
}

fn disable(ctx: &mut Ctx, name: &str) -> Result<ExitCode> {
    let mut m = manager(ctx)?;
    let was_enabled = ctx
        .config
        .config()
        .plugins
        .enabled
        .iter()
        .any(|n| n == name);
    let enabled = m.disable(name)?;
    if was_enabled {
        save_enabled(ctx, &enabled)?;
    }
    ctx.out
        .json(&serde_json::json!({ "enabled": false, "changed": was_enabled, "plugin": name }));
    if was_enabled {
        ctx.out
            .line(format!("{} {name}", ctx.out.paint(Style::Good, "Disabled")));
    } else {
        ctx.out.line(format!("Plugin {name} was not enabled."));
    }
    Ok(ExitCode::SUCCESS)
}

// ------------------------------------------------------ install / remove --

fn install(ctx: &mut Ctx, path: &Path, upgrade: bool) -> Result<ExitCode> {
    let mut m = manager(ctx)?;
    let report = m.install(path, upgrade)?;
    ctx.out.json(&report);
    let verb = match &report.replaced {
        Some(old) => format!("Upgraded {} {old} →", report.name),
        None => format!("Installed {}", report.name),
    };
    ctx.out.line(format!(
        "{} {} into {}",
        ctx.out.paint(Style::Good, verb),
        report.version,
        report.dir.display()
    ));
    if report.needs_reenable {
        ctx.out.warn(format!(
            "{} changed, so it will not run until you re-enable it: `caffeine plugin enable {}`",
            report.name, report.name
        ));
    } else if m.get(&report.name).is_some_and(|e| !e.enabled) {
        ctx.out.line(format!(
            "  Nothing was run. Review it with `caffeine plugin info {0}`, then `caffeine plugin enable {0}`.",
            report.name
        ));
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(ctx: &mut Ctx, name: &str, yes: bool) -> Result<ExitCode> {
    let mut m = manager(ctx)?;
    let entry = m.require(name)?.clone();
    if !yes && entry.origin == Origin::Installed {
        match confirm(&format!(
            "Delete {} and its trust record?",
            entry
                .dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        )) {
            Some(true) => {}
            Some(false) => return Err(Error::failure("cancelled; nothing was changed")),
            None => {
                return Err(Error::usage("refusing to delete without confirmation")
                    .with_hint("re-run with --yes"));
            }
        }
    }
    let dir = m.remove(name)?;
    let enabled = ctx.config.config().plugins.enabled.clone();
    if enabled.iter().any(|n| n == name) {
        let rest: Vec<_> = enabled.into_iter().filter(|n| n != name).collect();
        save_enabled(ctx, &rest)?;
    }
    ctx.out
        .json(&serde_json::json!({ "removed": name, "dir": dir }));
    ctx.out.line(format!(
        "{} {name} ({})",
        ctx.out.paint(Style::Good, "Removed"),
        dir.display()
    ));
    Ok(ExitCode::SUCCESS)
}

// --------------------------------------------------- create / validate --

fn create(ctx: &Ctx, name: &str, path: Option<&Path>, api_path: Option<&Path>) -> Result<ExitCode> {
    let dir = path.map_or_else(
        || Path::new(&format!("caffeine-plugin-{name}")).to_path_buf(),
        Path::to_path_buf,
    );
    let api = match api_path {
        Some(p) => {
            let p = p
                .canonicalize()
                .map_err(|e| Error::io(format!("resolving {}", p.display()), e))?;
            if !p.join("Cargo.toml").is_file() {
                return Err(Error::usage(format!(
                    "{} is not a crate directory",
                    p.display()
                )));
            }
            ApiSource::Path(p)
        }
        None => ApiSource::Git(env!("CARGO_PKG_REPOSITORY").to_string()),
    };
    let files = scaffold::create(&dir, name, &api)?;
    ctx.out
        .json(&serde_json::json!({ "created": dir, "files": files }));
    ctx.out.line(format!(
        "{} plugin project {}",
        ctx.out.paint(Style::Good, "Created"),
        dir.display()
    ));
    for f in &files {
        ctx.out.line(format!("  {}", f.display()));
    }
    ctx.out.line(format!(
        "\nNext:\n  cd {}\n  cargo build --release\n  caffeine plugin validate .\n  caffeine plugin install .\n  caffeine plugin enable {name}",
        dir.display()
    ));
    Ok(ExitCode::SUCCESS)
}

fn validate(ctx: &Ctx, path: &Path) -> Result<ExitCode> {
    let m = manager(ctx)?;
    let report = m.validate(path);
    let valid = report.is_valid();
    ctx.out
        .json(&serde_json::json!({ "valid": valid, "report": report }));
    let out = &ctx.out;
    match &report.manifest {
        Some(manifest) => out.line(format!(
            "{} {} {} ({})",
            if valid {
                out.paint(Style::Good, "valid:")
            } else {
                out.paint(Style::Bad, "invalid:")
            },
            manifest.name(),
            manifest.plugin.version,
            report.dir.display()
        )),
        None => out.line(format!(
            "{} {}",
            out.paint(Style::Bad, "invalid:"),
            report.dir.display()
        )),
    }
    for p in &report.problems {
        let style = if p.is_error() {
            Style::Bad
        } else {
            Style::Warn
        };
        out.line(format!("  {}", out.paint(style, p.to_string())));
    }
    if let Some(exe) = &report.executable {
        out.line(format!("  executable: {}", exe.display()));
    }
    Ok(if valid {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(caffeine_core::ErrorKind::Plugin.exit_code() as u8)
    })
}

// ----------------------------------------------------------- invocation --

/// `caffeine <command> ...` for a plugin-provided command.
pub fn external(ctx: &Ctx, argv: &[String]) -> Result<ExitCode> {
    let (command, args) = argv.split_first().expect("clap provides the command name");
    let m = manager(ctx)?;
    let Some(entry) = m.command_provider(command) else {
        if let Some(e) = m
            .entries()
            .iter()
            .find(|e| e.manifest.command(command).is_some())
        {
            return Err(Error::plugin(format!(
                "`{command}` is provided by plugin \"{}\", which is {}",
                e.name(),
                e.state()
            ))
            .with_hint(format!("see `caffeine plugin info {}`", e.name())));
        }
        return Err(Error::usage(format!("unrecognized command '{command}'"))
            .with_hint("run `caffeine --help`, or `caffeine plugin list` for plugin commands"));
    };
    let name = entry.name().to_string();
    invoke(ctx, &m, &name, command, args)
}

/// Run a plugin command, inside an awake session when it is declared
/// `keep_awake`.
fn invoke(ctx: &Ctx, m: &Manager, name: &str, command: &str, args: &[String]) -> Result<ExitCode> {
    let entry = m.require(name)?;
    let spec = entry.manifest.command(command).cloned();
    let request = RunRequest {
        command: command.to_string(),
        args: args.to_vec(),
        cwd: std::env::current_dir().map_err(|e| Error::io("reading the current directory", e))?,
        json: ctx.out.is_json(),
    };
    let json = ctx.out.is_json();
    let quiet = ctx.global.quiet;
    let work = || -> Result<(i32, Vec<Value>)> {
        let mut host = PluginOutput {
            json,
            quiet,
            data: Vec::new(),
        };
        let code = m.run(name, &request, &mut host)?;
        Ok((code, host.data))
    };
    let keep_awake = spec.as_ref().is_some_and(|s| s.keep_awake);
    let ((code, data), session) = if keep_awake {
        ctx.warn_config_problems();
        let label = format!("caffeine {command}");
        // Validate before acquiring anything, so a plugin that cannot run
        // never holds an assertion.
        m.prepare(name)?;
        crate::commands::session::keep_awake_while(ctx, &label, work)?
    } else {
        (work()?, None)
    };
    if json {
        let data = match data.len() {
            0 => Value::Null,
            1 => data.into_iter().next().expect("one value"),
            _ => Value::Array(data),
        };
        ctx.out.json(&serde_json::json!({
            "plugin": name,
            "command": command,
            "exit_code": code,
            "data": data,
            "session": session,
        }));
    }
    Ok(ExitCode::from((code & 0xff) as u8))
}

/// Forwards plugin output to the terminal. Built per invocation on the
/// thread that runs the plugin.
struct PluginOutput {
    json: bool,
    quiet: bool,
    data: Vec<Value>,
}

impl Host for PluginOutput {
    fn log(&mut self, level: LogLevel, message: &str) {
        let level = match level {
            LogLevel::Error => log::Level::Error,
            LogLevel::Warn => log::Level::Warn,
            LogLevel::Info => log::Level::Info,
            LogLevel::Debug => log::Level::Debug,
            LogLevel::Trace => log::Level::Trace,
        };
        log::log!(target: "caffeine::plugin", level, "{message}");
    }

    fn print(&mut self, text: &str) {
        if !self.json && !self.quiet {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{text}");
            let _ = out.flush();
        }
    }

    fn data(&mut self, value: Value) {
        self.data.push(value);
    }
}

// --------------------------------------------------------------- doctor --

/// Plugin checks for `caffeine doctor`.
pub fn doctor_checks(ctx: &Ctx) -> Vec<DiagnosticCheck> {
    let m = match manager(ctx) {
        Ok(m) => m,
        Err(e) => {
            return vec![DiagnosticCheck::new(
                "plugins.discover",
                CheckStatus::Fail,
                e.to_string(),
            )];
        }
    };
    let mut checks = Vec::new();
    for b in m.broken() {
        checks.push(DiagnosticCheck::new(
            "plugins.manifest",
            CheckStatus::Warn,
            format!("unreadable plugin in {}: {}", b.dir.display(), b.error),
        ));
    }
    for name in m.missing_enabled() {
        checks.push(
            DiagnosticCheck::new(
                "plugins.missing",
                CheckStatus::Warn,
                format!("plugins.enabled lists \"{name}\", but no such plugin exists"),
            )
            .with_hint(format!("`caffeine plugin disable {name}` removes it")),
        );
    }
    for e in m.entries().iter().filter(|e| e.enabled) {
        let id = format!("plugins.{}", e.name());
        if e.has_errors() || !matches!(e.trust, TrustStatus::BuiltIn | TrustStatus::Trusted) {
            let why = e
                .problems
                .iter()
                .find(|p| p.is_error())
                .map(|p| p.message.clone())
                .unwrap_or_else(|| e.trust.as_str().to_string());
            checks.push(
                DiagnosticCheck::new(
                    &id,
                    CheckStatus::Fail,
                    format!("plugin {} cannot run: {why}", e.name()),
                )
                .with_hint(format!("see `caffeine plugin info {}`", e.name())),
            );
            continue;
        }
        match m.diagnose(e.name()) {
            Ok(list) if list.is_empty() => checks.push(DiagnosticCheck::new(
                &id,
                CheckStatus::Ok,
                format!("plugin {} is ready", e.name()),
            )),
            Ok(list) => {
                for c in list {
                    let status = match c.status {
                        caffeine_plugin_api::CheckStatus::Ok => CheckStatus::Ok,
                        caffeine_plugin_api::CheckStatus::Info => CheckStatus::Info,
                        caffeine_plugin_api::CheckStatus::Warn => CheckStatus::Warn,
                        caffeine_plugin_api::CheckStatus::Fail => CheckStatus::Fail,
                    };
                    let mut check = DiagnosticCheck::new(
                        format!("plugins.{}.{}", e.name(), c.id),
                        status,
                        format!("{}: {}", e.name(), c.summary),
                    );
                    if let Some(h) = c.hint {
                        check = check.with_hint(h);
                    }
                    checks.push(check);
                }
            }
            Err(err) => checks.push(DiagnosticCheck::new(
                &id,
                CheckStatus::Fail,
                err.to_string(),
            )),
        }
    }
    checks
}

/// Dotted paths of secret plugin settings, for `config show`.
pub fn secret_paths(ctx: &Ctx) -> Vec<String> {
    manager(ctx).map(|m| m.secret_paths()).unwrap_or_default()
}
