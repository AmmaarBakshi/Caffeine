//! A small `log` implementation: stderr for interactive commands, an
//! append-only file (rotated at 1 MiB) for background session hosts.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct Logger {
    level: LevelFilter,
    file: Option<Mutex<File>>,
    stderr: bool,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Dependencies (zbus, ...) only log at warn and above unless tracing.
        let ours = record.target().starts_with("caffeine");
        if !ours && record.level() > log::Level::Warn && self.level < LevelFilter::Trace {
            return;
        }
        if self.stderr {
            eprintln!(
                "caffeine: {}: {}",
                record.level().as_str().to_ascii_lowercase(),
                record.args()
            );
        }
        if let Some(file) = &self.file
            && let Ok(mut f) = file.lock()
        {
            let _ = writeln!(
                f,
                "{} [{}] pid={} {}",
                caffeine_core::time::format_rfc3339(caffeine_core::time::unix_now()),
                record.level(),
                std::process::id(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        if let Some(file) = &self.file
            && let Ok(mut f) = file.lock()
        {
            let _ = f.flush();
        }
    }
}

/// Install the logger for an interactive command.
pub fn init_stderr(level: LevelFilter) {
    let _ = log::set_boxed_logger(Box::new(Logger {
        level,
        file: None,
        stderr: true,
    }));
    log::set_max_level(level);
}

/// Install the logger for a background host. Returns the log path in use.
pub fn init_file(level: LevelFilter, path: &Path) -> Option<PathBuf> {
    let file = open_rotating(path);
    let used = file.as_ref().map(|_| path.to_path_buf());
    let _ = log::set_boxed_logger(Box::new(Logger {
        level,
        file: file.map(Mutex::new),
        stderr: false,
    }));
    log::set_max_level(level);
    used
}

fn open_rotating(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        caffeine_core::Paths::ensure_dir(dir).ok()?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let mut rotated = path.as_os_str().to_os_string();
        rotated.push(".1");
        let _ = std::fs::rename(path, rotated);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}
