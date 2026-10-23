//! Plugin discovery, validation, trust and lifecycle.
//!
//! Plugins come from three places, in order of precedence:
//!
//! 1. **Built-in** plugins compiled into `caffeine`. Trusted implicitly and
//!    called in-process.
//! 2. **Installed** plugins in `<data>/plugins/<name>/`, placed there by
//!    `caffeine plugin install`.
//! 3. **External** plugins found in `plugins.search_paths`, used in place
//!    (handy while developing a plugin).
//!
//! Discovery only reads manifests; it never executes plugin code. A plugin
//! runs only when it is enabled, compatible, free of manifest errors and,
//! for third-party plugins, unchanged since the user trusted it.

mod env;
mod install;
mod runner;

mod trust;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use caffeine_core::config::Config;
use caffeine_core::{Error, Paths, Result};
use caffeine_plugin_api::manifest::Severity;
use caffeine_plugin_api::{MANIFEST_FILE, Manifest, Permission, Plugin, Problem};

pub use env::ALLOWED as ALLOWED_ENV;
pub use install::{InstallReport, ValidationReport};
pub use runner::RunRequest;
pub use trust::{TrustRecord, TrustStore};

/// A plugin compiled into the `caffeine` binary.
#[derive(Clone, Copy)]
pub struct Builtin {
    pub factory: fn() -> Box<dyn Plugin>,
}

impl std::fmt::Debug for Builtin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Builtin")
    }
}

/// Where a plugin was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    BuiltIn,
    Installed,
    External,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::BuiltIn => "built-in",
            Origin::Installed => "installed",
            Origin::External => "external",
        }
    }

    pub fn is_third_party(self) -> bool {
        self != Origin::BuiltIn
    }
}

/// Whether the user trusts a plugin's current files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustStatus {
    /// Built-in plugins ship with Caffeine.
    BuiltIn,
    /// Enabled by the user and unchanged since.
    Trusted,
    /// Never enabled (or disabled since).
    NotTrusted,
    /// Enabled once, but the manifest or executable changed since.
    Changed,
}

impl TrustStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustStatus::BuiltIn => "built-in",
            TrustStatus::Trusted => "trusted",
            TrustStatus::NotTrusted => "not trusted",
            TrustStatus::Changed => "changed since trusted",
        }
    }
}

/// A discovered plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    pub manifest: Manifest,
    pub origin: Origin,
    /// Directory containing the manifest (third-party plugins).
    pub dir: Option<PathBuf>,
    /// Absolute path of the executable for this platform.
    pub executable: Option<PathBuf>,
    pub enabled: bool,
    pub trust: TrustStatus,
    pub problems: Vec<Problem>,
}

impl PluginEntry {
    pub fn name(&self) -> &str {
        self.manifest.name()
    }

    /// Whether manifest or compatibility errors prevent running it.
    pub fn has_errors(&self) -> bool {
        self.problems.iter().any(Problem::is_error)
    }

    /// Short state for listings.
    pub fn state(&self) -> &'static str {
        match (self.enabled, self.has_errors(), self.trust) {
            (_, true, _) => "unusable",
            (true, false, TrustStatus::Changed) => "changed",
            (true, false, TrustStatus::NotTrusted) => "untrusted",
            (true, false, _) => "enabled",
            (false, false, _) => "disabled",
        }
    }

    pub fn manifest_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(MANIFEST_FILE))
    }
}

/// A directory with a manifest that could not be parsed.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenPlugin {
    pub dir: PathBuf,
    pub error: String,
}

/// Everything the manager needs to know about the host.
#[derive(Debug, Clone)]
pub struct ManagerOptions {
    /// `<data>/plugins`: installed plugins and the trust store.
    pub plugin_dir: PathBuf,
    /// Root of per-plugin data directories.
    pub data_dir: PathBuf,
    pub search_paths: Vec<PathBuf>,
    pub enabled: Vec<String>,
    pub settings: BTreeMap<String, Value>,
    pub host_version: semver::Version,
    /// Top-level CLI commands a plugin command must not shadow.
    pub reserved_commands: Vec<String>,
    pub builtins: Vec<Builtin>,
}

impl ManagerOptions {
    pub fn new(paths: &Paths, config: &Config, host_version: semver::Version) -> ManagerOptions {
        ManagerOptions {
            plugin_dir: paths.plugin_dir.clone(),
            data_dir: paths.data_dir.join("plugin-data"),
            search_paths: config.plugins.search_paths.clone(),
            enabled: config.plugins.enabled.clone(),
            settings: config.plugins.settings.clone(),
            host_version,
            reserved_commands: Vec::new(),
            builtins: Vec::new(),
        }
    }
}

/// Name of the trust store inside the plugin directory.
pub const TRUST_FILE: &str = "trust.json";

#[derive(Debug)]
pub struct Manager {
    options: ManagerOptions,
    trust: TrustStore,
    entries: Vec<PluginEntry>,
    broken: Vec<BrokenPlugin>,
    builtin_factories: BTreeMap<String, Builtin>,
}

impl Manager {
    /// Discover every plugin. Reads manifests only.
    pub fn discover(options: ManagerOptions) -> Result<Manager> {
        let trust = TrustStore::load(&options.plugin_dir.join(TRUST_FILE))?;
        let mut manager = Manager {
            options,
            trust,
            entries: Vec::new(),
            broken: Vec::new(),
            builtin_factories: BTreeMap::new(),
        };
        manager.scan();
        Ok(manager)
    }

    /// Re-read everything from disk (after install, enable, ...).
    pub fn rescan(&mut self) {
        self.scan();
    }

    fn scan(&mut self) {
        self.entries.clear();
        self.broken.clear();
        self.builtin_factories.clear();
        for builtin in self.options.builtins.clone() {
            let manifest = (builtin.factory)().manifest();
            self.builtin_factories
                .insert(manifest.name().to_string(), builtin);
            self.push(manifest, Origin::BuiltIn, None);
        }
        let mut dirs = vec![(Origin::Installed, plugin_subdirs(&self.options.plugin_dir))];
        for root in self.options.search_paths.clone() {
            let mut found = Vec::new();
            if root.join(MANIFEST_FILE).is_file() {
                found.push(root.clone());
            }
            found.extend(plugin_subdirs(&root));
            dirs.push((Origin::External, found));
        }
        for (origin, list) in dirs {
            for dir in list {
                match read_manifest(&dir) {
                    Ok(manifest) => self.push(manifest, origin, Some(dir)),
                    Err(error) => self.broken.push(BrokenPlugin { dir, error }),
                }
            }
        }
    }

    fn push(&mut self, manifest: Manifest, origin: Origin, dir: Option<PathBuf>) {
        let name = manifest.name().to_string();
        let mut problems = manifest.validate();
        problems.extend(compatibility_problems(
            &manifest,
            &self.options.host_version,
        ));

        let mut executable = None;
        if let Some(dir) = &dir {
            let (exe, exe_problems) = resolve_executable(&manifest, dir);
            executable = exe;
            problems.extend(exe_problems);
            if origin == Origin::Installed
                && dir.file_name().is_some_and(|n| n.to_string_lossy() != name)
            {
                problems.push(Problem::warning(format!(
                    "installed in directory {} but named \"{name}\"",
                    dir.display()
                )));
            }
        }

        if let Some(earlier) = self.entries.iter().find(|e| e.name() == name) {
            problems.push(Problem::error(format!(
                "shadowed by the {} plugin of the same name{}",
                earlier.origin.as_str(),
                earlier
                    .dir
                    .as_ref()
                    .map(|d| format!(" in {}", d.display()))
                    .unwrap_or_default()
            )));
        }
        for command in &manifest.commands {
            if self.options.reserved_commands.contains(&command.name) {
                problems.push(Problem::error(format!(
                    "command \"{}\" collides with a built-in caffeine command",
                    command.name
                )));
            } else if let Some(other) = self.entries.iter().find(|e| {
                e.name() != name && !e.has_errors() && e.manifest.command(&command.name).is_some()
            }) {
                problems.push(Problem::error(format!(
                    "command \"{}\" is already provided by plugin \"{}\"",
                    command.name,
                    other.name()
                )));
            }
        }
        problems.extend(settings_problems(
            &manifest,
            self.options.settings.get(&name),
        ));

        let trust = if origin == Origin::BuiltIn {
            TrustStatus::BuiltIn
        } else {
            match (self.trust.get(&name), &executable, &dir) {
                (None, _, _) => TrustStatus::NotTrusted,
                (Some(record), Some(exe), Some(dir)) => {
                    match trust::digest(&dir.join(MANIFEST_FILE), exe) {
                        Ok(d) if d == record.digest => TrustStatus::Trusted,
                        _ => TrustStatus::Changed,
                    }
                }
                (Some(_), _, _) => TrustStatus::Changed,
            }
        };
        let shadowed = self.entries.iter().any(|e| e.name() == name);
        self.entries.push(PluginEntry {
            enabled: !shadowed && self.options.enabled.contains(&name),
            manifest,
            origin,
            dir,
            executable,
            trust,
            problems,
        });
    }

    pub fn entries(&self) -> &[PluginEntry] {
        &self.entries
    }

    pub fn broken(&self) -> &[BrokenPlugin] {
        &self.broken
    }

    /// The effective entry for `name` (the first, unshadowed one).
    pub fn get(&self, name: &str) -> Option<&PluginEntry> {
        self.entries.iter().find(|e| e.name() == name)
    }

    pub fn require(&self, name: &str) -> Result<&PluginEntry> {
        self.get(name).ok_or_else(|| {
            Error::not_found(format!("no plugin named \"{name}\""))
                .with_hint("run `caffeine plugin list` to see available plugins")
        })
    }

    /// Names listed in `plugins.enabled` that no plugin provides.
    pub fn missing_enabled(&self) -> Vec<String> {
        self.options
            .enabled
            .iter()
            .filter(|n| self.get(n).is_none())
            .cloned()
            .collect()
    }

    /// The enabled plugin providing top-level command `command`.
    pub fn command_provider(&self, command: &str) -> Option<&PluginEntry> {
        self.entries
            .iter()
            .find(|e| e.enabled && !e.has_errors() && e.manifest.command(command).is_some())
    }

    /// Dotted config paths of every secret plugin setting, for redaction.
    pub fn secret_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .flat_map(|e| {
                e.manifest
                    .settings
                    .iter()
                    .filter(|s| s.secret)
                    .map(move |s| format!("plugins.settings.{}.{}", e.name(), s.key))
            })
            .collect()
    }

    /// Manifest defaults overlaid with the user's settings.
    pub fn resolved_settings(&self, entry: &PluginEntry) -> Map<String, Value> {
        let mut settings = entry.manifest.default_settings();
        if let Some(Value::Object(user)) = self.options.settings.get(entry.name()) {
            for (k, v) in user {
                if entry.manifest.setting(k).is_some() {
                    settings.insert(k.clone(), v.clone());
                }
            }
        }
        settings
    }

    /// Per-plugin persistent data directory.
    pub fn data_dir_for(&self, name: &str) -> PathBuf {
        self.options.data_dir.join(name)
    }

    pub fn host_version(&self) -> &semver::Version {
        &self.options.host_version
    }

    /// Check that `name` may be enabled and, for third-party plugins,
    /// record trust in its current files with the declared permissions.
    /// Returns the new `plugins.enabled` list; the caller saves it.
    pub fn enable(&mut self, name: &str) -> Result<Vec<String>> {
        let entry = self.require(name)?.clone();
        if entry.has_errors() {
            let errors: Vec<_> = entry
                .problems
                .iter()
                .filter(|p| p.severity == Severity::Error)
                .map(|p| p.message.clone())
                .collect();
            return Err(Error::plugin(format!(
                "plugin \"{name}\" cannot be enabled:\n  - {}",
                errors.join("\n  - ")
            )));
        }
        if entry.origin.is_third_party() {
            let (Some(dir), Some(exe)) = (&entry.dir, &entry.executable) else {
                return Err(Error::plugin(format!(
                    "plugin \"{name}\" has no executable"
                )));
            };
            let record = TrustRecord {
                version: entry.manifest.plugin.version.clone(),
                digest: trust::digest(&dir.join(MANIFEST_FILE), exe)?,
                permissions: entry.manifest.plugin.permissions.clone(),
                trusted_at: caffeine_core::time::unix_now(),
            };
            self.trust.insert(name, record)?;
        }
        if !self.options.enabled.iter().any(|n| n == name) {
            self.options.enabled.push(name.to_string());
        }
        self.scan();
        Ok(self.options.enabled.clone())
    }

    /// Disable `name` and forget its trust record. Returns the new
    /// `plugins.enabled` list; the caller saves it.
    pub fn disable(&mut self, name: &str) -> Result<Vec<String>> {
        let known = self.get(name).is_some() || self.options.enabled.iter().any(|n| n == name);
        if !known {
            return Err(Error::not_found(format!("no plugin named \"{name}\"")));
        }
        self.trust.remove(name)?;
        self.options.enabled.retain(|n| n != name);
        self.scan();
        Ok(self.options.enabled.clone())
    }

    fn builtin(&self, name: &str) -> Option<Builtin> {
        self.builtin_factories.get(name).copied()
    }

    fn trust_record(&self, name: &str) -> Option<&TrustRecord> {
        self.trust.get(name)
    }
}

fn plugin_subdirs(root: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.join(MANIFEST_FILE).is_file()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    dirs.sort();
    dirs
}

/// Parse `<dir>/caffeine-plugin.toml`.
pub fn read_manifest(dir: &Path) -> std::result::Result<Manifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Manifest::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Problems that depend on the host rather than the manifest itself.
pub fn compatibility_problems(manifest: &Manifest, host_version: &semver::Version) -> Vec<Problem> {
    let mut problems = Vec::new();
    let os = std::env::consts::OS;
    if !manifest.supports_platform(os) {
        problems.push(Problem::error(format!(
            "does not support {os} (supports: {})",
            manifest.plugin.platforms.join(", ")
        )));
    }
    if let Some(req) = &manifest.plugin.caffeine
        && !req.matches(host_version)
    {
        problems.push(Problem::error(format!(
            "requires caffeine {req}, this is {host_version}"
        )));
    }
    problems
}

/// Resolve a third-party plugin's executable, refusing paths that escape
/// the plugin directory (including through symbolic links).
pub fn resolve_executable(manifest: &Manifest, dir: &Path) -> (Option<PathBuf>, Vec<Problem>) {
    let Some(spec) = &manifest.executable else {
        return (
            None,
            vec![Problem::error(
                "third-party plugins must declare an [executable]",
            )],
        );
    };
    let relative = spec.path_for(std::env::consts::OS);
    if !caffeine_plugin_api::manifest::is_contained_relative_path(relative) {
        // Already reported by manifest validation.
        return (None, Vec::new());
    }
    let path = dir.join(relative);
    if !path.is_file() {
        return (
            None,
            vec![Problem::error(format!(
                "executable {} does not exist (has the plugin been built?)",
                path.display()
            ))],
        );
    }
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(real), Ok(root)) if real.starts_with(&root) => (Some(real), Vec::new()),
        (Ok(real), Ok(_)) => (
            None,
            vec![Problem::error(format!(
                "executable resolves to {}, outside the plugin directory",
                real.display()
            ))],
        ),
        (Err(e), _) | (_, Err(e)) => (
            None,
            vec![Problem::error(format!(
                "cannot resolve {}: {e}",
                path.display()
            ))],
        ),
    }
}

/// Validate the user's settings table for a plugin against its schema.
pub fn settings_problems(manifest: &Manifest, user: Option<&Value>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let name = manifest.name();
    match user {
        None => {}
        Some(Value::Object(map)) => {
            let declared: BTreeSet<_> = manifest.settings.iter().map(|s| s.key.as_str()).collect();
            for (key, value) in map {
                match manifest.setting(key) {
                    Some(spec) => {
                        if let Err(e) = spec.validate_value(value) {
                            problems.push(Problem::error(format!("plugins.settings.{name}.{e}")));
                        }
                    }
                    None => problems.push(Problem::warning(format!(
                        "plugins.settings.{name}.{key} is not a setting of this plugin (known: {})",
                        if declared.is_empty() {
                            "none".to_string()
                        } else {
                            declared.iter().copied().collect::<Vec<_>>().join(", ")
                        }
                    ))),
                }
            }
        }
        Some(_) => problems.push(Problem::error(format!(
            "plugins.settings.{name} must be a table"
        ))),
    }
    problems
}

/// Permissions a plugin declares that its trust record does not cover
/// (empty for built-in plugins and for fully trusted ones).
pub fn ungranted_permissions(manager: &Manager, entry: &PluginEntry) -> Vec<Permission> {
    if !entry.origin.is_third_party() {
        return Vec::new();
    }
    let granted = manager
        .trust_record(entry.name())
        .map(|r| r.permissions.clone())
        .unwrap_or_default();
    entry
        .manifest
        .plugin
        .permissions
        .iter()
        .filter(|p| !granted.contains(p))
        .copied()
        .collect()
}
