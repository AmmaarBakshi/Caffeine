//! Runtime session state on disk.
//!
//! Layout inside the runtime directory:
//!
//! * `session.lock` — held with an exclusive OS file lock by the process that
//!   owns the active session (the *session host*). The lock is released by
//!   the kernel when the host exits for any reason, including `kill -9`, so
//!   "can I take the lock?" is a reliable liveness test that is immune to PID
//!   reuse.
//! * `session.json` — description of the active session.
//! * `start.lock` — serializes concurrent `caffeine start` invocations.
//! * `stop-request` — written by `caffeine stop`; contains the session id.
//!
//! The durable data directory holds `last-session.json`, a record of how the
//! previous session ended. Runtime files never encode persistent power policy.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::BatteryAction;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::paths::Paths;
use crate::power::{ActiveAssertion, AwakeRequest};

pub const STATE_FORMAT: u32 = 1;

/// How the session host was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SessionMode {
    /// Detached host started by `caffeine start`.
    Background,
    /// `caffeine start --foreground`, held by the invoking terminal.
    Foreground,
    /// `caffeine run -- <program>`: lasts while the program runs.
    Command { program: String },
}

impl SessionMode {
    pub fn describe(&self) -> String {
        match self {
            SessionMode::Background => "background".into(),
            SessionMode::Foreground => "foreground".into(),
            SessionMode::Command { program } => format!("while `{program}` runs"),
        }
    }
}

/// Battery policy captured when the session started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryWatch {
    pub on_battery: BatteryAction,
    /// 0 disables the threshold.
    pub threshold: u8,
}

impl Default for BatteryWatch {
    fn default() -> Self {
        BatteryWatch {
            on_battery: BatteryAction::Continue,
            threshold: 0,
        }
    }
}

impl BatteryWatch {
    pub fn is_active(&self) -> bool {
        self.threshold > 0 || self.on_battery == BatteryAction::Stop
    }
}

/// The active session, as written to `session.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub format: u32,
    pub id: String,
    pub pid: u32,
    pub mode: SessionMode,
    pub backend: String,
    pub started_at: u64,
    pub expires_at: Option<u64>,
    pub request: AwakeRequest,
    pub assertions: Vec<ActiveAssertion>,
    pub battery: BatteryWatch,
    pub heartbeat_at: u64,
    /// Number of suspected resumes from sleep observed by the host.
    pub resumes: u32,
    pub last_resume_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<PathBuf>,
}

impl SessionState {
    /// Seconds remaining, or `None` for indefinite sessions.
    pub fn remaining(&self, now: u64) -> Option<u64> {
        self.expires_at.map(|e| e.saturating_sub(now))
    }
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum EndReason {
    /// `caffeine stop` or Ctrl+C in a foreground session.
    Stopped,
    /// The configured duration elapsed.
    Expired,
    /// Battery charge fell below the threshold.
    BatteryThreshold { percent: u8, threshold: u8 },
    /// The machine switched to battery and policy said to stop.
    SwitchedToBattery,
    /// The host received a termination signal.
    Terminated,
    /// The wrapped command exited (`caffeine run`).
    CommandExited { code: Option<i32> },
}

impl EndReason {
    pub fn describe(&self) -> String {
        match self {
            EndReason::Stopped => "stopped".into(),
            EndReason::Expired => "duration elapsed".into(),
            EndReason::BatteryThreshold { percent, threshold } => {
                format!("battery at {percent}% fell below the {threshold}% threshold")
            }
            EndReason::SwitchedToBattery => "switched to battery power".into(),
            EndReason::Terminated => "terminated by signal".into(),
            EndReason::CommandExited { code: Some(c) } => format!("command exited with status {c}"),
            EndReason::CommandExited { code: None } => "command was terminated".into(),
        }
    }
}

/// Record of a finished session (`last-session.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub mode: SessionMode,
    pub started_at: u64,
    pub ended_at: u64,
    pub end: EndReason,
    pub resumes: u32,
    /// Set if releasing the OS assertions reported an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_error: Option<String>,
}

/// Result of inspecting the runtime directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// No session.
    Inactive,
    /// A live host owns the session.
    Active(SessionState),
    /// A host holds the lock but has not published its state yet.
    Starting,
    /// State exists but no live host owns it (crash or forced kill).
    Stale(SessionState),
}

/// Exclusive ownership of the session, held by the session host.
#[derive(Debug)]
pub struct HostLock {
    _file: File,
}

/// Serializes concurrent `start` commands.
#[derive(Debug)]
pub struct StartLock {
    _file: File,
}

/// Access to session files.
#[derive(Debug, Clone)]
pub struct SessionStore {
    runtime_dir: PathBuf,
    data_dir: PathBuf,
}

impl SessionStore {
    pub fn new(paths: &Paths) -> SessionStore {
        SessionStore {
            runtime_dir: paths.runtime_dir.clone(),
            data_dir: paths.data_dir.clone(),
        }
    }

    pub fn with_dirs(runtime_dir: &Path, data_dir: &Path) -> SessionStore {
        SessionStore {
            runtime_dir: runtime_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        }
    }

    pub fn state_path(&self) -> PathBuf {
        self.runtime_dir.join("session.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.runtime_dir.join("session.lock")
    }

    fn start_lock_path(&self) -> PathBuf {
        self.runtime_dir.join("start.lock")
    }

    fn stop_request_path(&self) -> PathBuf {
        self.runtime_dir.join("stop-request")
    }

    pub fn last_session_path(&self) -> PathBuf {
        self.data_dir.join("last-session.json")
    }

    fn open_lock_file(&self, path: &Path) -> Result<File> {
        Paths::ensure_dir(&self.runtime_dir)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| Error::io(format!("opening {}", path.display()), e))
    }

    /// Take exclusive ownership of the session without blocking.
    pub fn acquire_host_lock(&self) -> Result<HostLock> {
        let file = self.open_lock_file(&self.lock_path())?;
        match file.try_lock() {
            Ok(()) => Ok(HostLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(Error::conflict(
                "another Caffeine session is already active",
            )),
            Err(TryLockError::Error(e)) => Err(Error::io("locking the session file", e)),
        }
    }

    /// Serialize `start` invocations, waiting up to `timeout`.
    pub fn acquire_start_lock(&self, timeout: Duration) -> Result<StartLock> {
        let file = self.open_lock_file(&self.start_lock_path())?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(StartLock { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::conflict(
                        "timed out waiting for another `caffeine start` to finish",
                    ));
                }
                Err(TryLockError::Error(e)) => return Err(Error::io("locking start.lock", e)),
            }
        }
    }

    /// Inspect the runtime directory.
    pub fn probe(&self) -> Result<Probe> {
        let state = self.read_state()?;
        if !self.lock_path().exists() {
            return Ok(match state {
                Some(s) => Probe::Stale(s),
                None => Probe::Inactive,
            });
        }
        let file = self.open_lock_file(&self.lock_path())?;
        let held = match file.try_lock() {
            Ok(()) => false,
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(e)) => return Err(Error::io("probing the session lock", e)),
        };
        drop(file);
        Ok(match (held, state) {
            (true, Some(s)) => Probe::Active(s),
            (true, None) => Probe::Starting,
            (false, Some(s)) => Probe::Stale(s),
            (false, None) => Probe::Inactive,
        })
    }

    /// Wait until the session host for `id` has exited, up to `timeout`.
    /// Returns `true` if it ended in time.
    pub fn wait_for_end(&self, id: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.probe()? {
                Probe::Active(s) if s.id == id => {}
                Probe::Starting => {}
                _ => return Ok(true),
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Read `session.json`. A corrupt file is reported as an error.
    pub fn read_state(&self) -> Result<Option<SessionState>> {
        fsutil::read_json(&self.state_path())
    }

    pub fn write_state(&self, state: &SessionState) -> Result<()> {
        Paths::ensure_dir(&self.runtime_dir)?;
        fsutil::write_json_atomic(&self.state_path(), state)
    }

    pub fn remove_state(&self) -> Result<()> {
        fsutil::remove_if_exists(&self.state_path())
    }

    /// Remove leftover state if (and only if) no live host owns it.
    /// Returns the removed state.
    pub fn clear_stale(&self) -> Result<Option<SessionState>> {
        let _lock = match self.acquire_host_lock() {
            Ok(lock) => lock,
            Err(e) if e.kind() == crate::ErrorKind::Conflict => return Ok(None),
            Err(e) => return Err(e),
        };
        // A corrupt state file with no owner is garbage; remove it.
        let state = self.read_state().unwrap_or_default();
        self.remove_state()?;
        fsutil::remove_if_exists(&self.stop_request_path())?;
        Ok(state)
    }

    /// Ask the host owning session `id` to stop.
    pub fn request_stop(&self, id: &str) -> Result<()> {
        fsutil::write_atomic(&self.stop_request_path(), id.as_bytes(), false)
    }

    /// Consume a stop request addressed to `id`, if any.
    pub fn take_stop_request(&self, id: &str) -> bool {
        match std::fs::read_to_string(self.stop_request_path()) {
            Ok(content) if content.trim() == id => {
                let _ = fsutil::remove_if_exists(&self.stop_request_path());
                true
            }
            _ => false,
        }
    }

    pub fn clear_stop_request(&self) -> Result<()> {
        fsutil::remove_if_exists(&self.stop_request_path())
    }

    pub fn write_last_session(&self, record: &SessionRecord) -> Result<()> {
        Paths::ensure_dir(&self.data_dir)?;
        fsutil::write_json_atomic(&self.last_session_path(), record)
    }

    pub fn read_last_session(&self) -> Result<Option<SessionRecord>> {
        fsutil::read_json(&self.last_session_path())
    }
}

/// Generate a short, unique-enough session identifier.
pub fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = (nanos as u64) ^ (u64::from(std::process::id()) << 40) ^ (nanos >> 64) as u64;
    format!("{:012x}", mixed & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::with_dirs(&dir.path().join("run"), &dir.path().join("data"));
        (dir, store)
    }

    fn sample_state(id: &str) -> SessionState {
        SessionState {
            format: STATE_FORMAT,
            id: id.into(),
            pid: std::process::id(),
            mode: SessionMode::Background,
            backend: "fake".into(),
            started_at: 100,
            expires_at: Some(160),
            request: AwakeRequest::default(),
            assertions: vec![],
            battery: BatteryWatch::default(),
            heartbeat_at: 100,
            resumes: 0,
            last_resume_at: None,
            log_file: None,
        }
    }

    #[test]
    fn probe_distinguishes_active_stale_and_starting() {
        let (_d, store) = store();
        assert_eq!(store.probe().unwrap(), Probe::Inactive);

        let lock = store.acquire_host_lock().unwrap();
        assert_eq!(store.probe().unwrap(), Probe::Starting);
        store.write_state(&sample_state("a")).unwrap();
        assert!(matches!(store.probe().unwrap(), Probe::Active(s) if s.id == "a"));

        // A second host cannot take over.
        assert_eq!(
            store.acquire_host_lock().unwrap_err().kind(),
            crate::ErrorKind::Conflict
        );

        // The host "crashes": the lock goes away but the state file stays.
        drop(lock);
        assert!(matches!(store.probe().unwrap(), Probe::Stale(s) if s.id == "a"));
        assert_eq!(store.clear_stale().unwrap().unwrap().id, "a");
        assert_eq!(store.probe().unwrap(), Probe::Inactive);
    }

    #[test]
    fn clear_stale_never_touches_a_live_session() {
        let (_d, store) = store();
        let _lock = store.acquire_host_lock().unwrap();
        store.write_state(&sample_state("live")).unwrap();
        assert_eq!(store.clear_stale().unwrap(), None);
        assert!(store.read_state().unwrap().is_some());
    }

    #[test]
    fn stop_requests_are_addressed() {
        let (_d, store) = store();
        store.request_stop("abc").unwrap();
        assert!(!store.take_stop_request("other"));
        assert!(store.take_stop_request("abc"));
        assert!(!store.take_stop_request("abc"), "requests are consumed");
    }

    #[test]
    fn remaining_time() {
        let s = sample_state("x");
        assert_eq!(s.remaining(130), Some(30));
        assert_eq!(s.remaining(500), Some(0));
    }

    #[test]
    fn session_ids_are_hex() {
        let id = new_session_id();
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
