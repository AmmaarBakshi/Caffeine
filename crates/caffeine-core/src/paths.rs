//! Platform-appropriate file locations.
//!
//! | Purpose        | Linux                              | macOS                                   | Windows                          |
//! |----------------|------------------------------------|-----------------------------------------|----------------------------------|
//! | config         | `$XDG_CONFIG_HOME/caffeine`        | `~/Library/Application Support/caffeine`| `%APPDATA%\caffeine`             |
//! | data (durable) | `$XDG_DATA_HOME/caffeine`          | `~/Library/Application Support/caffeine`| `%LOCALAPPDATA%\caffeine`        |
//! | runtime        | `$XDG_RUNTIME_DIR/caffeine`        | `<data>/run`                            | `<data>\run`                     |
//! | logs           | `$XDG_STATE_HOME/caffeine`         | `~/Library/Logs/caffeine`               | `<data>\logs`                    |
//!
//! Durable data (plugin trust records, the lid-policy restore journal, the
//! last-session record) is kept apart from runtime state (the live session
//! file and lock) so that a reboot clears transient state but never loses the
//! information needed to restore a modified power setting.
//!
//! Environment overrides:
//!
//! * `CAFFEINE_HOME` relocates *everything* under one directory
//!   (`config/`, `data/`, `run/`, `logs/`). Used by tests and portable setups.
//! * `CAFFEINE_CONFIG` points at a specific configuration file.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const ENV_HOME: &str = "CAFFEINE_HOME";
pub const ENV_CONFIG: &str = "CAFFEINE_CONFIG";

/// Resolved directory layout.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    pub plugin_dir: PathBuf,
}

impl Paths {
    /// Resolve paths from the environment and platform conventions.
    pub fn resolve() -> Result<Paths> {
        let home_override = std::env::var_os(ENV_HOME).filter(|v| !v.is_empty());
        let mut paths = match home_override {
            Some(root) => Paths::under(Path::new(&root)),
            None => Paths::platform()?,
        };
        if let Some(cfg) = std::env::var_os(ENV_CONFIG).filter(|v| !v.is_empty()) {
            paths.config_file = PathBuf::from(cfg);
        }
        Ok(paths)
    }

    /// A self-contained layout rooted at `root`.
    pub fn under(root: &Path) -> Paths {
        let config_dir = root.join("config");
        let data_dir = root.join("data");
        Paths {
            config_file: config_dir.join(CONFIG_FILE_NAME),
            plugin_dir: data_dir.join("plugins"),
            runtime_dir: root.join("run"),
            log_dir: root.join("logs"),
            config_dir,
            data_dir,
        }
    }

    fn platform() -> Result<Paths> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            Error::failure("cannot determine the home directory")
                .with_hint("set CAFFEINE_HOME to a writable directory")
        })?;
        let config_dir = base.config_dir().join("caffeine");
        let data_dir = base.data_local_dir().join("caffeine");
        let runtime_dir = base
            .runtime_dir()
            .map(|d| d.join("caffeine"))
            .unwrap_or_else(|| data_dir.join("run"));
        let log_dir = if cfg!(target_os = "macos") {
            base.home_dir()
                .join("Library")
                .join("Logs")
                .join("caffeine")
        } else {
            base.state_dir()
                .map(|d| d.join("caffeine"))
                .unwrap_or_else(|| data_dir.join("logs"))
        };
        Ok(Paths {
            config_file: config_dir.join(CONFIG_FILE_NAME),
            plugin_dir: data_dir.join("plugins"),
            config_dir,
            data_dir,
            runtime_dir,
            log_dir,
        })
    }

    /// Replace the configuration file location (e.g. from `--config-file`).
    pub fn with_config_file(mut self, file: PathBuf) -> Paths {
        self.config_file = file;
        self
    }

    /// Default log file used by background session hosts.
    pub fn log_file(&self) -> PathBuf {
        self.log_dir.join("caffeine.log")
    }

    /// Create a directory (and parents) if needed, restricting permissions to
    /// the current user on Unix.
    pub fn ensure_dir(dir: &Path) -> Result<()> {
        if dir.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_root_is_self_contained() {
        let root = Path::new("/tmp/caffeine-test");
        let p = Paths::under(root);
        for dir in [
            &p.config_dir,
            &p.data_dir,
            &p.runtime_dir,
            &p.log_dir,
            &p.plugin_dir,
        ] {
            assert!(dir.starts_with(root));
        }
        assert_eq!(p.config_file.file_name().unwrap(), CONFIG_FILE_NAME);
        assert_ne!(
            p.runtime_dir, p.data_dir,
            "runtime state must be separate from durable data"
        );
    }

    #[test]
    fn ensure_dir_creates_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        Paths::ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
        Paths::ensure_dir(&nested).unwrap();
    }
}
