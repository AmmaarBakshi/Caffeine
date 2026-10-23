//! Metadata for every built-in setting.
//!
//! This registry drives `caffeine config get/set`, the interactive editor,
//! the generated default configuration file and unknown-key detection, so
//! that all four always agree on names, types and help text.

use crate::power::Capability;

use super::schema::{
    BatteryAction, ColorMode, ConflictPolicy, DefaultCommand, DisplayPolicy, ExpireAction,
    LogLevel, Notifications, backend_choices,
};

/// Editor section a setting belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    General,
    Power,
    Sessions,
    Plugins,
    Advanced,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::General,
        Section::Power,
        Section::Sessions,
        Section::Plugins,
        Section::Advanced,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Power => "Power management",
            Section::Sessions => "Sessions",
            Section::Plugins => "Plugins",
            Section::Advanced => "Advanced",
        }
    }
}

/// Value type of a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    /// One of a fixed list of strings.
    Choice(&'static [&'static str]),
    /// A [`crate::duration::SessionLength`] such as `2h` or `indefinite`.
    Length,
    Integer {
        min: i64,
        max: i64,
    },
    /// An optional absolute path; empty means "default".
    OptionalPath,
    /// A list of absolute paths.
    PathList,
    /// A list of strings, optionally restricted to known values.
    StringList(Option<&'static [&'static str]>),
}

impl SettingKind {
    pub fn describe(&self) -> String {
        match self {
            SettingKind::Bool => "true | false".into(),
            SettingKind::Choice(c) => c.join(" | "),
            SettingKind::Length => "duration (e.g. 30m, 2h, 1h30m) or indefinite".into(),
            SettingKind::Integer { min, max } => format!("integer {min}..={max}"),
            SettingKind::OptionalPath => "absolute path, or empty for the default".into(),
            SettingKind::PathList => "comma-separated absolute paths".into(),
            SettingKind::StringList(Some(c)) => {
                format!("comma-separated subset of: {}", c.join(", "))
            }
            SettingKind::StringList(None) => "comma-separated list".into(),
        }
    }
}

/// Description of one setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted TOML path, e.g. `power.display`.
    pub key: &'static str,
    pub section: Section,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
    /// Capabilities that must be usable for the setting to have any effect.
    /// The editor shows the setting as unsupported (and refuses to toggle
    /// it) when any is missing.
    pub requires: &'static [Capability],
}

const EXPERIMENTAL_NAMES: &[&str] = &["session-lid-override"];

/// Every built-in setting, in display order.
pub fn all() -> Vec<SettingDef> {
    use Section::*;
    use SettingKind::*;
    vec![
        SettingDef {
            key: "general.default_command",
            section: General,
            label: "Startup behavior",
            help: "What a bare `caffeine` invocation does: show status, print help, or start a session.",
            kind: Choice(DefaultCommand::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "general.log_level",
            section: General,
            label: "Logging verbosity",
            help: "Minimum level written to the session log and to stderr with -v.",
            kind: Choice(LogLevel::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "general.notifications",
            section: General,
            label: "Notifications",
            help: "Terminal-only notice when a foreground session ends: none, the terminal bell, \
                   or an OSC 9 desktop notification escape (supported by some terminals).",
            kind: Choice(Notifications::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "general.color",
            section: General,
            label: "Color output",
            help: "Colorize human-readable output. NO_COLOR in the environment always disables color.",
            kind: Choice(ColorMode::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "general.safety_warnings",
            section: General,
            label: "Safety warnings",
            help: "Warn about battery drain, ventilation and closed-lid heat when starting sessions.",
            kind: Bool,
            requires: &[],
        },
        SettingDef {
            key: "power.prevent_idle_sleep",
            section: Power,
            label: "Idle-sleep prevention",
            help: "Sessions stop the idle timer from suspending the system. Explicit suspend is never blocked.",
            kind: Bool,
            requires: &[Capability::IdleSleep],
        },
        SettingDef {
            key: "power.display",
            section: Power,
            label: "Display-sleep policy",
            help: "keep-on also prevents the display from sleeping during sessions.",
            kind: Choice(DisplayPolicy::VALUES),
            requires: &[Capability::DisplaySleep],
        },
        SettingDef {
            key: "power.lid_detection",
            section: Power,
            label: "Lid-state detection",
            help: "Report whether the lid is open or closed in `caffeine status`.",
            kind: Bool,
            requires: &[Capability::LidState],
        },
        SettingDef {
            key: "power.lid_session",
            section: Power,
            label: "Lid-close policy during sessions",
            help: "Keep running with the lid closed while a session is active. Mind ventilation: a \
                   closed laptop in a bag can overheat.",
            kind: Choice(&["system", "keep-awake-on-ac", "keep-awake"]),
            requires: &[Capability::LidSessionOverride],
        },
        SettingDef {
            key: "power.on_battery",
            section: Power,
            label: "On switch to battery",
            help: "Continue the session or end it when the machine starts running on battery.",
            kind: Choice(BatteryAction::VALUES),
            requires: &[Capability::PowerSource],
        },
        SettingDef {
            key: "power.battery_threshold",
            section: Power,
            label: "Battery threshold (%)",
            help: "End the session when battery charge falls below this percentage. 0 disables the check.",
            kind: Integer { min: 0, max: 100 },
            requires: &[Capability::PowerSource],
        },
        SettingDef {
            key: "session.default_duration",
            section: Sessions,
            label: "Default session duration",
            help: "Used by `caffeine start` without --duration.",
            kind: Length,
            requires: &[],
        },
        SettingDef {
            key: "session.max_duration",
            section: Sessions,
            label: "Maximum session duration",
            help: "Upper bound applied to every session, including indefinite ones.",
            kind: Length,
            requires: &[],
        },
        SettingDef {
            key: "session.on_expire",
            section: Sessions,
            label: "When a session expires",
            help: "stop-and-notify also sends the configured terminal notification (foreground sessions).",
            kind: Choice(ExpireAction::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "session.on_conflict",
            section: Sessions,
            label: "Start while a session runs",
            help: "When `start` asks for different settings than the running session: fail, keep the \
                   running session, or replace it.",
            kind: Choice(ConflictPolicy::VALUES),
            requires: &[],
        },
        SettingDef {
            key: "session.stop_timeout_secs",
            section: Sessions,
            label: "Stop timeout (seconds)",
            help: "How long `caffeine stop` waits for the session host to release its assertions.",
            kind: Integer { min: 1, max: 300 },
            requires: &[],
        },
        SettingDef {
            key: "session.record_last_session",
            section: Sessions,
            label: "Remember last session",
            help: "Keep a record of how the previous session ended, shown by `caffeine status`.",
            kind: Bool,
            requires: &[],
        },
        SettingDef {
            key: "plugins.enabled",
            section: Plugins,
            label: "Enabled plugins",
            help: "Use `caffeine plugin enable <name>` so permissions are reviewed and trust is recorded.",
            kind: StringList(None),
            requires: &[],
        },
        SettingDef {
            key: "plugins.search_paths",
            section: Plugins,
            label: "Extra plugin directories",
            help: "Additional directories searched for third-party plugins.",
            kind: PathList,
            requires: &[],
        },
        SettingDef {
            key: "advanced.backend",
            section: Advanced,
            label: "Platform backend",
            help: "Native mechanism used for sessions. `auto` picks the best available.",
            kind: Choice(backend_choices()),
            requires: &[],
        },
        SettingDef {
            key: "advanced.verbose_diagnostics",
            section: Advanced,
            label: "Verbose diagnostics",
            help: "Include low-level details in `caffeine doctor`.",
            kind: Bool,
            requires: &[],
        },
        SettingDef {
            key: "advanced.log_file",
            section: Advanced,
            label: "Log file",
            help: "Where background sessions write their log. Empty uses the default location.",
            kind: OptionalPath,
            requires: &[],
        },
        SettingDef {
            key: "advanced.experimental",
            section: Advanced,
            label: "Experimental features",
            help: "session-lid-override: on Windows, temporarily change the lid-close action while a \
                   session runs (restored on stop; `caffeine lid restore` recovers after a crash).",
            kind: StringList(Some(EXPERIMENTAL_NAMES)),
            requires: &[],
        },
    ]
}

/// Look up a setting by its dotted key.
pub fn find(key: &str) -> Option<SettingDef> {
    all().into_iter().find(|s| s.key == key)
}

/// Whether `key` (a dotted path found in a config file) is known. Anything
/// below `plugins.settings` belongs to plugins and is validated elsewhere.
pub fn is_known_path(key: &str) -> bool {
    key == "schema_version"
        || key == "plugins.settings"
        || key.starts_with("plugins.settings.")
        || all()
            .iter()
            .any(|s| s.key == key || s.key.starts_with(&format!("{key}.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn keys_are_unique() {
        let defs = all();
        let mut keys: Vec<_> = defs.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), defs.len());
    }

    #[test]
    fn every_setting_exists_in_the_schema() {
        let json = serde_json::to_value(Config::default()).unwrap();
        for def in all() {
            let mut node = &json;
            for part in def.key.split('.') {
                node = node
                    .get(part)
                    .unwrap_or_else(|| panic!("{} missing from schema", def.key));
            }
        }
    }

    #[test]
    fn experimental_names_match_schema_flags() {
        let names: Vec<_> = crate::config::schema::EXPERIMENTAL_FLAGS
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(names, EXPERIMENTAL_NAMES);
    }

    #[test]
    fn known_paths() {
        assert!(is_known_path("power"));
        assert!(is_known_path("power.display"));
        assert!(is_known_path("plugins.settings.build.tasks"));
        assert!(!is_known_path("power.turbo"));
        assert!(!is_known_path("nonsense"));
    }
}
