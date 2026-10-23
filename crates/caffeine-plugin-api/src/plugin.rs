//! The Rust-facing plugin interface.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::manifest::Manifest;
use crate::permission::Permission;

/// Log levels, from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Information about the host, sent once before any request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Always `caffeine`.
    pub name: String,
    pub version: semver::Version,
    /// [`std::env::consts::OS`] of the host.
    pub os: String,
    /// [`std::env::consts::ARCH`] of the host.
    pub arch: String,
}

impl HostInfo {
    /// Describe the current process as a host of the given version.
    pub fn current(version: semver::Version) -> HostInfo {
        HostInfo {
            name: "caffeine".into(),
            version,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        }
    }
}

/// Everything a plugin learns at initialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Initialize {
    pub host: HostInfo,
    /// The plugin's settings: manifest defaults overlaid with the user's
    /// `[plugins.settings.<name>]` table, already validated against the
    /// declared schema.
    pub settings: Map<String, Value>,
    /// Permissions the user granted (a subset of those declared).
    pub permissions: Vec<Permission>,
    /// A directory the plugin may use for its own persistent data.
    pub data_dir: PathBuf,
}

/// One command invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    /// One of the commands declared in the manifest.
    pub command: String,
    /// Arguments after the command name, exactly as the user typed them.
    pub args: Vec<String>,
    /// The user's working directory.
    pub cwd: PathBuf,
    /// The user asked for machine-readable output: report results with
    /// [`Host::data`] and keep [`Host::print`] for incidental text.
    pub json: bool,
}

/// A failure reported by a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> PluginError {
        PluginError {
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> PluginError {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

/// Outcome of one diagnostic check (`caffeine doctor`, `plugin info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Stable identifier, e.g. `tool.cargo`.
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    pub fn new(id: impl Into<String>, status: CheckStatus, summary: impl Into<String>) -> Check {
        Check {
            id: id.into(),
            status,
            summary: summary.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Check {
        self.hint = Some(hint.into());
        self
    }
}

/// What a plugin may ask of the host while handling a request.
pub trait Host {
    /// Write to Caffeine's log (stderr with `-v`, or the session log).
    fn log(&mut self, level: LogLevel, message: &str);

    /// Print a line of human-readable output. Suppressed in `--json` and
    /// `--quiet` mode.
    fn print(&mut self, text: &str);

    /// Contribute a machine-readable result. In `--json` mode the values
    /// of all calls are emitted as the command's JSON document (a single
    /// value as-is, several as an array).
    fn data(&mut self, value: Value);
}

/// A Caffeine plugin.
///
/// Lifecycle for each `caffeine` invocation that involves the plugin:
/// [`Plugin::initialize`], then one [`Plugin::invoke`] or
/// [`Plugin::diagnose`], then [`Plugin::shutdown`]. A plugin is never
/// called unless the user enabled it, and is never called merely because a
/// configuration file was loaded.
pub trait Plugin: Send {
    /// The plugin's manifest. For third-party plugins this must describe
    /// the same name and version as the `caffeine-plugin.toml` file.
    fn manifest(&self) -> Manifest;

    /// Prepare for requests. Fail here if the settings are unusable.
    fn initialize(&mut self, init: &Initialize) -> Result<(), PluginError> {
        let _ = init;
        Ok(())
    }

    /// Run one of the declared commands and return its exit code.
    fn invoke(&mut self, invocation: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError>;

    /// Report on the plugin's health (missing tools, bad settings, ...).
    fn diagnose(&mut self) -> Vec<Check> {
        Vec::new()
    }

    /// Release resources. Called even when a request failed.
    fn shutdown(&mut self) {}
}
