//! The configuration schema (version 1).
//!
//! Every field has a validated default so that an empty or missing file is a
//! valid configuration. Unknown keys are tolerated (and reported as warnings)
//! so that a configuration written by a newer Caffeine still loads.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::duration::SessionLength;
use crate::error::{Error, Result};
use crate::power::LidSessionMode;

/// The schema version written by this build.
pub const SCHEMA_VERSION: u32 = 1;

/// Experimental feature flags understood by this build.
pub const EXPERIMENTAL_FLAGS: &[(&str, &str)] = &[(
    "session-lid-override",
    "Windows: emulate a session-scoped lid override by temporarily changing the \
     active power scheme (restored on stop, journaled for crash recovery)",
)];

/// Power backends selectable per platform. `auto` is always valid.
pub fn backend_choices() -> &'static [&'static str] {
    if cfg!(windows) {
        &["auto", "power-request", "execution-state"]
    } else if cfg!(target_os = "macos") {
        &["auto", "iokit"]
    } else if cfg!(target_os = "linux") {
        &["auto", "logind", "desktop", "logind+desktop"]
    } else {
        &["auto"]
    }
}

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub general: General,
    pub power: Power,
    pub session: Session,
    pub plugins: Plugins,
    pub advanced: Advanced,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema_version: SCHEMA_VERSION,
            general: General::default(),
            power: Power::default(),
            session: Session::default(),
            plugins: Plugins::default(),
            advanced: Advanced::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// What `caffeine` does when run without a subcommand.
    pub default_command: DefaultCommand,
    pub log_level: LogLevel,
    pub notifications: Notifications,
    pub color: ColorMode,
    /// Print battery, ventilation and closed-lid heat warnings.
    pub safety_warnings: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            default_command: DefaultCommand::Status,
            log_level: LogLevel::Warn,
            notifications: Notifications::None,
            color: ColorMode::Auto,
            safety_warnings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Power {
    /// Prevent idle-triggered system sleep during sessions.
    pub prevent_idle_sleep: bool,
    /// Display policy during sessions.
    pub display: DisplayPolicy,
    /// Query the lid state in `status` output.
    pub lid_detection: bool,
    /// Lid-close handling during sessions.
    pub lid_session: LidSessionMode,
    /// What to do when the machine switches to battery during a session.
    pub on_battery: BatteryAction,
    /// End the session when battery charge drops below this percentage
    /// (0 disables the check).
    pub battery_threshold: u8,
}

impl Default for Power {
    fn default() -> Self {
        Power {
            prevent_idle_sleep: true,
            display: DisplayPolicy::AllowSleep,
            lid_detection: true,
            lid_session: LidSessionMode::System,
            on_battery: BatteryAction::Continue,
            battery_threshold: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Duration used by `caffeine start` without `--duration`.
    pub default_duration: SessionLength,
    /// Upper bound for any session (`indefinite` = no bound).
    pub max_duration: SessionLength,
    pub on_expire: ExpireAction,
    /// What `start` does when a session with different settings is running.
    pub on_conflict: ConflictPolicy,
    /// Seconds `caffeine stop` waits for the session host to exit.
    pub stop_timeout_secs: u32,
    /// Keep a record of the last finished session for `status`.
    pub record_last_session: bool,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            default_duration: SessionLength::Indefinite,
            max_duration: SessionLength::Indefinite,
            on_expire: ExpireAction::Stop,
            on_conflict: ConflictPolicy::Error,
            stop_timeout_secs: 10,
            record_last_session: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugins {
    /// Names of enabled plugins. Enabling a third-party plugin additionally
    /// requires a trust record created by `caffeine plugin enable`.
    pub enabled: Vec<String>,
    /// Extra directories searched for third-party plugins.
    pub search_paths: Vec<PathBuf>,
    /// Plugin-specific settings, keyed by plugin name. Validated against
    /// each plugin's declared schema by the plugin manager.
    pub settings: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    /// Native backend selection; see [`backend_choices`].
    pub backend: String,
    /// Include low-level details in `caffeine doctor`.
    pub verbose_diagnostics: bool,
    /// Log file for background session hosts (empty = default location).
    pub log_file: Option<PathBuf>,
    /// Opt-in experimental features; see [`EXPERIMENTAL_FLAGS`].
    pub experimental: Vec<String>,
}

impl Default for Advanced {
    fn default() -> Self {
        Advanced {
            backend: "auto".to_string(),
            verbose_diagnostics: false,
            log_file: None,
            experimental: Vec::new(),
        }
    }
}

impl Advanced {
    pub fn experimental_enabled(&self, flag: &str) -> bool {
        self.experimental.iter().any(|f| f == flag)
    }
}

/// Declare a string-backed enum with `FromStr`, `Display`, serde support and
/// the list of accepted values.
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const VALUES: &'static [&'static str] = &[$($text),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self> {
                match s.trim() {
                    $($text => Ok($name::$variant),)+
                    other => Err(Error::usage(format!(
                        "invalid value \"{}\" (expected one of: {})",
                        other,
                        Self::VALUES.join(", ")
                    ))),
                }
            }
        }
    };
}

string_enum!(
    /// Behavior of a bare `caffeine` invocation.
    DefaultCommand { Status => "status", Help => "help", Start => "start" }
);
string_enum!(LogLevel { Error => "error", Warn => "warn", Info => "info", Debug => "debug", Trace => "trace" });
string_enum!(
    /// Terminal-only notification mechanisms used by foreground sessions.
    Notifications { None => "none", Bell => "bell", Osc9 => "osc9" }
);
string_enum!(ColorMode { Auto => "auto", Always => "always", Never => "never" });
string_enum!(DisplayPolicy { AllowSleep => "allow-sleep", KeepOn => "keep-on" });
string_enum!(BatteryAction { Continue => "continue", Stop => "stop" });
string_enum!(ExpireAction { Stop => "stop", StopAndNotify => "stop-and-notify" });
string_enum!(ConflictPolicy { Error => "error", Keep => "keep", Replace => "replace" });

impl LogLevel {
    pub fn to_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
    }
}

/// Whether `name` is a valid plugin identifier: lowercase ASCII letters,
/// digits and single hyphens, starting with a letter, at most 64 bytes.
pub fn is_valid_plugin_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && !name.ends_with('-')
        && !name.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

impl Config {
    /// Semantic validation beyond what deserialization checks. Returns every
    /// problem found, not just the first.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.schema_version == 0 || self.schema_version > SCHEMA_VERSION {
            problems.push(format!(
                "schema_version {} is not supported by this build (expected 1..={SCHEMA_VERSION})",
                self.schema_version
            ));
        }
        if self.power.battery_threshold > 100 {
            problems.push(format!(
                "power.battery_threshold must be between 0 and 100 (got {})",
                self.power.battery_threshold
            ));
        }
        if let (SessionLength::Fixed(default), SessionLength::Fixed(max)) =
            (self.session.default_duration, self.session.max_duration)
            && default > max
        {
            problems.push(format!(
                "session.default_duration ({}) exceeds session.max_duration ({})",
                self.session.default_duration, self.session.max_duration
            ));
        }
        if self.session.stop_timeout_secs == 0 || self.session.stop_timeout_secs > 300 {
            problems.push(format!(
                "session.stop_timeout_secs must be between 1 and 300 (got {})",
                self.session.stop_timeout_secs
            ));
        }
        if !backend_choices().contains(&self.advanced.backend.as_str()) {
            problems.push(format!(
                "advanced.backend \"{}\" is not available on this platform (expected one of: {})",
                self.advanced.backend,
                backend_choices().join(", ")
            ));
        }
        for flag in &self.advanced.experimental {
            if !EXPERIMENTAL_FLAGS.iter().any(|(f, _)| f == flag) {
                problems.push(format!("advanced.experimental: unknown flag \"{flag}\""));
            }
        }
        if let Some(log_file) = &self.advanced.log_file
            && !log_file.as_os_str().is_empty()
            && !log_file.is_absolute()
        {
            problems.push(format!(
                "advanced.log_file must be an absolute path (got \"{}\")",
                log_file.display()
            ));
        }
        for name in &self.plugins.enabled {
            if !is_valid_plugin_name(name) {
                problems.push(format!("plugins.enabled: \"{name}\" is not a valid plugin name"));
            }
        }
        for name in self.plugins.settings.keys() {
            if !is_valid_plugin_name(name) {
                problems.push(format!("plugins.settings: \"{name}\" is not a valid plugin name"));
            }
        }
        for path in &self.plugins.search_paths {
            if !path.is_absolute() {
                problems.push(format!(
                    "plugins.search_paths: \"{}\" must be an absolute path",
                    path.display()
                ));
            }
        }
        problems
    }

    /// Clamp a requested session length to `session.max_duration`.
    /// Returns the effective length and whether it was reduced.
    pub fn clamp_length(&self, requested: SessionLength) -> (SessionLength, bool) {
        match (requested, self.session.max_duration) {
            (_, SessionLength::Indefinite) => (requested, false),
            (SessionLength::Indefinite, max) => (max, true),
            (SessionLength::Fixed(r), SessionLength::Fixed(m)) if r > m => {
                (SessionLength::Fixed(m), true)
            }
            (r, _) => (r, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn defaults_are_valid() {
        assert!(Config::default().validate().is_empty());
    }

    #[test]
    fn validation_reports_every_problem() {
        let mut c = Config::default();
        c.power.battery_threshold = 150;
        c.session.default_duration = SessionLength::Fixed(Duration::from_secs(7_200));
        c.session.max_duration = SessionLength::Fixed(Duration::from_secs(3_600));
        c.advanced.backend = "nonsense".into();
        c.advanced.experimental = vec!["warp-drive".into()];
        c.plugins.enabled = vec!["Bad Name".into()];
        let problems = c.validate();
        assert_eq!(problems.len(), 5, "{problems:#?}");
    }

    #[test]
    fn plugin_names() {
        for good in ["build", "my-plugin", "a1", "x"] {
            assert!(is_valid_plugin_name(good), "{good}");
        }
        for bad in ["", "Build", "1abc", "a--b", "a-", "a_b", "a b", &"a".repeat(65)] {
            assert!(!is_valid_plugin_name(bad), "{bad}");
        }
    }

    #[test]
    fn clamp_respects_max_duration() {
        let mut c = Config::default();
        let two_h = SessionLength::Fixed(Duration::from_secs(7_200));
        assert_eq!(c.clamp_length(two_h), (two_h, false));
        c.session.max_duration = SessionLength::Fixed(Duration::from_secs(3_600));
        let one_h = SessionLength::Fixed(Duration::from_secs(3_600));
        assert_eq!(c.clamp_length(two_h), (one_h, true));
        assert_eq!(c.clamp_length(SessionLength::Indefinite), (one_h, true));
    }

    #[test]
    fn enums_parse_and_display() {
        assert_eq!("keep-on".parse::<DisplayPolicy>().unwrap(), DisplayPolicy::KeepOn);
        assert!("sometimes".parse::<DisplayPolicy>().is_err());
        assert_eq!(ConflictPolicy::Replace.to_string(), "replace");
    }
}
