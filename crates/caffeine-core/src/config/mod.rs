//! Configuration loading, migration, validation, editing and safe saving.
//!
//! The file is parsed twice: once into a format-preserving
//! [`toml_edit::DocumentMut`] (so edits keep the user's comments, ordering
//! and unknown keys) and once into the typed [`Config`]. Every edit is
//! applied to the document and re-validated by deserializing the result; a
//! rejected edit leaves the document untouched.
//!
//! Saving never silently overwrites: if the file changed on disk since it
//! was loaded, [`ConfigFile::save`] fails with a conflict. Writes are atomic
//! and keep a `.bak` copy of the previous version.

pub mod schema;
pub mod settings;

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

pub use schema::*;
pub use settings::{Section, SettingDef, SettingKind};

use crate::duration::SessionLength;
use crate::error::{Error, Result};
use crate::fsutil;

/// A configuration file and its parsed, effective contents.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
    config: Config,
    document: DocumentMut,
    /// File contents at load time; `None` if the file did not exist.
    original: Option<String>,
    /// Migration notes and unknown-key warnings gathered while loading.
    notes: Vec<String>,
    /// Set when saving must be refused (e.g. written by a newer version).
    read_only: Option<String>,
    dirty: bool,
}

impl ConfigFile {
    /// Load `path`. A missing file yields the defaults; a malformed or
    /// invalid file is an error.
    pub fn load(path: &Path) -> Result<ConfigFile> {
        match fsutil::read_optional(path)? {
            None => Ok(ConfigFile::defaults(path)),
            Some(text) => ConfigFile::from_text(path, &text),
        }
    }

    /// Like [`ConfigFile::load`], but falls back to defaults when the file is
    /// unusable, returning the error so callers can warn. The fallback is
    /// marked read-only so the broken file is never overwritten implicitly.
    pub fn load_or_default(path: &Path) -> (ConfigFile, Option<Error>) {
        match ConfigFile::load(path) {
            Ok(cfg) => (cfg, None),
            Err(err) => {
                let mut cfg = ConfigFile::defaults(path);
                cfg.original = fsutil::read_optional(path).ok().flatten();
                cfg.read_only = Some(format!(
                    "{} could not be loaded; using defaults. Fix it with `caffeine config edit` \
                     or reset it with `caffeine config reset`",
                    path.display()
                ));
                (cfg, Some(err))
            }
        }
    }

    /// The default configuration, not yet written to `path`.
    pub fn defaults(path: &Path) -> ConfigFile {
        let document: DocumentMut = template()
            .parse()
            .expect("built-in configuration template must parse");
        ConfigFile {
            path: path.to_path_buf(),
            config: Config::default(),
            document,
            original: None,
            notes: Vec::new(),
            read_only: None,
            dirty: false,
        }
    }

    /// Parse configuration text.
    pub fn from_text(path: &Path, text: &str) -> Result<ConfigFile> {
        let mut document: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
            Error::config(format!(
                "{} is not valid TOML: {}",
                path.display(),
                e.to_string().trim()
            ))
        })?;
        let mut notes = Vec::new();
        let mut read_only = None;

        let version = match document.get("schema_version") {
            None => 0,
            Some(item) => item
                .as_integer()
                .filter(|v| *v >= 0 && *v <= i64::from(u32::MAX))
                .ok_or_else(|| {
                    Error::config(format!(
                        "{}: schema_version must be a non-negative integer",
                        path.display()
                    ))
                })? as u32,
        };
        if version > SCHEMA_VERSION {
            read_only = Some(format!(
                "{} uses schema version {version}, written by a newer Caffeine; this build \
                 (schema {SCHEMA_VERSION}) will read it but not modify it",
                path.display()
            ));
        } else {
            notes.extend(migrate(&mut document, version));
        }

        let config = deserialize(&document.to_string(), path)?;
        let mut problems = config.validate();
        if read_only.is_some() {
            problems.retain(|p| !p.starts_with("schema_version"));
        }
        if !problems.is_empty() {
            return Err(Error::config(format!(
                "{} is invalid:\n  - {}",
                path.display(),
                problems.join("\n  - ")
            ))
            .with_hint("run `caffeine config edit` to fix it, or `caffeine config reset`"));
        }
        for key in unknown_keys(&document) {
            notes.push(format!("unknown setting \"{key}\" (ignored)"));
        }
        let migrated = version < SCHEMA_VERSION && read_only.is_none();
        Ok(ConfigFile {
            path: path.to_path_buf(),
            config,
            document,
            original: Some(text.to_string()),
            notes,
            read_only,
            dirty: migrated,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Whether the file existed when loaded.
    pub fn exists_on_disk(&self) -> bool {
        self.original.is_some()
    }

    /// Whether there are unsaved changes (including pending migrations).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The document as it would be written.
    pub fn to_text(&self) -> String {
        self.document.to_string()
    }

    /// The effective value of a built-in setting, formatted for display.
    pub fn get(&self, key: &str) -> Result<String> {
        let json = serde_json::to_value(&self.config)
            .map_err(|e| Error::failure(format!("serializing config: {e}")))?;
        let mut node = &json;
        for part in key.split('.') {
            node = node
                .get(part)
                .ok_or_else(|| Error::not_found(format!("unknown setting \"{key}\"")))?;
        }
        Ok(display_json(node))
    }

    /// Set a built-in setting from its textual representation.
    pub fn set(&mut self, key: &str, raw: &str) -> Result<()> {
        let def = settings::find(key).ok_or_else(|| {
            Error::not_found(format!("unknown setting \"{key}\""))
                .with_hint("run `caffeine config show` to list settings")
        })?;
        let item = parse_setting_value(&def, raw)?;
        self.apply(key, item)
    }

    /// Reset a built-in setting to its default by removing it from the file.
    pub fn unset(&mut self, key: &str) -> Result<()> {
        settings::find(key)
            .ok_or_else(|| Error::not_found(format!("unknown setting \"{key}\"")))?;
        self.apply(key, Item::None)
    }

    /// Replace the settings table of one plugin. Values are validated by the
    /// plugin manager before calling this.
    pub fn set_plugin_settings(&mut self, plugin: &str, table: Table) -> Result<()> {
        if !is_valid_plugin_name(plugin) {
            return Err(Error::usage(format!(
                "\"{plugin}\" is not a valid plugin name"
            )));
        }
        let item = if table.is_empty() {
            Item::None
        } else {
            Item::Table(table)
        };
        self.apply(&format!("plugins.settings.{plugin}"), item)
    }

    /// Write `item` at dotted `key` (removing it for `Item::None`), then
    /// re-validate. On failure the document is left unchanged.
    fn apply(&mut self, key: &str, item: Item) -> Result<()> {
        self.ensure_writable()?;
        let mut doc = self.document.clone();
        set_path(&mut doc, key, item)?;
        let config = deserialize(&doc.to_string(), &self.path)?;
        let problems = config.validate();
        if !problems.is_empty() {
            return Err(Error::config(problems.join("; ")));
        }
        self.document = doc;
        self.config = config;
        self.dirty = true;
        Ok(())
    }

    /// Replace the whole document with `text` (used after external editing).
    pub fn replace_text(&mut self, text: &str) -> Result<()> {
        self.ensure_writable()?;
        let parsed = ConfigFile::from_text(&self.path, text)?;
        self.document = parsed.document;
        self.config = parsed.config;
        self.notes = parsed.notes;
        self.dirty = true;
        Ok(())
    }

    fn ensure_writable(&self) -> Result<()> {
        match &self.read_only {
            Some(reason) => Err(Error::config(format!(
                "refusing to modify configuration: {reason}"
            ))),
            None => Ok(()),
        }
    }

    /// Whether the file on disk still matches what was loaded.
    pub fn changed_on_disk(&self) -> Result<bool> {
        Ok(fsutil::read_optional(&self.path)? != self.original)
    }

    /// Atomically write the document, keeping a `.bak` of the previous file.
    /// Fails with a conflict if the file changed on disk since loading.
    pub fn save(&mut self) -> Result<()> {
        self.ensure_writable()?;
        if self.changed_on_disk()? {
            return Err(Error::conflict(format!(
                "{} was modified by another program since it was loaded",
                self.path.display()
            ))
            .with_hint("reload and re-apply your changes; nothing was written"));
        }
        let text = self.to_text();
        fsutil::write_atomic(&self.path, text.as_bytes(), true)?;
        self.original = Some(text);
        self.dirty = false;
        Ok(())
    }

    /// Write the default configuration to `path`, moving any existing file
    /// (valid or not) to a timestamped backup first. Returns the backup path.
    pub fn reset(path: &Path) -> Result<Option<PathBuf>> {
        let backup = if path.exists() {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(format!(".reset-{}.bak", crate::time::unix_now()));
            let backup = path.with_file_name(name);
            std::fs::copy(path, &backup)
                .map_err(|e| Error::io(format!("backing up {}", path.display()), e))?;
            Some(backup)
        } else {
            None
        };
        fsutil::write_atomic(path, template().as_bytes(), false)?;
        Ok(backup)
    }
}

fn deserialize(text: &str, path: &Path) -> Result<Config> {
    toml_edit::de::from_str::<Config>(text).map_err(|e| {
        Error::config(format!("{}: {}", path.display(), e.to_string().trim()))
            .with_hint("run `caffeine config validate` for details")
    })
}

/// Apply schema migrations in place, returning a note for each step.
///
/// * v0 → v1: files written before versioning gain `schema_version = 1`.
fn migrate(doc: &mut DocumentMut, from: u32) -> Vec<String> {
    let mut notes = Vec::new();
    let mut version = from;
    while version < SCHEMA_VERSION {
        if version == 0 {
            notes.push("configuration has no schema_version; treating it as version 1".to_string());
        }
        version += 1;
    }
    if from < SCHEMA_VERSION {
        let mut decorated = toml_edit::value(i64::from(SCHEMA_VERSION));
        if let Some(v) = decorated.as_value_mut() {
            v.decor_mut()
                .set_suffix("  # added by caffeine (schema migration)");
        }
        doc.insert("schema_version", decorated);
    }
    notes
}

/// Insert (or remove, for `Item::None`) an item at a dotted path, creating
/// intermediate standard tables as needed.
fn set_path(doc: &mut DocumentMut, key: &str, item: Item) -> Result<()> {
    let parts: Vec<&str> = key.split('.').collect();
    let (last, parents) = parts.split_last().expect("split yields at least one part");
    let mut table: &mut Table = doc.as_table_mut();
    for part in parents {
        if !table.contains_key(part) {
            if item.is_none() {
                return Ok(());
            }
            let mut t = Table::new();
            t.set_implicit(true);
            table.insert(part, Item::Table(t));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| {
                Error::config(format!(
                    "cannot set \"{key}\": \"{part}\" is not a table in the file"
                ))
            })?;
    }
    if item.is_none() {
        table.remove(last);
    } else if let (Some(existing), Item::Value(new)) = (table.get_mut(last), &item) {
        // Keep the existing comment/whitespace decoration around the value.
        if let Some(old) = existing.as_value() {
            let decor = old.decor().clone();
            let mut new = new.clone();
            *new.decor_mut() = decor;
            *existing = Item::Value(new);
        } else {
            *existing = item;
        }
    } else {
        table.insert(last, item);
    }
    Ok(())
}

/// Convert user input to a TOML item according to the setting's kind.
pub fn parse_setting_value(def: &SettingDef, raw: &str) -> Result<Item> {
    let raw = raw.trim();
    let bad = |why: String| Error::usage(format!("invalid value for {}: {why}", def.key));
    let list = |raw: &str| -> Vec<String> {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    Ok(match def.kind {
        SettingKind::Bool => toml_edit::value(
            parse_bool(raw)
                .ok_or_else(|| bad(format!("\"{raw}\" is not a boolean (use true or false)")))?,
        ),
        SettingKind::Choice(choices) => {
            if !choices.contains(&raw) {
                return Err(bad(format!(
                    "\"{raw}\" (expected one of: {})",
                    choices.join(", ")
                )));
            }
            toml_edit::value(raw)
        }
        SettingKind::Length => {
            let len: SessionLength = raw
                .parse()
                .map_err(|e: Error| bad(e.message().to_string()))?;
            toml_edit::value(len.to_string())
        }
        SettingKind::Integer { min, max } => {
            let n: i64 = raw
                .parse()
                .map_err(|_| bad(format!("\"{raw}\" is not an integer")))?;
            if n < min || n > max {
                return Err(bad(format!("{n} is outside {min}..={max}")));
            }
            toml_edit::value(n)
        }
        SettingKind::OptionalPath => {
            if raw.is_empty() {
                Item::None
            } else if !Path::new(raw).is_absolute() {
                return Err(bad(format!("\"{raw}\" is not an absolute path")));
            } else {
                toml_edit::value(raw)
            }
        }
        SettingKind::PathList => {
            let items = list(raw);
            if let Some(rel) = items.iter().find(|p| !Path::new(p).is_absolute()) {
                return Err(bad(format!("\"{rel}\" is not an absolute path")));
            }
            Item::Value(Value::Array(items.into_iter().collect()))
        }
        SettingKind::StringList(allowed) => {
            let items = list(raw);
            if let Some(allowed) = allowed
                && let Some(unknown) = items.iter().find(|i| !allowed.contains(&i.as_str()))
            {
                return Err(bad(format!(
                    "\"{unknown}\" (expected values from: {})",
                    allowed.join(", ")
                )));
            }
            Item::Value(Value::Array(items.into_iter().collect()))
        }
    })
}

pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn display_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(display_json)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// Dotted paths present in the document that this build does not know.
pub fn unknown_keys(doc: &DocumentMut) -> Vec<String> {
    fn walk(prefix: &str, table: &dyn toml_edit::TableLike, out: &mut Vec<String>) {
        for (key, item) in table.iter() {
            let path = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            if !settings::is_known_path(&path) {
                out.push(path);
                continue;
            }
            if path == "plugins.settings" {
                continue;
            }
            if let Some(t) = item.as_table_like() {
                walk(&path, t, out);
            }
        }
    }
    let mut out = Vec::new();
    walk("", doc.as_table(), &mut out);
    out
}

/// Key fragments treated as secrets when printing plugin settings.
const SECRET_HINTS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "credential",
    "private_key",
];

/// The effective configuration as JSON with secrets redacted.
///
/// Plugin settings whose key looks secret, or whose dotted path is listed in
/// `secret_paths` (from plugin schemas), are replaced by `"<redacted>"`.
pub fn effective_json(config: &Config, secret_paths: &[String]) -> serde_json::Value {
    fn redact(prefix: &str, v: &mut serde_json::Value, secret_paths: &[String]) {
        if let serde_json::Value::Object(map) = v {
            for (k, child) in map.iter_mut() {
                let path = format!("{prefix}.{k}");
                let lower = k.to_ascii_lowercase();
                if secret_paths.contains(&path) || SECRET_HINTS.iter().any(|h| lower.contains(h)) {
                    *child = serde_json::Value::String("<redacted>".into());
                } else {
                    redact(&path, child, secret_paths);
                }
            }
        }
    }
    let mut json = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
    if let Some(settings) = json.pointer_mut("/plugins/settings") {
        redact("plugins.settings", settings, secret_paths);
    }
    json
}

/// The commented default configuration file.
pub fn template() -> String {
    let defaults = serde_json::to_value(Config::default()).expect("defaults serialize");
    let mut out = String::from(
        "# Caffeine configuration.\n\
         #\n\
         # Edit with `caffeine --config` (interactive) or `caffeine config edit`.\n\
         # Every value below is the default; delete a line to fall back to it.\n\
         # `caffeine config validate` checks this file.\n\n",
    );
    out.push_str(&format!("schema_version = {SCHEMA_VERSION}\n"));
    let mut current_table = "";
    for def in settings::all() {
        let (table, name) = def.key.split_once('.').expect("settings are namespaced");
        if table != current_table {
            out.push_str(&format!("\n[{table}]\n"));
            current_table = table;
        } else {
            out.push('\n');
        }
        out.push_str(&format!("# {}: {}\n", def.label, wrap_comment(def.help)));
        out.push_str(&format!("# Accepts: {}\n", def.kind.describe()));
        let value = defaults.pointer(&format!("/{}", def.key.replace('.', "/")));
        match value {
            None | Some(serde_json::Value::Null) => out.push_str(&format!("# {name} = \"\"\n")),
            Some(v) => out.push_str(&format!("{name} = {}\n", toml_literal(v))),
        }
    }
    out.push_str(
        "\n# Plugin-specific settings live in [plugins.settings.<plugin-name>] tables.\n\
         # See `caffeine plugin info <name>` for each plugin's options.\n",
    );
    out
}

fn wrap_comment(text: &str) -> String {
    let mut out = String::new();
    let mut line_len = 0;
    for word in text.split_whitespace() {
        if line_len > 0 && line_len + word.len() > 76 {
            out.push_str("\n#   ");
            line_len = 0;
        } else if line_len > 0 {
            out.push(' ');
        }
        out.push_str(word);
        line_len += word.len() + 1;
    }
    out
}

fn toml_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => Value::from(s.as_str()).to_string().trim().to_string(),
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(toml_literal).collect();
            format!("[{}]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
