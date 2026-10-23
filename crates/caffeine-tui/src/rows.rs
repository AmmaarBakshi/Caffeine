//! The editor's row model: what each line shows and how edits apply.

use serde_json::{Map, Value};

use caffeine_core::config::{ConfigFile, Section, SettingDef, SettingKind, parse_bool, settings};
use caffeine_core::power::CapabilityReport;
use caffeine_core::{Error, Result};
use caffeine_plugin_api::manifest::SettingType;
use caffeine_plugin_api::{Permission, SettingSpec};
use caffeine_plugin_manager::{Manager, Origin, PluginEntry};

/// Whether a setting can have any effect on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Supported,
    /// Shown, but not editable. The reason is shown in the help panel.
    Unsupported(String),
}

/// A plugin as the editor presents it.
#[derive(Debug, Clone)]
pub struct PluginView {
    pub name: String,
    pub version: String,
    pub origin: Origin,
    pub description: String,
    pub permissions: Vec<Permission>,
    /// Errors that prevent enabling it.
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub settings: Vec<SettingSpec>,
    pub platforms: Vec<String>,
}

impl PluginView {
    pub fn from_entry(e: &PluginEntry) -> PluginView {
        PluginView {
            name: e.name().to_string(),
            version: e.manifest.plugin.version.to_string(),
            origin: e.origin,
            description: e.manifest.plugin.description.clone(),
            permissions: e.manifest.plugin.permissions.clone(),
            errors: e
                .problems
                .iter()
                .filter(|p| p.is_error())
                .map(|p| p.message.clone())
                .collect(),
            warnings: e
                .problems
                .iter()
                .filter(|p| !p.is_error())
                .map(|p| p.message.clone())
                .collect(),
            settings: e.manifest.settings.clone(),
            platforms: e.manifest.plugin.platforms.clone(),
        }
    }

    pub fn from_manager(m: &Manager) -> Vec<PluginView> {
        m.entries().iter().map(PluginView::from_entry).collect()
    }
}

/// One line in the settings panel.
#[derive(Debug, Clone)]
pub enum Row {
    Setting {
        def: SettingDef,
        availability: Availability,
    },
    /// Enable/disable toggle for a plugin (index into the plugin list).
    Plugin(usize),
    /// A setting declared by a plugin.
    PluginSetting { plugin: usize, spec: SettingSpec },
    /// Read-only information.
    Info { label: String, value: String },
}

/// Build the rows of `section`.
pub fn rows_for(
    section: Section,
    caps: Result<&CapabilityReport, &str>,
    plugins: &[PluginView],
    info: &[(String, String)],
) -> Vec<Row> {
    let mut rows = Vec::new();
    for def in settings::all().into_iter().filter(|d| d.section == section) {
        // Managed through the plugin toggles below.
        if def.key == "plugins.enabled" {
            continue;
        }
        rows.push(Row::Setting {
            availability: availability(&def, caps),
            def,
        });
    }
    if section == Section::Plugins {
        for (i, p) in plugins.iter().enumerate() {
            rows.push(Row::Plugin(i));
            for spec in &p.settings {
                rows.push(Row::PluginSetting {
                    plugin: i,
                    spec: spec.clone(),
                });
            }
        }
    }
    if section == Section::Advanced {
        for (label, value) in info {
            rows.push(Row::Info {
                label: label.clone(),
                value: value.clone(),
            });
        }
    }
    rows
}

fn availability(def: &SettingDef, caps: Result<&CapabilityReport, &str>) -> Availability {
    if def.requires.is_empty() {
        return Availability::Supported;
    }
    match caps {
        Err(e) => Availability::Unsupported(format!("the power backend is unavailable: {e}")),
        Ok(report) => {
            for cap in def.requires {
                if !report.is_usable(*cap) {
                    let detail = report
                        .get(*cap)
                        .map(|c| c.detail.clone())
                        .unwrap_or_default();
                    return Availability::Unsupported(format!(
                        "{} is {} here: {detail}",
                        cap.description(),
                        report.support(*cap).as_str()
                    ));
                }
            }
            Availability::Supported
        }
    }
}

/// The user's settings table for `plugin` (not including defaults).
pub fn plugin_user_settings(config: &ConfigFile, plugin: &str) -> Map<String, Value> {
    match config.config().plugins.settings.get(plugin) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// The effective value of a plugin setting (user value or default).
pub fn plugin_value(config: &ConfigFile, plugin: &str, spec: &SettingSpec) -> Option<Value> {
    plugin_user_settings(config, plugin)
        .get(&spec.key)
        .cloned()
        .or_else(|| spec.default.clone())
}

/// Format a plugin setting value for display.
pub fn show_plugin_value(spec: &SettingSpec, value: Option<&Value>) -> String {
    match value {
        None => "(unset)".into(),
        Some(_) if spec.secret => "(set, hidden)".into(),
        Some(Value::String(s)) if s.is_empty() => "\"\"".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(m)) if m.is_empty() => "(empty table)".into(),
        Some(Value::Object(m)) => format!("{} entries", m.len()),
        Some(v) => v.to_string(),
    }
}

/// Text placed in the input line when editing a plugin setting.
pub fn plugin_edit_text(spec: &SettingSpec, value: Option<&Value>) -> String {
    if spec.secret {
        return String::new();
    }
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(_)) | None | Some(Value::Object(_)) => value
            .map(|v| show_plugin_value(spec, Some(v)))
            .unwrap_or_default(),
        Some(v) => v.to_string(),
    }
}

/// Parse user input for a plugin setting into a validated JSON value.
pub fn parse_plugin_input(spec: &SettingSpec, raw: &str) -> Result<Value> {
    let raw = raw.trim();
    let value = match spec.kind {
        SettingType::Bool => Value::Bool(
            parse_bool(raw)
                .ok_or_else(|| Error::usage(format!("\"{raw}\" is not true or false")))?,
        ),
        SettingType::Integer => Value::from(
            raw.parse::<i64>()
                .map_err(|_| Error::usage(format!("\"{raw}\" is not an integer")))?,
        ),
        SettingType::StringList => Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ),
        SettingType::Table => {
            return Err(Error::usage(
                "tables cannot be edited here; use `caffeine config edit`",
            ));
        }
        SettingType::String | SettingType::Choice | SettingType::Path => {
            Value::String(raw.to_string())
        }
    };
    spec.validate_value(&value).map_err(Error::usage)?;
    Ok(value)
}

/// Write (or with `None`, remove) one plugin setting in the configuration.
pub fn apply_plugin_setting(
    config: &mut ConfigFile,
    plugin: &str,
    key: &str,
    value: Option<Value>,
) -> Result<()> {
    let mut map = plugin_user_settings(config, plugin);
    match value {
        Some(v) => {
            map.insert(key.to_string(), v);
        }
        None => {
            map.remove(key);
        }
    }
    let doc = toml_edit::ser::to_document(&Value::Object(map))
        .map_err(|e| Error::config(format!("cannot encode plugin settings: {e}")))?;
    config.set_plugin_settings(plugin, doc.as_table().clone())
}

/// Text placed in the input line when editing a built-in setting.
pub fn setting_edit_text(config: &ConfigFile, def: &SettingDef) -> String {
    config.get(def.key).unwrap_or_default()
}

/// Whether a built-in setting currently has its default value.
pub fn is_default(config: &ConfigFile, defaults: &ConfigFile, key: &str) -> bool {
    config.get(key).ok() == defaults.get(key).ok()
}

/// The choices a setting can cycle through, if any.
pub fn choices_of(kind: &SettingKind) -> Option<Vec<String>> {
    match kind {
        SettingKind::Bool => Some(vec!["true".into(), "false".into()]),
        SettingKind::Choice(c) => Some(c.iter().map(|s| s.to_string()).collect()),
        _ => None,
    }
}

pub fn plugin_choices(spec: &SettingSpec) -> Option<Vec<String>> {
    match spec.kind {
        SettingType::Bool => Some(vec!["true".into(), "false".into()]),
        SettingType::Choice => Some(spec.choices.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caffeine_core::power::{Capability, CapabilityStatus, PlatformInfo, Support};

    fn report(unsupported: &[Capability]) -> CapabilityReport {
        CapabilityReport {
            platform: PlatformInfo {
                os: "test".into(),
                os_version: None,
                arch: "x".into(),
                backend: "fake".into(),
                init_system: None,
                desktop: None,
                notes: vec![],
            },
            capabilities: Capability::ALL
                .iter()
                .map(|c| {
                    if unsupported.contains(c) {
                        CapabilityStatus::unsupported(*c, "not here")
                    } else {
                        CapabilityStatus::new(*c, Support::Supported, Some("m"), "ok")
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn unsupported_settings_are_marked() {
        let r = report(&[Capability::LidSessionOverride]);
        let rows = rows_for(Section::Power, Ok(&r), &[], &[]);
        let lid = rows
            .iter()
            .find_map(|row| match row {
                Row::Setting { def, availability } if def.key == "power.lid_session" => {
                    Some(availability.clone())
                }
                _ => None,
            })
            .unwrap();
        assert!(matches!(lid, Availability::Unsupported(ref why) if why.contains("not here")));
        let rows = rows_for(Section::Power, Err("boom"), &[], &[]);
        assert!(rows.iter().all(|row| matches!(
            row,
            Row::Setting {
                availability: Availability::Unsupported(_),
                ..
            }
        )));
    }

    #[test]
    fn plugin_inputs_are_typed_and_validated() {
        let spec: SettingSpec = serde_json::from_value(serde_json::json!({
            "key": "jobs", "type": "integer", "description": "d", "min": 1, "max": 8
        }))
        .unwrap();
        assert_eq!(parse_plugin_input(&spec, " 4 ").unwrap(), Value::from(4));
        assert!(parse_plugin_input(&spec, "9").is_err());
        assert!(parse_plugin_input(&spec, "x").is_err());
    }

    #[test]
    fn plugin_settings_round_trip_through_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = ConfigFile::defaults(&tmp.path().join("c.toml"));
        apply_plugin_setting(&mut cfg, "hello", "greeting", Some(Value::from("Yo"))).unwrap();
        apply_plugin_setting(&mut cfg, "hello", "times", Some(Value::from(3))).unwrap();
        assert_eq!(plugin_user_settings(&cfg, "hello")["greeting"], "Yo");
        assert!(cfg.to_text().contains("[plugins.settings.hello]"));
        apply_plugin_setting(&mut cfg, "hello", "greeting", None).unwrap();
        apply_plugin_setting(&mut cfg, "hello", "times", None).unwrap();
        assert!(plugin_user_settings(&cfg, "hello").is_empty());
    }
}
