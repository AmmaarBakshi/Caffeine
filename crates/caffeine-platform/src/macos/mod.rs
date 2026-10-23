//! macOS backend (Intel and Apple Silicon), using IOKit power assertions.
//!
//! Assertions created with `IOPMAssertionCreateWithName` are owned by the
//! process and released by the kernel when it exits. They appear in
//! `pmset -g assertions` under the name Caffeine passes.
//!
//! macOS offers no supported, per-user lid-close setting. Closing the lid
//! sleeps the machine unless it is in clamshell mode (external display,
//! power and input attached); Caffeine reports this but never pretends to
//! change it.

mod ffi;

use caffeine_core::power::*;
use caffeine_core::{Error, Result};

use crate::BackendOptions;
use crate::guard::CompositeGuard;

#[derive(Debug)]
pub struct MacBackend;

impl MacBackend {
    pub fn new(options: &BackendOptions) -> Result<MacBackend> {
        match options.backend.as_str() {
            "auto" | "iokit" => Ok(MacBackend),
            other => Err(Error::config(format!(
                "advanced.backend \"{other}\" is not available on macOS (use auto or iokit)"
            ))),
        }
    }
}

fn io_error(context: &str, code: ffi::IOReturn) -> Error {
    let msg = format!("{context} failed with IOReturn 0x{:08x}", code as u32);
    if code == ffi::kIOReturnNotPrivileged || code == ffi::kIOReturnNotPermitted {
        Error::permission(msg)
    } else {
        Error::failure(msg)
    }
    .with_os_code(i64::from(code))
}

/// One IOPM assertion.
struct Assertion {
    id: ffi::IOPMAssertionID,
    kind: &'static str,
    capability: Capability,
    released: bool,
}

impl Assertion {
    fn create(kind: &'static str, reason: &str, capability: Capability) -> Result<Assertion> {
        let kind_cf =
            ffi::cf_string(kind).ok_or_else(|| Error::failure("CFStringCreate failed"))?;
        let name_cf =
            ffi::cf_string(reason).ok_or_else(|| Error::failure("CFStringCreate failed"))?;
        let mut id: ffi::IOPMAssertionID = 0;
        // SAFETY: both CFStrings are valid for the call; `id` is an out-param.
        let rc = unsafe {
            ffi::IOPMAssertionCreateWithName(
                kind_cf.as_ptr(),
                ffi::kIOPMAssertionLevelOn,
                name_cf.as_ptr(),
                &mut id,
            )
        };
        if rc != ffi::kIOReturnSuccess {
            return Err(io_error(
                &format!("IOPMAssertionCreateWithName({kind})"),
                rc,
            ));
        }
        Ok(Assertion {
            id,
            kind,
            capability,
            released: false,
        })
    }

    fn do_release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        // SAFETY: releasing an assertion we created, once.
        let rc = unsafe { ffi::IOPMAssertionRelease(self.id) };
        if rc != ffi::kIOReturnSuccess {
            return Err(io_error(
                &format!("IOPMAssertionRelease({})", self.kind),
                rc,
            ));
        }
        Ok(())
    }
}

impl AwakeGuard for Assertion {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        vec![ActiveAssertion {
            capability: self.capability,
            mechanism: format!("IOPMAssertion {}", self.kind),
            detail: format!("id {}; listed by `pmset -g assertions`", self.id),
        }]
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.do_release()
    }
}

impl Drop for Assertion {
    fn drop(&mut self) {
        if let Err(e) = self.do_release() {
            log::error!("{e}");
        }
    }
}

fn clamshell_state() -> Option<bool> {
    ffi::root_domain_property("AppleClamshellState").and_then(|v| ffi::to_bool(v.as_ptr()))
}

fn clamshell_causes_sleep() -> Option<bool> {
    ffi::root_domain_property("AppleClamshellCausesSleep").and_then(|v| ffi::to_bool(v.as_ptr()))
}

fn os_version() -> Option<String> {
    let name = std::ffi::CString::new("kern.osproductversion").ok()?;
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: buffer and length are valid; no new value is written.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(len);
    Some(format!("macOS {}", String::from_utf8_lossy(&buf[..end])))
}

const UNSUPPORTED_LID_WRITE: &str = "macOS has no supported lid-close setting: the lid sleeps the Mac \
    unless clamshell mode is active (external display, power and input). `pmset disablesleep` disables \
    all sleep system-wide and needs root, so Caffeine does not use it";

impl PowerBackend for MacBackend {
    fn name(&self) -> &str {
        "macos-iokit"
    }

    fn capabilities(&self) -> CapabilityReport {
        let lid = clamshell_state().is_some();
        let platform = PlatformInfo {
            os: "macos".into(),
            os_version: os_version(),
            arch: std::env::consts::ARCH.into(),
            backend: self.name().into(),
            init_system: None,
            desktop: None,
            notes: vec![],
        };
        let mut caps = vec![
            CapabilityStatus::new(
                Capability::IdleSleep,
                Support::Supported,
                Some("IOPMAssertion PreventUserIdleSystemSleep"),
                "idle sleep is blocked; closing the lid or choosing Sleep still sleeps the Mac",
            ),
            CapabilityStatus::new(
                Capability::DisplaySleep,
                Support::Supported,
                Some("IOPMAssertion PreventUserIdleDisplaySleep"),
                "display idle sleep is blocked",
            ),
        ];
        if lid {
            caps.push(CapabilityStatus::new(
                Capability::LidState,
                Support::Supported,
                Some("IOPMrootDomain AppleClamshellState"),
                "lid state from the power management root domain",
            ));
            caps.push(CapabilityStatus::new(
                Capability::LidPolicyRead,
                Support::Limited,
                Some("IOPMrootDomain AppleClamshellCausesSleep"),
                "reports whether closing the lid will sleep the Mac right now (no configurable action exists)",
            ));
        } else {
            caps.push(CapabilityStatus::unsupported(
                Capability::LidState,
                "no lid (desktop Mac)",
            ));
            caps.push(CapabilityStatus::unsupported(
                Capability::LidPolicyRead,
                "no lid (desktop Mac)",
            ));
        }
        caps.push(CapabilityStatus::unsupported(
            Capability::LidPolicyWrite,
            UNSUPPORTED_LID_WRITE,
        ));
        caps.push(CapabilityStatus::unsupported(
            Capability::LidSessionOverride,
            UNSUPPORTED_LID_WRITE,
        ));
        caps.push(CapabilityStatus::new(
            Capability::PowerSource,
            Support::Supported,
            Some("IOPowerSources"),
            "AC adapter and internal battery status",
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
        if req.lid != LidSessionMode::System {
            return Err(Error::unsupported(UNSUPPORTED_LID_WRITE));
        }
        let mut guard = CompositeGuard::new();
        if req.idle_sleep {
            guard.push(Box::new(Assertion::create(
                "PreventUserIdleSystemSleep",
                &req.reason,
                Capability::IdleSleep,
            )?));
        }
        if req.display {
            guard.push(Box::new(Assertion::create(
                "PreventUserIdleDisplaySleep",
                &req.reason,
                Capability::DisplaySleep,
            )?));
        }
        Ok(Box::new(guard))
    }

    fn lid_status(&self) -> Result<LidStatus> {
        Ok(match clamshell_state() {
            Some(closed) => LidStatus {
                state: if closed {
                    LidState::Closed
                } else {
                    LidState::Open
                },
                source: "IOPMrootDomain AppleClamshellState".into(),
                notes: vec![],
            },
            None => LidStatus {
                state: LidState::NotPresent,
                source: "IOPMrootDomain".into(),
                notes: vec![],
            },
        })
    }

    fn lid_policy(&self) -> Result<LidPolicy> {
        if clamshell_state().is_none() {
            return Err(Error::unsupported("this Mac has no lid"));
        }
        let action = match clamshell_causes_sleep() {
            Some(true) => Some(LidAction::Sleep),
            Some(false) => Some(LidAction::DoNothing),
            None => None,
        };
        Ok(LidPolicy {
            on_ac: action.clone(),
            on_battery: action,
            docked: None,
            source: "IOPMrootDomain AppleClamshellCausesSleep".into(),
            writable: false,
            overridden_by: vec![],
            notes: vec![
                "macOS decides dynamically: closing the lid sleeps the Mac unless clamshell mode is active \
                 (external display, power adapter and keyboard/mouse connected)"
                    .into(),
            ],
        })
    }

    fn set_lid_policy(&self, _change: &LidPolicyChange) -> Result<LidPolicy> {
        Err(Error::unsupported(UNSUPPORTED_LID_WRITE))
    }

    fn power_source(&self) -> Result<PowerSource> {
        // SAFETY: Copy-rule objects are wrapped in CfOwned; Get-rule
        // objects are only used while their owner is alive.
        unsafe {
            let blob = ffi::CfOwned::new(ffi::IOPSCopyPowerSourcesInfo())
                .ok_or_else(|| Error::failure("IOPSCopyPowerSourcesInfo returned nothing"))?;
            let providing = ffi::to_string(ffi::IOPSGetProvidingPowerSourceType(blob.as_ptr()));
            let kind = match providing.as_deref() {
                Some("AC Power") => PowerSourceKind::Ac,
                Some("Battery Power") => PowerSourceKind::Battery,
                Some("UPS Power") => PowerSourceKind::Ups,
                _ => PowerSourceKind::Unknown,
            };
            let mut source = PowerSource {
                kind,
                battery_present: false,
                battery_percent: None,
                charging: None,
            };
            if let Some(list) = ffi::CfOwned::new(ffi::IOPSCopyPowerSourcesList(blob.as_ptr())) {
                for i in 0..ffi::CFArrayGetCount(list.as_ptr()) {
                    let ps = ffi::CFArrayGetValueAtIndex(list.as_ptr(), i);
                    let desc = ffi::IOPSGetPowerSourceDescription(blob.as_ptr(), ps);
                    if desc.is_null() {
                        continue;
                    }
                    if ffi::to_string(ffi::dict_get(desc, "Type")).as_deref()
                        != Some("InternalBattery")
                    {
                        continue;
                    }
                    if ffi::to_bool(ffi::dict_get(desc, "Is Present")) == Some(false) {
                        continue;
                    }
                    source.battery_present = true;
                    let cur = ffi::to_i32(ffi::dict_get(desc, "Current Capacity"));
                    let max = ffi::to_i32(ffi::dict_get(desc, "Max Capacity"));
                    if let (Some(cur), Some(max)) = (cur, max)
                        && max > 0
                    {
                        source.battery_percent = Some(((cur * 100) / max).clamp(0, 100) as u8);
                    }
                    source.charging = ffi::to_bool(ffi::dict_get(desc, "Is Charging"));
                    break;
                }
            }
            Ok(source)
        }
    }

    fn diagnose(&self) -> Vec<DiagnosticCheck> {
        let mut checks = Vec::new();
        match Assertion::create(
            "PreventUserIdleSystemSleep",
            "caffeine doctor probe",
            Capability::IdleSleep,
        )
        .and_then(|a| Box::new(a).release())
        {
            Ok(()) => checks.push(DiagnosticCheck::new(
                "macos.assertion",
                CheckStatus::Ok,
                "IOKit created and released a PreventUserIdleSystemSleep assertion",
            )),
            Err(e) => checks.push(DiagnosticCheck::new(
                "macos.assertion",
                CheckStatus::Fail,
                e.to_string(),
            )),
        }
        match (clamshell_state(), clamshell_causes_sleep()) {
            (None, _) => checks.push(DiagnosticCheck::new(
                "macos.lid",
                CheckStatus::Info,
                "no lid (desktop Mac)",
            )),
            (Some(closed), causes) => checks.push(DiagnosticCheck::new(
                "macos.lid",
                CheckStatus::Info,
                format!(
                    "lid {}; closing it {} sleep the Mac",
                    if closed { "closed" } else { "open" },
                    match causes {
                        Some(false) => "will not (clamshell mode available)",
                        _ => "will",
                    }
                ),
            )),
        }
        checks
    }
}
