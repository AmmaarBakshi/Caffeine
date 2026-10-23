//! Native power-management backends.
//!
//! | Platform | Idle sleep                       | Display                    | Lid                                   |
//! |----------|----------------------------------|----------------------------|---------------------------------------|
//! | Windows  | `PowerSetRequest(SystemRequired)`| `PowerSetRequest(Display)` | power scheme `LIDACTION`, lid switch notification |
//! | macOS    | `IOPMAssertion PreventUserIdleSystemSleep` | `PreventUserIdleDisplaySleep` | `AppleClamshellState` (read-only) |
//! | Linux    | logind `Inhibit("idle")` + desktop inhibitors | desktop inhibitors | logind `LidClosed`, `HandleLidSwitch*`, `Inhibit("handle-lid-switch")` |
//!
//! Every assertion Caffeine takes is bound to the lifetime of the process
//! that holds it, so the operating system cleans up even after a forced
//! kill. The only exception is a *persistent* lid-policy change on Windows,
//! which is journaled (see [`journal`]) so it can always be restored.

use std::path::PathBuf;

pub use guard::CompositeGuard;

use caffeine_core::Result;
use caffeine_core::power::PowerBackend;

mod guard;
pub mod journal;
pub mod process;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod unsupported;
#[cfg(windows)]
mod windows;

/// Options that influence backend construction.
#[derive(Debug, Clone)]
pub struct BackendOptions {
    /// Value of `advanced.backend` (`auto` or a platform-specific name).
    pub backend: String,
    /// Enabled experimental flags (`advanced.experimental`).
    pub experimental: Vec<String>,
    /// Durable data directory, used for the restore journal.
    pub data_dir: PathBuf,
}

impl BackendOptions {
    pub fn new(data_dir: PathBuf) -> BackendOptions {
        BackendOptions {
            backend: "auto".into(),
            experimental: Vec::new(),
            data_dir,
        }
    }

    pub fn experimental(&self, flag: &str) -> bool {
        self.experimental.iter().any(|f| f == flag)
    }
}

/// Construct the native backend for this platform.
pub fn native(options: &BackendOptions) -> Result<Box<dyn PowerBackend>> {
    #[cfg(windows)]
    {
        windows::WindowsBackend::new(options).map(|b| Box::new(b) as Box<dyn PowerBackend>)
    }
    #[cfg(target_os = "macos")]
    {
        macos::MacBackend::new(options).map(|b| Box::new(b) as Box<dyn PowerBackend>)
    }
    #[cfg(target_os = "linux")]
    {
        linux::LinuxBackend::new(options).map(|b| Box::new(b) as Box<dyn PowerBackend>)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        unsupported::UnsupportedBackend::new(options).map(|b| Box::new(b) as Box<dyn PowerBackend>)
    }
}

/// Name of the operating system family this binary was built for.
pub fn os_name() -> &'static str {
    std::env::consts::OS
}
