use super::*;
use std::time::Duration;

fn tmp_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    (dir, path)
}

#[test]
fn template_parses_to_defaults() {
    let (_d, path) = tmp_path();
    let cfg = ConfigFile::from_text(&path, &template()).unwrap();
    assert_eq!(cfg.config(), &Config::default());
    assert!(cfg.notes().is_empty(), "{:?}", cfg.notes());
    assert!(!cfg.is_dirty());
}

#[test]
fn missing_file_yields_defaults_without_writing() {
    let (_d, path) = tmp_path();
    let cfg = ConfigFile::load(&path).unwrap();
    assert_eq!(cfg.config(), &Config::default());
    assert!(!cfg.exists_on_disk());
    assert!(!path.exists(), "loading must not create the file");
}

#[test]
fn partial_file_fills_in_defaults() {
    let (_d, path) = tmp_path();
    let cfg = ConfigFile::from_text(
        &path,
        "schema_version = 1\n[power]\ndisplay = \"keep-on\"\n",
    )
    .unwrap();
    assert_eq!(cfg.config().power.display, DisplayPolicy::KeepOn);
    assert!(cfg.config().power.prevent_idle_sleep);
}

#[test]
fn malformed_toml_is_a_config_error() {
    let (_d, path) = tmp_path();
    let err = ConfigFile::from_text(&path, "[power\ndisplay = 1").unwrap_err();
    assert_eq!(err.kind(), crate::ErrorKind::Config);
}

#[test]
fn wrong_types_and_semantic_errors_are_rejected() {
    let (_d, path) = tmp_path();
    assert!(ConfigFile::from_text(&path, "[power]\ndisplay = \"bright\"\n").is_err());
    assert!(ConfigFile::from_text(&path, "[power]\nbattery_threshold = 101\n").is_err());
    let err = ConfigFile::from_text(
        &path,
        "[session]\ndefault_duration = \"3h\"\nmax_duration = \"1h\"\n",
    )
    .unwrap_err();
    assert!(err.message().contains("exceeds"), "{err}");
}

#[test]
fn unversioned_file_is_migrated_in_memory_only() {
    let (_d, path) = tmp_path();
    std::fs::write(&path, "[general]\nlog_level = \"info\"\n").unwrap();
    let cfg = ConfigFile::load(&path).unwrap();
    assert!(cfg.is_dirty());
    assert!(cfg.to_text().contains("schema_version = 1"));
    assert!(cfg.notes().iter().any(|n| n.contains("schema_version")));
    // Nothing is written until the caller saves.
    assert!(!std::fs::read_to_string(&path).unwrap().contains("schema_version"));
}

#[test]
fn newer_schema_is_read_only() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::from_text(&path, "schema_version = 99\n").unwrap();
    assert!(cfg.read_only_reason().is_some());
    assert!(cfg.set("power.display", "keep-on").is_err());
    assert!(cfg.save().is_err());
}

#[test]
fn unknown_keys_are_warnings_not_errors() {
    let (_d, path) = tmp_path();
    let cfg = ConfigFile::from_text(
        &path,
        "schema_version = 1\nfuture = 1\n[power]\nturbo = true\n[plugins.settings.build]\nanything = 1\n",
    )
    .unwrap();
    let notes = cfg.notes().join("\n");
    assert!(notes.contains("\"future\""));
    assert!(notes.contains("\"power.turbo\""));
    assert!(!notes.contains("anything"));
}

#[test]
fn set_preserves_comments_and_validates() {
    let (_d, path) = tmp_path();
    let text = "schema_version = 1\n\n# my note\n[power]\n# keep the screen on\ndisplay = \"allow-sleep\" # inline\n";
    let mut cfg = ConfigFile::from_text(&path, text).unwrap();
    cfg.set("power.display", "keep-on").unwrap();
    let out = cfg.to_text();
    assert!(out.contains("# my note"));
    assert!(out.contains("# keep the screen on"));
    assert!(out.contains("display = \"keep-on\" # inline"), "{out}");
    assert_eq!(cfg.config().power.display, DisplayPolicy::KeepOn);

    // Invalid values leave the document untouched.
    let before = cfg.to_text();
    assert!(cfg.set("power.display", "dim").is_err());
    assert!(cfg.set("power.battery_threshold", "300").is_err());
    assert!(cfg.set("nope.nothing", "1").is_err());
    assert_eq!(cfg.to_text(), before);
}

#[test]
fn set_creates_missing_tables_and_cross_validates() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::from_text(&path, "schema_version = 1\n").unwrap();
    cfg.set("session.max_duration", "1h").unwrap();
    assert!(cfg.set("session.default_duration", "2h").is_err());
    cfg.set("session.default_duration", "30m").unwrap();
    assert_eq!(
        cfg.config().session.default_duration,
        SessionLength::Fixed(Duration::from_secs(1_800))
    );
    assert_eq!(cfg.get("session.default_duration").unwrap(), "30m");
    cfg.unset("session.default_duration").unwrap();
    assert_eq!(cfg.get("session.default_duration").unwrap(), "indefinite");
}

#[test]
fn lists_and_paths() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::from_text(&path, "").unwrap();
    cfg.set("advanced.experimental", "session-lid-override").unwrap();
    assert!(cfg.config().advanced.experimental_enabled("session-lid-override"));
    assert!(cfg.set("advanced.experimental", "warp").is_err());
    assert!(cfg.set("advanced.log_file", "relative.log").is_err());
    cfg.set("advanced.log_file", "").unwrap();
    assert_eq!(cfg.config().advanced.log_file, None);
}

#[test]
fn save_is_atomic_and_detects_external_changes() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::load(&path).unwrap();
    cfg.set("general.log_level", "debug").unwrap();
    cfg.save().unwrap();
    assert!(!cfg.is_dirty());
    let reloaded = ConfigFile::load(&path).unwrap();
    assert_eq!(reloaded.config().general.log_level, LogLevel::Debug);

    // Another program edits the file; our stale copy must not clobber it.
    std::fs::write(&path, "schema_version = 1\n[general]\nlog_level = \"trace\"\n").unwrap();
    cfg.set("general.log_level", "error").unwrap();
    let err = cfg.save().unwrap_err();
    assert_eq!(err.kind(), crate::ErrorKind::Conflict);
    assert!(std::fs::read_to_string(&path).unwrap().contains("trace"));
}

#[test]
fn second_save_keeps_backup() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::load(&path).unwrap();
    cfg.set("general.color", "never").unwrap();
    cfg.save().unwrap();
    cfg.set("general.color", "always").unwrap();
    cfg.save().unwrap();
    let bak = std::fs::read_to_string(fsutil::backup_path(&path)).unwrap();
    assert!(bak.contains("color = \"never\""));
}

#[test]
fn load_or_default_never_overwrites_broken_file() {
    let (_d, path) = tmp_path();
    std::fs::write(&path, "this is = = not toml").unwrap();
    let (mut cfg, err) = ConfigFile::load_or_default(&path);
    assert!(err.is_some());
    assert_eq!(cfg.config(), &Config::default());
    assert!(cfg.save().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is = = not toml");
}

#[test]
fn reset_backs_up_existing_file() {
    let (_d, path) = tmp_path();
    std::fs::write(&path, "broken = = =").unwrap();
    let backup = ConfigFile::reset(&path).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(backup).unwrap(), "broken = = =");
    assert_eq!(ConfigFile::load(&path).unwrap().config(), &Config::default());
}

#[test]
fn effective_json_redacts_secrets() {
    let mut config = Config::default();
    config.plugins.settings.insert(
        "deploy".into(),
        serde_json::json!({"api_token": "abc", "region": "eu", "nested": {"password": "x"}, "custom": "s3cret"}),
    );
    let json = effective_json(&config, &["plugins.settings.deploy.custom".to_string()]);
    let deploy = &json["plugins"]["settings"]["deploy"];
    assert_eq!(deploy["api_token"], "<redacted>");
    assert_eq!(deploy["nested"]["password"], "<redacted>");
    assert_eq!(deploy["custom"], "<redacted>");
    assert_eq!(deploy["region"], "eu");
}

#[test]
fn plugin_settings_round_trip() {
    let (_d, path) = tmp_path();
    let mut cfg = ConfigFile::from_text(&path, "schema_version = 1\n").unwrap();
    let mut table = Table::new();
    table.insert("greeting", toml_edit::value("hi"));
    cfg.set_plugin_settings("hello", table).unwrap();
    assert_eq!(cfg.config().plugins.settings["hello"]["greeting"], "hi");
    assert!(cfg.to_text().contains("[plugins.settings.hello]"));
}
