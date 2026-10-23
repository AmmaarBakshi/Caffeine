//! The plugin manifest (`caffeine-plugin.toml`).
//!
//! ```toml
//! [plugin]
//! name = "hello"
//! version = "0.1.0"
//! description = "Greets people"
//! api = "^1.0"                 # protocol versions this plugin speaks
//! caffeine = ">=0.1"           # optional: required Caffeine version
//! platforms = ["linux", "macos", "windows"]   # optional: default all
//! permissions = ["power.inhibit"]
//!
//! [executable]
//! path = "bin/caffeine-hello"         # relative to the plugin directory
//! windows = "bin/caffeine-hello.exe"  # optional per-OS override
//! args = []
//!
//! [[commands]]
//! name = "hello"
//! summary = "Print a greeting"
//! usage = "hello [NAME]"
//! keep_awake = false
//!
//! [[settings]]
//! key = "greeting"
//! type = "string"
//! default = "Hello"
//! description = "Word used to greet"
//! ```
//!
//! Unknown fields are rejected: a typo in `permissions` must not silently
//! produce a plugin that appears to ask for nothing.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission::Permission;
use crate::{PROTOCOL_VERSION, is_valid_name};

/// File name of a plugin manifest inside its directory.
pub const MANIFEST_FILE: &str = "caffeine-plugin.toml";

/// Operating systems a manifest may list in `platforms`.
pub const PLATFORMS: &[&str] = &["linux", "macos", "windows"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugin: PluginInfo,
    /// The program implementing the plugin. Required for third-party
    /// plugins; built-in plugins run in-process and have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub settings: Vec<SettingSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInfo {
    pub name: String,
    pub version: Version,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Protocol versions the plugin speaks.
    pub api: VersionReq,
    /// Caffeine versions the plugin works with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caffeine: Option<VersionReq>,
    /// Supported operating systems; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableSpec {
    /// Path relative to the plugin directory.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<String>,
    /// Arguments always passed before the protocol starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

impl ExecutableSpec {
    /// The relative path to use on `os` (a [`std::env::consts::OS`] value).
    pub fn path_for(&self, os: &str) -> &str {
        let specific = match os {
            "windows" => self.windows.as_deref(),
            "macos" => self.macos.as_deref(),
            "linux" => self.linux.as_deref(),
            _ => None,
        };
        specific.unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Name, also usable as a top-level `caffeine <name>` command.
    pub name: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Keep the system awake while the command runs (needs `power.inhibit`).
    #[serde(default)]
    pub keep_awake: bool,
}

/// Type of a plugin setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingType {
    Bool,
    String,
    Integer,
    /// One of `choices`.
    Choice,
    StringList,
    /// An absolute filesystem path.
    Path,
    /// A free-form table, validated by the plugin itself.
    Table,
}

impl SettingType {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingType::Bool => "bool",
            SettingType::String => "string",
            SettingType::Integer => "integer",
            SettingType::Choice => "choice",
            SettingType::StringList => "string-list",
            SettingType::Path => "path",
            SettingType::Table => "table",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingSpec {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: SettingType,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Redacted from `caffeine config show` and never logged.
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl SettingSpec {
    /// Check a configured value against the declared type.
    pub fn validate_value(&self, value: &Value) -> Result<(), String> {
        let wrong = |expected: &str| Err(format!("{} must be {expected}", self.key));
        match self.kind {
            SettingType::Bool if !value.is_boolean() => wrong("true or false"),
            SettingType::String if !value.is_string() => wrong("a string"),
            SettingType::Integer => {
                let Some(n) = value.as_i64() else {
                    return wrong("an integer");
                };
                if self.min.is_some_and(|min| n < min) || self.max.is_some_and(|max| n > max) {
                    return Err(format!(
                        "{} must be between {} and {} (got {n})",
                        self.key,
                        self.min.map_or("-inf".into(), |m| m.to_string()),
                        self.max.map_or("+inf".into(), |m| m.to_string()),
                    ));
                }
                Ok(())
            }
            SettingType::Choice => match value.as_str() {
                Some(s) if self.choices.iter().any(|c| c == s) => Ok(()),
                _ => wrong(&format!("one of: {}", self.choices.join(", "))),
            },
            SettingType::StringList => match value.as_array() {
                Some(items) if items.iter().all(Value::is_string) => Ok(()),
                _ => wrong("a list of strings"),
            },
            SettingType::Path => match value.as_str() {
                Some(s) if Path::new(s).is_absolute() || s.is_empty() => Ok(()),
                _ => wrong("an absolute path"),
            },
            SettingType::Table if !value.is_object() => wrong("a table"),
            _ => Ok(()),
        }
    }

    /// Human-readable description of accepted values.
    pub fn describe_type(&self) -> String {
        match self.kind {
            SettingType::Choice => self.choices.join(" | "),
            SettingType::Integer => format!(
                "integer {}..={}",
                self.min.map_or(String::new(), |m| m.to_string()),
                self.max.map_or(String::new(), |m| m.to_string())
            ),
            other => other.as_str().to_string(),
        }
    }
}

/// How serious a manifest problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The plugin cannot be enabled or run.
    Error,
    /// The plugin works but something looks wrong.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn error(message: impl Into<String>) -> Problem {
        Problem {
            severity: Severity::Error,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Problem {
        Problem {
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{tag}: {}", self.message)
    }
}

/// Whether `key` is a valid setting key: lowercase letters, digits and
/// underscores, starting with a letter.
pub fn is_valid_setting_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && key.len() <= 64
}

/// Whether `path` is relative and stays inside the directory it is
/// resolved against (no `..`, no root or drive prefix).
pub fn is_contained_relative_path(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && !path.contains('\\')
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

impl Manifest {
    /// Parse a manifest from TOML text. Structural errors (missing fields,
    /// wrong types, unknown fields) fail here; semantic checks are in
    /// [`Manifest::validate`].
    pub fn from_toml(text: &str) -> Result<Manifest, String> {
        toml_edit::de::from_str(text).map_err(|e| e.to_string().trim().to_string())
    }

    pub fn to_toml(&self) -> String {
        toml_edit::ser::to_string_pretty(self).expect("manifest serializes to TOML")
    }

    pub fn name(&self) -> &str {
        &self.plugin.name
    }

    pub fn command(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|c| c.name == name)
    }

    pub fn setting(&self, key: &str) -> Option<&SettingSpec> {
        self.settings.iter().find(|s| s.key == key)
    }

    pub fn has_permission(&self, p: Permission) -> bool {
        self.plugin.permissions.contains(&p)
    }

    /// Whether the plugin declares support for `os`.
    pub fn supports_platform(&self, os: &str) -> bool {
        self.plugin.platforms.is_empty() || self.plugin.platforms.iter().any(|p| p == os)
    }

    /// Whether the plugin speaks the host's protocol version.
    pub fn is_protocol_compatible(&self) -> bool {
        self.plugin.api.matches(&PROTOCOL_VERSION)
    }

    /// Default values of every setting that declares one.
    pub fn default_settings(&self) -> serde_json::Map<String, Value> {
        self.settings
            .iter()
            .filter_map(|s| s.default.clone().map(|d| (s.key.clone(), d)))
            .collect()
    }

    /// Semantic validation. Every problem is reported, not only the first.
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let info = &self.plugin;
        if !is_valid_name(&info.name) {
            problems.push(Problem::error(format!(
                "plugin name \"{}\" is invalid: use lowercase letters, digits and single \
                 hyphens, starting with a letter (at most 64 characters)",
                info.name
            )));
        }
        if info.description.trim().is_empty() {
            problems.push(Problem::error("plugin.description must not be empty"));
        }
        if !self.is_protocol_compatible() {
            problems.push(Problem::error(format!(
                "plugin requires protocol {} but this Caffeine speaks {PROTOCOL_VERSION}",
                info.api
            )));
        }
        for p in &info.platforms {
            if !PLATFORMS.contains(&p.as_str()) {
                problems.push(Problem::warning(format!(
                    "unknown platform \"{p}\" (known: {})",
                    PLATFORMS.join(", ")
                )));
            }
        }
        let mut seen = BTreeSet::new();
        for p in &info.permissions {
            if !seen.insert(*p) {
                problems.push(Problem::warning(format!("permission {p} is listed twice")));
            }
        }

        if let Some(exe) = &self.executable {
            for (field, path) in [
                ("path", Some(&exe.path)),
                ("windows", exe.windows.as_ref()),
                ("macos", exe.macos.as_ref()),
                ("linux", exe.linux.as_ref()),
            ] {
                if let Some(path) = path
                    && !is_contained_relative_path(path)
                {
                    problems.push(Problem::error(format!(
                        "executable.{field} \"{path}\" must be a relative path inside the plugin \
                         directory, using forward slashes"
                    )));
                }
            }
        }

        if self.commands.is_empty() {
            problems.push(Problem::error(
                "a plugin must declare at least one [[commands]] entry",
            ));
        }
        let mut names = BTreeSet::new();
        for c in &self.commands {
            if !is_valid_name(&c.name) {
                problems.push(Problem::error(format!(
                    "command name \"{}\" is invalid",
                    c.name
                )));
            }
            if !names.insert(c.name.as_str()) {
                problems.push(Problem::error(format!(
                    "command \"{}\" is declared twice",
                    c.name
                )));
            }
            if c.summary.trim().is_empty() {
                problems.push(Problem::warning(format!(
                    "command \"{}\" has no summary",
                    c.name
                )));
            }
            if c.keep_awake && !self.has_permission(Permission::PowerInhibit) {
                problems.push(Problem::error(format!(
                    "command \"{}\" sets keep_awake but the plugin does not declare the \
                     power.inhibit permission",
                    c.name
                )));
            }
        }

        let mut keys = BTreeSet::new();
        for s in &self.settings {
            if !is_valid_setting_key(&s.key) {
                problems.push(Problem::error(format!(
                    "setting key \"{}\" is invalid: use lowercase letters, digits and underscores",
                    s.key
                )));
            }
            if !keys.insert(s.key.as_str()) {
                problems.push(Problem::error(format!(
                    "setting \"{}\" is declared twice",
                    s.key
                )));
            }
            if s.kind == SettingType::Choice && s.choices.is_empty() {
                problems.push(Problem::error(format!(
                    "setting \"{}\" has type choice but no choices",
                    s.key
                )));
            }
            if s.kind != SettingType::Choice && !s.choices.is_empty() {
                problems.push(Problem::warning(format!(
                    "setting \"{}\": choices are ignored for type {}",
                    s.key,
                    s.kind.as_str()
                )));
            }
            if let (Some(min), Some(max)) = (s.min, s.max)
                && min > max
            {
                problems.push(Problem::error(format!(
                    "setting \"{}\": min exceeds max",
                    s.key
                )));
            }
            if let Some(default) = &s.default
                && let Err(e) = s.validate_value(default)
            {
                problems.push(Problem::error(format!("default of {e}")));
            }
            if s.secret && s.default.is_some() {
                problems.push(Problem::warning(format!(
                    "secret setting \"{}\" has a default value; secrets should not ship in manifests",
                    s.key
                )));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[plugin]
name = "hello"
version = "0.1.0"
description = "Greets people"
api = "^1.0"
permissions = ["power.inhibit"]

[executable]
path = "bin/hello"
windows = "bin/hello.exe"

[[commands]]
name = "hello"
summary = "Print a greeting"
keep_awake = true

[[settings]]
key = "greeting"
type = "string"
default = "Hello"
description = "Word used to greet"

[[settings]]
key = "times"
type = "integer"
min = 1
max = 10
default = 1
description = "Repetitions"
"#;

    #[test]
    fn parses_and_validates_example() {
        let m = Manifest::from_toml(EXAMPLE).unwrap();
        assert_eq!(m.name(), "hello");
        assert!(m.validate().is_empty(), "{:?}", m.validate());
        assert_eq!(
            m.executable.as_ref().unwrap().path_for("windows"),
            "bin/hello.exe"
        );
        assert_eq!(
            m.executable.as_ref().unwrap().path_for("linux"),
            "bin/hello"
        );
        assert_eq!(m.default_settings()["times"], 1);
        assert!(m.supports_platform("macos"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let typo = EXAMPLE.replace("permissions =", "permisions =");
        assert!(
            Manifest::from_toml(&typo)
                .unwrap_err()
                .contains("permisions")
        );
    }

    #[test]
    fn reports_semantic_problems() {
        let bad = EXAMPLE
            .replace(
                "name = \"hello\"\nversion",
                "name = \"Hello World\"\nversion",
            )
            .replace("permissions = [\"power.inhibit\"]", "permissions = []")
            .replace("path = \"bin/hello\"", "path = \"../escape\"")
            .replace("default = 1\n", "default = 99\n")
            .replace("api = \"^1.0\"", "api = \"^2\"");
        let problems = Manifest::from_toml(&bad).unwrap().validate();
        let errors: Vec<_> = problems.iter().filter(|p| p.is_error()).collect();
        assert_eq!(errors.len(), 5, "{problems:#?}");
    }

    #[test]
    fn contained_paths() {
        assert!(is_contained_relative_path("bin/tool"));
        assert!(is_contained_relative_path("./tool.exe"));
        for bad in ["", "../x", "bin/../../x", "/usr/bin/x", "C:/x", "bin\\x"] {
            assert!(!is_contained_relative_path(bad), "{bad}");
        }
    }

    #[test]
    fn setting_values() {
        let m = Manifest::from_toml(EXAMPLE).unwrap();
        let times = m.setting("times").unwrap();
        assert!(times.validate_value(&Value::from(5)).is_ok());
        assert!(times.validate_value(&Value::from(0)).is_err());
        assert!(times.validate_value(&Value::from("5")).is_err());
    }

    #[test]
    fn toml_round_trip() {
        let m = Manifest::from_toml(EXAMPLE).unwrap();
        assert_eq!(Manifest::from_toml(&m.to_toml()).unwrap(), m);
    }
}
