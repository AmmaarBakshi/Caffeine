//! Editor state and key handling, independent of the terminal.

use std::collections::BTreeSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use caffeine_core::config::{ConfigFile, Section};
use caffeine_core::power::CapabilityReport;
use caffeine_core::{Error, Result};
use caffeine_plugin_manager::{Manager, TrustStatus};

use crate::rows::{
    Availability, PluginView, Row, apply_plugin_setting, choices_of, parse_plugin_input,
    plugin_choices, plugin_edit_text, plugin_value, rows_for, setting_edit_text,
};

/// Everything the editor needs.
#[derive(Debug)]
pub struct EditorInput {
    pub config: ConfigFile,
    /// The platform's capabilities, or why they could not be determined.
    pub capabilities: std::result::Result<CapabilityReport, String>,
    /// Plugin discovery, or why it failed.
    pub plugins: std::result::Result<Manager, String>,
    /// Read-only facts shown in the Advanced section (paths, ...).
    pub info: Vec<(String, String)>,
    pub version: String,
}

/// How the editor ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Changes were written (possibly several times).
    Saved,
    /// Nothing was changed.
    Unchanged,
    /// The user quit without saving changes.
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing a value for the selected row.
    Edit {
        buffer: String,
        cursor: usize,
        masked: bool,
    },
    /// Choosing one of several values for the selected row.
    Pick {
        options: Vec<String>,
        index: usize,
    },
    /// Reviewing a third-party plugin's permissions before enabling it.
    ConfirmTrust {
        plugin: usize,
    },
    /// Quitting with unsaved changes.
    ConfirmExit,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Error,
}

#[derive(Debug)]
pub struct App {
    pub config: ConfigFile,
    pub defaults: ConfigFile,
    pub caps: std::result::Result<CapabilityReport, String>,
    manager: std::result::Result<Manager, String>,
    pub plugins: Vec<PluginView>,
    pub trust: Vec<TrustStatus>,
    /// `plugins.enabled` as last saved.
    saved_enabled: Vec<String>,
    /// Third-party plugins the user agreed to trust in this session.
    trusted_now: BTreeSet<String>,
    pub info: Vec<(String, String)>,
    pub version: String,
    pub section: usize,
    pub row: usize,
    pub rows: Vec<Row>,
    pub mode: Mode,
    pub status: Option<(StatusKind, String)>,
    saved: bool,
    pub outcome: Option<Outcome>,
}

impl App {
    pub fn new(input: EditorInput) -> App {
        let defaults = ConfigFile::defaults(input.config.path());
        let (plugins, trust) = match &input.plugins {
            Ok(m) => (
                PluginView::from_manager(m),
                m.entries().iter().map(|e| e.trust).collect(),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };
        let mut app = App {
            saved_enabled: input.config.config().plugins.enabled.clone(),
            config: input.config,
            defaults,
            caps: input.capabilities,
            manager: input.plugins,
            plugins,
            trust,
            trusted_now: BTreeSet::new(),
            info: input.info,
            version: input.version,
            section: 0,
            row: 0,
            rows: Vec::new(),
            mode: Mode::Normal,
            status: None,
            saved: false,
            outcome: None,
        };
        if let Some(reason) = app.config.read_only_reason() {
            app.status = Some((StatusKind::Error, format!("read-only: {reason}")));
        } else if app.config.is_dirty() {
            app.status = Some((
                StatusKind::Info,
                "the file was migrated to the current schema; save to keep it".into(),
            ));
        }
        app.rebuild();
        app
    }

    pub fn section(&self) -> Section {
        Section::ALL[self.section]
    }

    pub fn plugin_error(&self) -> Option<&str> {
        self.manager.as_ref().err().map(String::as_str)
    }

    pub fn is_dirty(&self) -> bool {
        self.config.is_dirty()
    }

    pub fn is_enabled(&self, plugin: &str) -> bool {
        self.config
            .config()
            .plugins
            .enabled
            .iter()
            .any(|n| n == plugin)
    }

    fn rebuild(&mut self) {
        let caps = self.caps.as_ref().map_err(String::as_str);
        self.rows = rows_for(self.section(), caps, &self.plugins, &self.info);
        if self.row >= self.rows.len() {
            self.row = self.rows.len().saturating_sub(1);
        }
    }

    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.row)
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = Some((StatusKind::Info, text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Some((StatusKind::Error, text.into()));
    }

    /// Handle one key press.
    pub fn handle(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.mode.clone() {
            Mode::Help => self.mode = Mode::Normal,
            Mode::ConfirmExit => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    if self.save() {
                        self.outcome = Some(Outcome::Saved);
                    } else {
                        self.mode = Mode::Normal;
                    }
                }
                KeyCode::Char('n' | 'N') => self.outcome = Some(Outcome::Discarded),
                KeyCode::Esc | KeyCode::Char('c' | 'C') => {
                    self.mode = Mode::Normal;
                    self.info("cancelled");
                }
                _ => {}
            },
            Mode::ConfirmTrust { plugin } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.mode = Mode::Normal;
                    let name = self.plugins[plugin].name.clone();
                    self.trusted_now.insert(name.clone());
                    self.set_enabled(&name, true);
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    self.info("not enabled");
                }
                _ => {}
            },
            Mode::Pick { options, index } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.mode = Mode::Pick {
                        index: index.checked_sub(1).unwrap_or(options.len() - 1),
                        options,
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.mode = Mode::Pick {
                        index: (index + 1) % options.len(),
                        options,
                    }
                }
                KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    let value = options[index].clone();
                    self.apply(&value);
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::Edit {
                mut buffer,
                mut cursor,
                masked,
            } => {
                match key.code {
                    KeyCode::Esc => {
                        self.mode = Mode::Normal;
                        self.info("cancelled");
                        return;
                    }
                    KeyCode::Enter => {
                        if self.apply(&buffer) {
                            self.mode = Mode::Normal;
                        }
                        return;
                    }
                    KeyCode::Char('u') if ctrl => {
                        buffer.clear();
                        cursor = 0;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        buffer.insert(byte_index(&buffer, cursor), c);
                        cursor += 1;
                    }
                    KeyCode::Backspace if cursor > 0 => {
                        cursor -= 1;
                        buffer.remove(byte_index(&buffer, cursor));
                    }
                    KeyCode::Delete if cursor < buffer.chars().count() => {
                        buffer.remove(byte_index(&buffer, cursor));
                    }
                    KeyCode::Left => cursor = cursor.saturating_sub(1),
                    KeyCode::Right => cursor = (cursor + 1).min(buffer.chars().count()),
                    KeyCode::Home => cursor = 0,
                    KeyCode::End => cursor = buffer.chars().count(),
                    _ => {}
                }
                self.mode = Mode::Edit {
                    buffer,
                    cursor,
                    masked,
                };
            }
            Mode::Normal => self.handle_normal(key, ctrl),
        }
    }

    fn handle_normal(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Char('s') if ctrl => {
                self.save();
            }
            KeyCode::Char('x' | 'q' | 'c') if ctrl => self.request_exit(),
            KeyCode::Char('q') => self.request_exit(),
            KeyCode::Char('g') if ctrl => self.mode = Mode::Help,
            KeyCode::F(1) | KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Tab => self.switch_section(1),
            KeyCode::BackTab => self.switch_section(Section::ALL.len() - 1),
            KeyCode::Char(c @ '1'..='5') => {
                self.section = (c as usize) - ('1' as usize);
                self.row = 0;
                self.rebuild();
            }
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.row = (self.row + 1).min(self.rows.len().saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.row = 0,
            KeyCode::End | KeyCode::Char('G') => self.row = self.rows.len().saturating_sub(1),
            KeyCode::PageUp => self.switch_section(Section::ALL.len() - 1),
            KeyCode::PageDown => self.switch_section(1),
            KeyCode::Enter => self.activate(),
            KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => self.cycle(1),
            KeyCode::Left | KeyCode::Char('h') => self.cycle(-1),
            KeyCode::Delete | KeyCode::Char('r') => self.reset(),
            _ => {}
        }
    }

    fn switch_section(&mut self, by: usize) {
        self.section = (self.section + by) % Section::ALL.len();
        self.row = 0;
        self.rebuild();
    }

    fn request_exit(&mut self) {
        if self.is_dirty() && self.config.read_only_reason().is_none() {
            self.mode = Mode::ConfirmExit;
        } else {
            self.outcome = Some(if self.saved {
                Outcome::Saved
            } else {
                Outcome::Unchanged
            });
        }
    }

    /// Refuse edits that cannot be performed, with the reason.
    fn editable(&mut self) -> bool {
        if let Some(reason) = self.config.read_only_reason() {
            let reason = reason.to_string();
            self.error(format!("read-only: {reason}"));
            return false;
        }
        match self.current().cloned() {
            Some(Row::Setting {
                def,
                availability: Availability::Unsupported(why),
            }) => {
                self.error(format!(
                    "{} is not supported on this machine: {why}",
                    def.label
                ));
                false
            }
            Some(Row::Info { label, .. }) => {
                self.info(format!(
                    "{label} is informational and cannot be changed here"
                ));
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// Enter: toggle, pick or start typing, depending on the row.
    fn activate(&mut self) {
        if !self.editable() {
            return;
        }
        match self.current().cloned() {
            Some(Row::Setting { def, .. }) => match choices_of(&def.kind) {
                Some(options) if options.len() == 2 && options[0] == "true" => self.cycle(1),
                Some(options) => {
                    let current = self.config.get(def.key).unwrap_or_default();
                    let index = options.iter().position(|o| *o == current).unwrap_or(0);
                    self.mode = Mode::Pick { options, index };
                }
                None => {
                    let buffer = setting_edit_text(&self.config, &def);
                    self.mode = Mode::Edit {
                        cursor: buffer.chars().count(),
                        buffer,
                        masked: false,
                    };
                }
            },
            Some(Row::Plugin(_)) => self.cycle(1),
            Some(Row::PluginSetting { plugin, spec }) => {
                if spec.kind == caffeine_plugin_api::manifest::SettingType::Table {
                    self.info(format!(
                        "{} is a table; edit it with `caffeine config edit` ([plugins.settings.{}.{}])",
                        spec.key, self.plugins[plugin].name, spec.key
                    ));
                    return;
                }
                match plugin_choices(&spec) {
                    Some(_) if spec.kind == caffeine_plugin_api::manifest::SettingType::Bool => {
                        self.cycle(1)
                    }
                    Some(options) => {
                        let current = plugin_value(&self.config, &self.plugins[plugin].name, &spec)
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default();
                        let index = options.iter().position(|o| *o == current).unwrap_or(0);
                        self.mode = Mode::Pick { options, index };
                    }
                    None => {
                        let value = plugin_value(&self.config, &self.plugins[plugin].name, &spec);
                        let buffer = plugin_edit_text(&spec, value.as_ref());
                        self.mode = Mode::Edit {
                            cursor: buffer.chars().count(),
                            buffer,
                            masked: spec.secret,
                        };
                    }
                }
            }
            Some(Row::Info { .. }) | None => {}
        }
    }

    /// Space/Left/Right: step through the values of a toggle, choice or
    /// plugin switch. Other rows are left alone.
    fn cycle(&mut self, step: isize) {
        let row = self.current().cloned();
        let (options, current) = match &row {
            Some(Row::Setting { def, .. }) => match choices_of(&def.kind) {
                Some(o) => (o, self.config.get(def.key).unwrap_or_default()),
                None => return,
            },
            Some(Row::PluginSetting { plugin, spec }) => match plugin_choices(spec) {
                Some(o) => {
                    let v = plugin_value(&self.config, &self.plugins[*plugin].name, spec);
                    let current = match v {
                        Some(serde_json::Value::String(s)) => s,
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    (o, current)
                }
                None => return,
            },
            Some(Row::Plugin(i)) => {
                if !self.editable() {
                    return;
                }
                self.toggle_plugin(*i);
                return;
            }
            _ => return,
        };
        if !self.editable() {
            return;
        }
        let pos = options.iter().position(|o| *o == current).unwrap_or(0) as isize;
        let next = (pos + step).rem_euclid(options.len() as isize) as usize;
        let value = options[next].clone();
        self.apply(&value);
    }

    fn toggle_plugin(&mut self, i: usize) {
        let view = self.plugins[i].clone();
        if self.is_enabled(&view.name) {
            self.trusted_now.remove(&view.name);
            self.set_enabled(&view.name, false);
            return;
        }
        if let Some(err) = view.errors.first() {
            self.error(format!("{} cannot be enabled: {err}", view.name));
            return;
        }
        if view.origin.is_third_party() {
            self.mode = Mode::ConfirmTrust { plugin: i };
        } else {
            self.set_enabled(&view.name, true);
        }
    }

    fn set_enabled(&mut self, name: &str, on: bool) {
        let mut list = self.config.config().plugins.enabled.clone();
        list.retain(|n| n != name);
        if on {
            list.push(name.to_string());
        }
        match self.config.set("plugins.enabled", &list.join(",")) {
            Ok(()) => self.info(format!(
                "{name} will be {} when you save",
                if on { "enabled" } else { "disabled" }
            )),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Apply a textual value to the selected row. Returns whether it was
    /// accepted.
    fn apply(&mut self, raw: &str) -> bool {
        let result = match self.current().cloned() {
            Some(Row::Setting { def, .. }) => self.config.set(def.key, raw).map(|()| {
                format!(
                    "{} = {}",
                    def.key,
                    self.config.get(def.key).unwrap_or_default()
                )
            }),
            Some(Row::PluginSetting { plugin, spec }) => {
                let name = self.plugins[plugin].name.clone();
                parse_plugin_input(&spec, raw).and_then(|v| {
                    apply_plugin_setting(&mut self.config, &name, &spec.key, Some(v))
                        .map(|()| format!("plugins.settings.{name}.{} updated", spec.key))
                })
            }
            _ => return false,
        };
        match result {
            Ok(msg) => {
                self.info(msg);
                true
            }
            Err(e) => {
                self.error(e.to_string());
                false
            }
        }
    }

    /// Reset the selected row to its default.
    fn reset(&mut self) {
        if self.config.read_only_reason().is_some() {
            self.editable();
            return;
        }
        let result = match self.current().cloned() {
            Some(Row::Setting { def, .. }) => self.config.unset(def.key).map(|()| {
                format!(
                    "{} reset to {}",
                    def.key,
                    self.config.get(def.key).unwrap_or_default()
                )
            }),
            Some(Row::PluginSetting { plugin, spec }) => {
                let name = self.plugins[plugin].name.clone();
                apply_plugin_setting(&mut self.config, &name, &spec.key, None)
                    .map(|()| format!("plugins.settings.{name}.{} reset to its default", spec.key))
            }
            _ => return,
        };
        match result {
            Ok(m) => self.info(m),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Record plugin trust changes, then write the file. Returns success.
    pub fn save(&mut self) -> bool {
        match self.try_save() {
            Ok(Some(msg)) => {
                self.saved = true;
                self.info(msg);
                true
            }
            Ok(None) => {
                self.info("no changes to save");
                true
            }
            Err(e) => {
                let hint = e.hint().map(|h| format!(" ({h})")).unwrap_or_default();
                self.error(format!("not saved: {e}{hint}"));
                false
            }
        }
    }

    fn try_save(&mut self) -> Result<Option<String>> {
        if let Some(reason) = self.config.read_only_reason() {
            return Err(Error::config(format!("read-only: {reason}")));
        }
        if !self.is_dirty() {
            return Ok(None);
        }
        let desired = self.config.config().plugins.enabled.clone();
        let newly: Vec<String> = desired
            .iter()
            .filter(|n| !self.saved_enabled.contains(n) || self.trusted_now.contains(*n))
            .cloned()
            .collect();
        let removed: Vec<String> = self
            .saved_enabled
            .iter()
            .filter(|n| !desired.contains(n))
            .cloned()
            .collect();
        if !newly.is_empty() || !removed.is_empty() {
            let manager = self
                .manager
                .as_mut()
                .map_err(|e| Error::plugin(format!("plugins are unavailable: {e}")))?;
            for name in &removed {
                if manager.get(name).is_some() {
                    manager.disable(name)?;
                }
            }
            for name in &newly {
                manager.enable(name)?;
            }
            self.trust = manager.entries().iter().map(|e| e.trust).collect();
        }
        self.config.save()?;
        self.saved_enabled = desired;
        self.trusted_now.clear();
        Ok(Some(format!("saved {}", self.config.path().display())))
    }
}

/// Byte offset of the `chars`-th character.
fn byte_index(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(i, _)| i)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub(crate) fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub(crate) fn app_with(path: &std::path::Path, text: Option<&str>) -> App {
        if let Some(t) = text {
            std::fs::write(path, t).unwrap();
        }
        let config = ConfigFile::load(path).unwrap();
        App::new(EditorInput {
            config,
            capabilities: Err("no backend in tests".into()),
            plugins: Err("no plugins in tests".into()),
            info: vec![("Configuration file".into(), path.display().to_string())],
            version: "0.0.0".into(),
        })
    }

    fn select(app: &mut App, key_name: &str) {
        let idx = app
            .rows
            .iter()
            .position(|r| matches!(r, Row::Setting { def, .. } if def.key == key_name))
            .unwrap_or_else(|| panic!("{key_name} not in section"));
        app.row = idx;
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn edit_toggle_pick_and_save() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let mut app = app_with(&path, Some("# my comment\nschema_version = 1\n"));
        assert_eq!(app.section(), Section::General);

        select(&mut app, "general.safety_warnings");
        app.handle(key(KeyCode::Enter));
        assert_eq!(app.config.get("general.safety_warnings").unwrap(), "false");

        select(&mut app, "general.log_level");
        app.handle(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Pick { .. }));
        // warn -> info
        app.handle(key(KeyCode::Down));
        app.handle(key(KeyCode::Enter));
        assert_eq!(app.config.get("general.log_level").unwrap(), "info");

        app.handle(key(KeyCode::Tab));
        app.handle(key(KeyCode::Tab));
        assert_eq!(app.section(), Section::Sessions);
        select(&mut app, "session.default_duration");
        app.handle(key(KeyCode::Enter));
        app.handle(ctrl('u'));
        type_text(&mut app, "banana");
        app.handle(key(KeyCode::Enter));
        // Rejected: still editing, with an error.
        assert!(matches!(app.mode, Mode::Edit { .. }));
        assert_eq!(app.status.as_ref().unwrap().0, StatusKind::Error);
        app.handle(ctrl('u'));
        type_text(&mut app, "1h30m");
        app.handle(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.config.get("session.default_duration").unwrap(), "1h30m");

        app.handle(ctrl('s'));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my comment"), "comments are preserved");
        assert!(text.contains("1h30m"));
        assert!(!app.is_dirty());
        app.handle(ctrl('x'));
        assert_eq!(app.outcome, Some(Outcome::Saved));
    }

    #[test]
    fn unsupported_settings_cannot_be_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with(&tmp.path().join("c.toml"), None);
        app.handle(key(KeyCode::Char('2')));
        assert_eq!(app.section(), Section::Power);
        select(&mut app, "power.prevent_idle_sleep");
        app.handle(key(KeyCode::Enter));
        assert_eq!(app.config.get("power.prevent_idle_sleep").unwrap(), "true");
        let (kind, msg) = app.status.clone().unwrap();
        assert_eq!(kind, StatusKind::Error);
        assert!(msg.contains("not supported"), "{msg}");
        assert!(!app.is_dirty());
    }

    #[test]
    fn exit_with_unsaved_changes_asks_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c.toml");
        let mut app = app_with(&path, None);
        select(&mut app, "general.safety_warnings");
        app.handle(key(KeyCode::Char(' ')));
        app.handle(ctrl('x'));
        assert_eq!(app.mode, Mode::ConfirmExit);
        app.handle(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.outcome.is_none());
        app.handle(key(KeyCode::Char('q')));
        app.handle(key(KeyCode::Char('n')));
        assert_eq!(app.outcome, Some(Outcome::Discarded));
        assert!(!path.exists(), "nothing was written");
    }

    #[test]
    fn save_refuses_to_overwrite_external_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c.toml");
        let mut app = app_with(&path, Some("schema_version = 1\n"));
        select(&mut app, "general.safety_warnings");
        app.handle(key(KeyCode::Enter));
        std::fs::write(&path, "schema_version = 1\n# edited elsewhere\n").unwrap();
        assert!(!app.save());
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .1
                .contains("modified by another program")
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("edited elsewhere")
        );
    }

    #[test]
    fn reset_and_text_editing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with(&tmp.path().join("c.toml"), None);
        app.handle(key(KeyCode::Char('3')));
        select(&mut app, "session.stop_timeout_secs");
        app.handle(key(KeyCode::Enter));
        // "10": Home, Delete -> "0"; End, "5" -> "05", which parses as 5.
        app.handle(key(KeyCode::Home));
        app.handle(key(KeyCode::Delete));
        app.handle(key(KeyCode::End));
        type_text(&mut app, "5");
        app.handle(key(KeyCode::Enter));
        assert_eq!(app.config.get("session.stop_timeout_secs").unwrap(), "5");
        app.handle(key(KeyCode::Char('r')));
        assert_eq!(app.config.get("session.stop_timeout_secs").unwrap(), "10");
    }
}
