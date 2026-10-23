//! Windows 10 / 11 backend.

mod lid;
mod request;
mod scheme;
mod util;

use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::Power::{
    GetPwrCapabilities, GetSystemPowerStatus, SYSTEM_POWER_CAPABILITIES, SYSTEM_POWER_STATUS,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use caffeine_core::power::*;
use caffeine_core::{Error, Result};

use crate::guard::CompositeGuard;
use crate::journal::{EntryKind, Journal};
use crate::{BackendOptions, process};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mechanism {
    PowerRequest,
    ExecutionState,
}

#[derive(Debug)]
pub struct WindowsBackend {
    mechanism: Mechanism,
    journal: Journal,
    session_lid_override: bool,
    lid_present: bool,
    modern_standby: bool,
    batteries_present: bool,
}

impl WindowsBackend {
    pub fn new(options: &BackendOptions) -> Result<WindowsBackend> {
        let mechanism = match options.backend.as_str() {
            "auto" | "power-request" => Mechanism::PowerRequest,
            "execution-state" => Mechanism::ExecutionState,
            other => {
                return Err(Error::config(format!(
                    "advanced.backend \"{other}\" is not available on Windows \
                     (use auto, power-request or execution-state)"
                )));
            }
        };
        let caps = power_capabilities();
        Ok(WindowsBackend {
            mechanism,
            journal: Journal::in_dir(&options.data_dir),
            session_lid_override: options.experimental("session-lid-override"),
            lid_present: caps.as_ref().is_some_and(|c| c.LidPresent),
            modern_standby: caps.as_ref().is_some_and(|c| c.AoAc),
            batteries_present: caps.as_ref().is_some_and(|c| c.SystemBatteriesPresent),
        })
    }

    fn mechanism_name(&self) -> &'static str {
        match self.mechanism {
            Mechanism::PowerRequest => "PowerSetRequest",
            Mechanism::ExecutionState => "SetThreadExecutionState",
        }
    }
}

fn power_capabilities() -> Option<SYSTEM_POWER_CAPABILITIES> {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    // SAFETY: `caps` is a valid out-parameter.
    unsafe { GetPwrCapabilities(&mut caps) }.then_some(caps)
}

fn read_registry_string(subkey: &str, value: &str) -> Option<String> {
    let subkey_w = util::wide(subkey);
    let value_w = util::wide(value);
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffers are valid and `size` is their byte length.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey_w.as_ptr(),
            value_w.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (rc == ERROR_SUCCESS).then(|| util::from_wide(&buf))
}

/// e.g. "Windows 11 Pro 25H2 (build 26200)".
fn os_version() -> Option<String> {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let mut product = read_registry_string(KEY, "ProductName")?;
    let build = read_registry_string(KEY, "CurrentBuildNumber").unwrap_or_default();
    // Windows 11 still reports "Windows 10" in ProductName.
    if build.parse::<u32>().is_ok_and(|b| b >= 22_000) {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }
    let display = read_registry_string(KEY, "DisplayVersion").unwrap_or_default();
    Some(format!("{product} {display} (build {build})").replace("  ", " "))
}

fn is_elevated() -> Option<bool> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: plain Win32 calls; the token handle is closed below.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        ) != 0;
        CloseHandle(token);
        ok.then_some(elevation.TokenIsElevated != 0)
    }
}

impl PowerBackend for WindowsBackend {
    fn name(&self) -> &str {
        match self.mechanism {
            Mechanism::PowerRequest => "windows-power-request",
            Mechanism::ExecutionState => "windows-execution-state",
        }
    }

    fn capabilities(&self) -> CapabilityReport {
        let mut notes = Vec::new();
        if self.modern_standby {
            notes.push("Modern Standby (S0 low-power idle) system".to_string());
        }
        let platform = PlatformInfo {
            os: "windows".into(),
            os_version: os_version(),
            arch: std::env::consts::ARCH.into(),
            backend: self.name().into(),
            init_system: None,
            desktop: None,
            notes,
        };
        let mech = self.mechanism_name();
        let mut caps = Vec::new();
        if self.modern_standby {
            caps.push(CapabilityStatus::new(
                Capability::IdleSleep,
                Support::Limited,
                Some(mech),
                "Modern Standby: the idle timer is blocked, but Windows may still enter standby \
                 after the display turns off; use --display to keep the screen on if that happens",
            ));
        } else {
            caps.push(CapabilityStatus::new(
                Capability::IdleSleep,
                Support::Supported,
                Some(mech),
                "system-required request; explicit sleep, lid close and critical battery still apply",
            ));
        }
        caps.push(CapabilityStatus::new(
            Capability::DisplaySleep,
            Support::Supported,
            Some(mech),
            "display-required request",
        ));
        if self.lid_present {
            caps.push(CapabilityStatus::new(
                Capability::LidState,
                Support::Supported,
                Some("GUID_LIDSWITCH_STATE_CHANGE notification"),
                "lid switch reported by the power manager",
            ));
        } else {
            caps.push(CapabilityStatus::unsupported(
                Capability::LidState,
                "no lid switch reported by the firmware (desktop or VM)",
            ));
        }
        match scheme::active_scheme().and_then(|s| scheme::read_lid_values(&s.guid)) {
            Ok(_) => caps.push(CapabilityStatus::new(
                Capability::LidPolicyRead,
                Support::Supported,
                Some("power scheme SUB_BUTTONS\\LIDACTION"),
                "per power source (plugged in / on battery)",
            )),
            Err(e) => caps.push(CapabilityStatus::new(
                Capability::LidPolicyRead,
                if e.kind() == caffeine_core::ErrorKind::PermissionDenied {
                    Support::RequiresPermission
                } else {
                    Support::Unsupported
                },
                Some("power scheme SUB_BUTTONS\\LIDACTION"),
                e.to_string(),
            )),
        }
        if self.lid_present {
            caps.push(CapabilityStatus::new(
                Capability::LidPolicyWrite,
                Support::Supported,
                Some("PowerWriteACValueIndex / PowerWriteDCValueIndex"),
                "persistent; original values are journaled and `caffeine lid restore` reverts them \
                 (group policy may deny the change)",
            ));
            if self.session_lid_override {
                caps.push(CapabilityStatus::new(
                    Capability::LidSessionOverride,
                    Support::Limited,
                    Some("temporary power scheme change (experimental)"),
                    "Windows has no session-scoped lid inhibitor; the lid action is changed while \
                     the session runs and restored afterwards (journaled for crash recovery)",
                ));
            } else {
                caps.push(CapabilityStatus::unsupported(
                    Capability::LidSessionOverride,
                    "Windows has no session-scoped lid inhibitor; enable the experimental \
                     `session-lid-override` flag to emulate one by temporarily changing the power scheme",
                ));
            }
        } else {
            for cap in [Capability::LidPolicyWrite, Capability::LidSessionOverride] {
                caps.push(CapabilityStatus::unsupported(cap, "no lid present"));
            }
        }
        caps.push(CapabilityStatus::new(
            Capability::PowerSource,
            Support::Supported,
            Some("GetSystemPowerStatus"),
            if self.batteries_present { "AC line and battery status" } else { "AC line status (no battery)" },
        ));
        caps.push(CapabilityStatus::new(
            Capability::ResumeDetection,
            Support::Supported,
            Some("session host clock-gap detection"),
            "resume is inferred from a gap in the session host's polling loop",
        ));
        CapabilityReport {
            platform,
            capabilities: caps,
        }
    }

    fn acquire(&self, req: &AwakeRequest) -> Result<Box<dyn AwakeGuard>> {
        let mut guard = CompositeGuard::new();
        if req.idle_sleep || req.display {
            match self.mechanism {
                Mechanism::PowerRequest => guard.push(Box::new(request::power_request(
                    &req.reason,
                    req.idle_sleep,
                    req.display,
                )?)),
                Mechanism::ExecutionState => guard.push(Box::new(request::ExecutionState::acquire(
                    req.idle_sleep,
                    req.display,
                )?)),
            }
        }
        if req.lid != LidSessionMode::System {
            if !self.lid_present {
                return Err(Error::unsupported("this machine has no lid to override"));
            }
            if !self.session_lid_override {
                return Err(Error::unsupported(
                    "session-scoped lid override is not available on Windows by default",
                )
                .with_hint(
                    "enable it with `caffeine config set advanced.experimental session-lid-override`, \
                     or change the policy explicitly with `caffeine lid set`",
                ));
            }
            let dc = (req.lid == LidSessionMode::KeepAwake).then_some(0);
            guard.push(Box::new(scheme::SessionLidOverride::apply(&self.journal, Some(0), dc)?));
        }
        Ok(Box::new(guard))
    }

    fn lid_status(&self) -> Result<LidStatus> {
        if !self.lid_present {
            return Ok(LidStatus {
                state: LidState::NotPresent,
                source: "GetPwrCapabilities".into(),
                notes: vec![],
            });
        }
        let state = lid::query(Duration::from_millis(1_500))?;
        let mut notes = Vec::new();
        if state == LidState::Unknown {
            notes.push("the power manager did not report the lid switch state".to_string());
        }
        Ok(LidStatus {
            state,
            source: "GUID_LIDSWITCH_STATE_CHANGE".into(),
            notes,
        })
    }

    fn lid_policy(&self) -> Result<LidPolicy> {
        let scheme = scheme::active_scheme()?;
        let values = scheme::read_lid_values(&scheme.guid)?;
        let mut notes = Vec::new();
        if !self.lid_present {
            notes.push("no lid present; the setting has no effect on this machine".to_string());
        }
        for entry in self.journal.entries()? {
            notes.push(format!(
                "Caffeine changed this setting ({}); `caffeine lid restore` reverts it",
                entry.description
            ));
        }
        Ok(LidPolicy {
            on_ac: Some(scheme::action_from_index(values.ac)),
            on_battery: Some(scheme::action_from_index(values.dc)),
            docked: None,
            source: format!(
                "power scheme \"{}\" ({})",
                scheme.name.unwrap_or_else(|| "active".into()),
                util::guid_to_string(&scheme.guid)
            ),
            writable: self.lid_present,
            overridden_by: vec![],
            notes,
        })
    }

    fn set_lid_policy(&self, change: &LidPolicyChange) -> Result<LidPolicy> {
        if !self.lid_present {
            return Err(Error::unsupported("this machine has no lid; refusing to change the lid action"));
        }
        if change.is_empty() {
            return Err(Error::usage("nothing to change (pass --ac and/or --battery)"));
        }
        let ac = change.on_ac.as_ref().map(scheme::index_from_action).transpose()?;
        let dc = change.on_battery.as_ref().map(scheme::index_from_action).transpose()?;
        scheme::apply_with_journal(&self.journal, EntryKind::PersistentLidPolicy, ac, dc)?;
        self.lid_policy()
    }

    fn power_source(&self) -> Result<PowerSource> {
        let mut status = SYSTEM_POWER_STATUS {
            ACLineStatus: 0,
            BatteryFlag: 0,
            BatteryLifePercent: 0,
            SystemStatusFlag: 0,
            BatteryLifeTime: 0,
            BatteryFullLifeTime: 0,
        };
        // SAFETY: valid out-parameter.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return Err(util::last_error("GetSystemPowerStatus"));
        }
        let no_battery = status.BatteryFlag == 128 || status.BatteryFlag == 255 && !self.batteries_present;
        let kind = match status.ACLineStatus {
            1 => PowerSourceKind::Ac,
            0 => PowerSourceKind::Battery,
            _ => PowerSourceKind::Unknown,
        };
        Ok(PowerSource {
            kind,
            battery_present: !no_battery,
            battery_percent: (!no_battery && status.BatteryLifePercent <= 100)
                .then_some(status.BatteryLifePercent),
            charging: (!no_battery && status.BatteryFlag != 255).then_some(status.BatteryFlag & 8 != 0),
        })
    }

    fn diagnose(&self) -> Vec<DiagnosticCheck> {
        let mut checks = Vec::new();
        let probe = AwakeRequest {
            reason: "caffeine doctor: verifying power request support".into(),
            ..AwakeRequest::default()
        };
        match self.acquire(&probe).and_then(|g| g.release()) {
            Ok(()) => checks.push(DiagnosticCheck::new(
                "windows.power-request",
                CheckStatus::Ok,
                format!("{} accepted and released a system-required request", self.mechanism_name()),
            )),
            Err(e) => checks.push(
                DiagnosticCheck::new("windows.power-request", CheckStatus::Fail, e.to_string())
                    .with_hint("idle-sleep prevention will not work; check `powercfg /requests` as Administrator"),
            ),
        }
        match self.lid_policy() {
            Ok(p) => checks.push(DiagnosticCheck::new(
                "windows.lid-policy",
                CheckStatus::Ok,
                format!(
                    "lid close: plugged in = {}, on battery = {} ({})",
                    p.on_ac.map(|a| a.to_string()).unwrap_or_default(),
                    p.on_battery.map(|a| a.to_string()).unwrap_or_default(),
                    p.source
                ),
            )),
            Err(e) => checks.push(DiagnosticCheck::new("windows.lid-policy", CheckStatus::Warn, e.to_string())),
        }
        if self.modern_standby {
            checks.push(
                DiagnosticCheck::new(
                    "windows.modern-standby",
                    CheckStatus::Info,
                    "Modern Standby system: standby may begin when the display turns off",
                )
                .with_hint("use `caffeine start --display` if the machine still sleeps"),
            );
        }
        match is_elevated() {
            Some(true) => checks.push(DiagnosticCheck::new(
                "windows.elevation",
                CheckStatus::Info,
                "running elevated; Caffeine does not need Administrator rights for sessions",
            )),
            Some(false) => checks.push(DiagnosticCheck::new(
                "windows.elevation",
                CheckStatus::Ok,
                "running without elevation (sufficient for sessions and, usually, lid policy)",
            )),
            None => {}
        }
        checks
    }

    fn pending_recovery(&self) -> Result<Vec<RecoveryItem>> {
        Ok(self
            .journal
            .entries()?
            .into_iter()
            .filter_map(|e| {
                let left_by_crash = match e.kind {
                    EntryKind::SessionLidOverride { pid } => {
                        if pid != std::process::id() && process::is_alive(pid) {
                            return None; // a live session owns it
                        }
                        true
                    }
                    EntryKind::PersistentLidPolicy => false,
                };
                Some(RecoveryItem {
                    id: e.id,
                    description: e.description,
                    left_by_crash,
                    created_at: e.created_at,
                })
            })
            .collect())
    }

    fn recover(&self) -> Result<Vec<String>> {
        let pending = self.pending_recovery()?;
        let mut restored = Vec::new();
        for item in pending {
            let Some(entry) = self.journal.entries()?.into_iter().find(|e| e.id == item.id) else {
                continue;
            };
            restored.push(scheme::restore_entry(&entry)?);
            self.journal.remove(&entry.id)?;
        }
        Ok(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> (tempfile::TempDir, WindowsBackend) {
        let dir = tempfile::tempdir().unwrap();
        let b = WindowsBackend::new(&BackendOptions::new(dir.path().to_path_buf())).unwrap();
        (dir, b)
    }

    /// Exercises the real Windows API: a system-required power request must
    /// be created and released without error.
    #[test]
    fn power_request_round_trip() {
        let (_d, b) = backend();
        let guard = b.acquire(&AwakeRequest::default()).unwrap();
        let assertions = guard.assertions();
        assert_eq!(assertions.len(), 1);
        assert_eq!(assertions[0].capability, Capability::IdleSleep);
        guard.release().unwrap();
    }

    #[test]
    fn display_request_round_trip() {
        let (_d, b) = backend();
        let req = AwakeRequest {
            display: true,
            ..AwakeRequest::default()
        };
        let guard = b.acquire(&req).unwrap();
        assert_eq!(guard.assertions().len(), 2);
        drop(guard); // Drop must release too.
    }

    #[test]
    fn execution_state_backend() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = BackendOptions::new(dir.path().to_path_buf());
        opts.backend = "execution-state".into();
        let b = WindowsBackend::new(&opts).unwrap();
        b.acquire(&AwakeRequest::default()).unwrap().release().unwrap();
    }

    #[test]
    fn rejects_foreign_backend_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = BackendOptions::new(dir.path().to_path_buf());
        opts.backend = "logind".into();
        assert!(WindowsBackend::new(&opts).is_err());
    }

    #[test]
    fn session_lid_override_requires_opt_in() {
        let (_d, b) = backend();
        let req = AwakeRequest {
            lid: LidSessionMode::KeepAwake,
            ..AwakeRequest::default()
        };
        let err = b.acquire(&req).err().expect("must not succeed without opt-in");
        assert_eq!(err.kind(), caffeine_core::ErrorKind::Unsupported);
    }

    #[test]
    fn reads_power_state_and_capabilities() {
        let (_d, b) = backend();
        let report = b.capabilities();
        assert_eq!(report.platform.os, "windows");
        assert!(report.is_usable(Capability::IdleSleep));
        b.power_source().unwrap();
        b.lid_status().unwrap();
        // Lid policy is readable on every Windows edition we target.
        let policy = b.lid_policy().unwrap();
        assert!(policy.on_ac.is_some() && policy.on_battery.is_some());
        assert!(b.pending_recovery().unwrap().is_empty());
    }
}
