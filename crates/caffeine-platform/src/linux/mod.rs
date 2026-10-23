//! Linux backend (systemd-logind or elogind, plus desktop session
//! inhibitors). Works on any init system that provides the
//! `org.freedesktop.login1` D-Bus API, and degrades to desktop-only or
//! clearly-reported "unsupported" otherwise.

mod dbus;
mod sysfs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use zbus::blocking::Connection;

use caffeine_core::power::*;
use caffeine_core::{Error, ErrorKind, Result};

use crate::BackendOptions;
use crate::guard::CompositeGuard;
use dbus::{DesktopApi, DesktopInhibitor, LogindInhibitor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Both,
    LogindOnly,
    DesktopOnly,
}

#[derive(Debug)]
pub struct LinuxBackend {
    mode: Mode,
    root: PathBuf,
}

/// What the buses offer right now.
struct Environment {
    system: Option<Connection>,
    logind: bool,
    session: Option<Connection>,
    idle_api: Option<DesktopApi>,
    display_api: Option<DesktopApi>,
}

impl LinuxBackend {
    pub fn new(options: &BackendOptions) -> Result<LinuxBackend> {
        let mode = match options.backend.as_str() {
            "auto" | "logind+desktop" => Mode::Both,
            "logind" => Mode::LogindOnly,
            "desktop" => Mode::DesktopOnly,
            other => {
                return Err(Error::config(format!(
                    "advanced.backend \"{other}\" is not available on Linux \
                     (use auto, logind, desktop or logind+desktop)"
                )));
            }
        };
        Ok(LinuxBackend {
            mode,
            root: PathBuf::from("/"),
        })
    }

    fn uses_logind(&self) -> bool {
        self.mode != Mode::DesktopOnly
    }

    fn uses_desktop(&self) -> bool {
        self.mode != Mode::LogindOnly
    }

    fn environment(&self) -> Environment {
        let system = dbus::system_bus().ok();
        let logind = system
            .as_ref()
            .is_some_and(|c| dbus::name_has_owner(c, dbus::LOGIND));
        let session = if self.uses_desktop() {
            dbus::session_bus().ok()
        } else {
            None
        };
        let (mut idle_api, mut display_api) = (None, None);
        if let Some(conn) = &session {
            if (DesktopApi::GnomeSession { flags: 0 }).available(conn) {
                idle_api = Some(DesktopApi::GnomeSession { flags: 4 });
                display_api = Some(DesktopApi::GnomeSession { flags: 8 });
            } else {
                if DesktopApi::FreedesktopPowerManagement.available(conn) {
                    idle_api = Some(DesktopApi::FreedesktopPowerManagement);
                }
                if DesktopApi::FreedesktopScreenSaver.available(conn) {
                    display_api = Some(DesktopApi::FreedesktopScreenSaver);
                }
            }
        }
        Environment {
            system,
            logind,
            session,
            idle_api,
            display_api,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn require_logind(env: &Environment) -> Result<&Connection> {
        match (&env.system, env.logind) {
            (Some(conn), true) => Ok(conn),
            (None, _) => Err(Error::unsupported("no D-Bus system bus is available")),
            (Some(_), false) => Err(Error::unsupported(
                "org.freedesktop.login1 (systemd-logind or elogind) is not running",
            )),
        }
    }
}

fn desktop_name() -> Option<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .or_else(|| std::env::var("DESKTOP_SESSION").ok())
        .filter(|s| !s.is_empty())
}

impl PowerBackend for LinuxBackend {
    fn name(&self) -> &str {
        match self.mode {
            Mode::Both => "linux-logind+desktop",
            Mode::LogindOnly => "linux-logind",
            Mode::DesktopOnly => "linux-desktop",
        }
    }

    fn capabilities(&self) -> CapabilityReport {
        let env = self.environment();
        let wsl = sysfs::is_wsl(self.root());
        let lid_present = sysfs::lid_present(self.root());
        let mut notes = Vec::new();
        if wsl {
            notes.push("Windows Subsystem for Linux".to_string());
        }
        if env.session.is_none() {
            notes.push("no desktop session bus (headless, TTY or SSH session)".to_string());
        }
        let platform = PlatformInfo {
            os: "linux".into(),
            os_version: sysfs::distribution(self.root()),
            arch: std::env::consts::ARCH.into(),
            backend: self.name().into(),
            init_system: Some(sysfs::init_system(self.root())),
            desktop: desktop_name(),
            notes,
        };

        let mut caps = Vec::new();
        let mut idle_mechs = Vec::new();
        if self.uses_logind() && env.logind {
            idle_mechs.push("logind Inhibit(idle)".to_string());
        }
        if let Some(api) = env.idle_api {
            idle_mechs.push(api.describe());
        }
        caps.push(if idle_mechs.is_empty() {
            CapabilityStatus::unsupported(
                Capability::IdleSleep,
                "neither logind nor a desktop session inhibitor interface is available",
            )
        } else if wsl {
            CapabilityStatus::new(
                Capability::IdleSleep,
                Support::Limited,
                Some(&idle_mechs.join(" + ")),
                "inside WSL these inhibitors only affect the Linux side; Windows decides when the \
                 host sleeps (run the Windows build of caffeine to keep the host awake)",
            )
        } else {
            let detail = if env.idle_api.is_none() && env.session.is_some() {
                "logind idle inhibitor; this desktop exposes no idle inhibitor interface, so its own \
                 idle-suspend timer may not honor it"
            } else if env.idle_api.is_none() {
                "logind idle inhibitor (affects logind's IdleAction; no desktop session detected)"
            } else {
                "idle-triggered suspend is inhibited; explicit suspend and lid close are not blocked"
            };
            CapabilityStatus::new(Capability::IdleSleep, Support::Supported, Some(&idle_mechs.join(" + ")), detail)
        });

        caps.push(match env.display_api {
            Some(api) => CapabilityStatus::new(
                Capability::DisplaySleep,
                Support::Supported,
                Some(&api.describe()),
                "desktop screen blanking / screensaver is inhibited",
            ),
            None => CapabilityStatus::unsupported(
                Capability::DisplaySleep,
                "no desktop screensaver interface; console blanking is controlled by the kernel \
                 (consoleblank=) or `setterm --blank`",
            ),
        });

        caps.push(if lid_present {
            CapabilityStatus::new(
                Capability::LidState,
                Support::Supported,
                Some(if env.logind {
                    "logind LidClosed"
                } else {
                    "/proc/acpi/button/lid"
                }),
                "lid switch detected",
            )
        } else {
            CapabilityStatus::unsupported(
                Capability::LidState,
                "no lid switch detected (desktop, server or VM)",
            )
        });

        caps.push(if env.logind {
            CapabilityStatus::new(
                Capability::LidPolicyRead,
                Support::Supported,
                Some("logind HandleLidSwitch*"),
                "effective logind policy; desktop environments may apply their own lid settings on top",
            )
        } else {
            CapabilityStatus::unsupported(Capability::LidPolicyRead, "logind is not available")
        });

        caps.push(CapabilityStatus::unsupported(
            Capability::LidPolicyWrite,
            "persistent lid policy belongs to logind.conf (root) or the desktop's power settings; \
             Caffeine does not edit system files — use the session override instead",
        ));

        caps.push(match (env.logind && self.uses_logind(), lid_present) {
            (true, true) => CapabilityStatus::new(
                Capability::LidSessionOverride,
                Support::Supported,
                Some("logind Inhibit(handle-lid-switch)"),
                "released automatically when the session ends; polkit grants it to the active local \
                 session by default",
            ),
            (_, false) => CapabilityStatus::unsupported(Capability::LidSessionOverride, "no lid present"),
            (false, true) => CapabilityStatus::unsupported(
                Capability::LidSessionOverride,
                "requires logind (systemd-logind or elogind)",
            ),
        });

        caps.push(match sysfs::power_source(self.root()) {
            Some(_) => CapabilityStatus::new(
                Capability::PowerSource,
                Support::Supported,
                Some("/sys/class/power_supply"),
                "AC adapter and battery status",
            ),
            None => CapabilityStatus::unsupported(
                Capability::PowerSource,
                "/sys/class/power_supply is not available",
            ),
        });

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
        let env = self.environment();
        let mut guard = CompositeGuard::new();

        if req.idle_sleep {
            let mut failures: Vec<Error> = Vec::new();
            let mut held = false;
            if self.uses_logind() {
                match Self::require_logind(&env).and_then(|conn| {
                    LogindInhibitor::acquire(conn, "idle", &req.reason, Capability::IdleSleep)
                }) {
                    Ok(g) => {
                        guard.push(Box::new(g));
                        held = true;
                    }
                    Err(e) => failures.push(e),
                }
            }
            if let (Some(api), Some(conn)) = (env.idle_api, &env.session) {
                match DesktopInhibitor::acquire(
                    conn.clone(),
                    api,
                    &req.reason,
                    Capability::IdleSleep,
                ) {
                    Ok(g) => {
                        guard.push(Box::new(g));
                        held = true;
                    }
                    Err(e) => failures.push(e),
                }
            } else if self.mode == Mode::DesktopOnly {
                failures.push(Error::unsupported(
                    "no desktop session inhibitor interface is available",
                ));
            }
            if !held {
                let mut err = failures
                    .into_iter()
                    .max_by_key(|e| e.kind() == ErrorKind::PermissionDenied)
                    .unwrap_or_else(|| {
                        Error::unsupported("no idle-sleep inhibitor mechanism is available")
                    });
                if err.hint().is_none() {
                    err = err.with_hint("run `caffeine doctor` to see which mechanisms were found");
                }
                return Err(err.context("cannot prevent idle sleep"));
            }
            for e in &failures {
                log::warn!("partial idle inhibition: {e}");
            }
        }

        if req.display {
            let (Some(api), Some(conn)) = (env.display_api, &env.session) else {
                return Err(Error::unsupported(
                    "cannot keep the display on: no desktop screensaver interface on the session bus",
                )
                .with_hint("display sleep on a text console is controlled by `setterm --blank`"));
            };
            guard.push(Box::new(DesktopInhibitor::acquire(
                conn.clone(),
                api,
                &req.reason,
                Capability::DisplaySleep,
            )?));
        }

        match req.lid {
            LidSessionMode::System => {}
            mode => {
                if !sysfs::lid_present(self.root()) {
                    return Err(Error::unsupported("this machine has no lid to override"));
                }
                let conn = Self::require_logind(&env)?;
                if mode == LidSessionMode::KeepAwake {
                    guard.push(Box::new(LogindInhibitor::acquire(
                        conn,
                        "handle-lid-switch",
                        &req.reason,
                        Capability::LidSessionOverride,
                    )?));
                } else {
                    guard.push(Box::new(AcScopedLidInhibitor::start(
                        conn,
                        &req.reason,
                        self.root(),
                    )?));
                }
            }
        }
        Ok(Box::new(guard))
    }

    fn lid_status(&self) -> Result<LidStatus> {
        if !sysfs::lid_present(self.root()) {
            return Ok(LidStatus {
                state: LidState::NotPresent,
                source: "sysfs/procfs".into(),
                notes: vec![],
            });
        }
        let env = self.environment();
        if let Ok(conn) = Self::require_logind(&env) {
            match dbus::logind_bool(conn, "LidClosed") {
                Ok(closed) => {
                    return Ok(LidStatus {
                        state: if closed {
                            LidState::Closed
                        } else {
                            LidState::Open
                        },
                        source: "logind LidClosed".into(),
                        notes: vec![],
                    });
                }
                Err(e) => log::debug!("LidClosed unavailable: {e}"),
            }
        }
        Ok(LidStatus {
            state: sysfs::acpi_lid_state(self.root()).unwrap_or(LidState::Unknown),
            source: "/proc/acpi/button/lid".into(),
            notes: vec![],
        })
    }

    fn lid_policy(&self) -> Result<LidPolicy> {
        let env = self.environment();
        let conn = Self::require_logind(&env)?;
        let parse = |prop: &str| -> Result<Option<LidAction>> {
            let v = dbus::logind_string(conn, prop)?;
            Ok((!v.is_empty()).then(|| v.parse().unwrap_or(LidAction::Other(v))))
        };
        let on_battery = parse("HandleLidSwitch")?;
        let mut notes = Vec::new();
        let on_ac = match parse("HandleLidSwitchExternalPower") {
            Ok(Some(a)) => Some(a),
            Ok(None) | Err(_) => {
                notes.push(
                    "HandleLidSwitchExternalPower is unset; the HandleLidSwitch action applies"
                        .into(),
                );
                on_battery.clone()
            }
        };
        let docked = parse("HandleLidSwitchDocked").ok().flatten();
        let overridden_by = dbus::list_inhibitors(conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.what.split(':').any(|w| w == "handle-lid-switch") && i.mode == "block")
            .map(|i| format!("{} ({}, pid {})", i.who, i.why, i.pid))
            .collect::<Vec<_>>();
        if !overridden_by.is_empty() {
            notes.push("lid-switch handling is currently inhibited by the processes listed".into());
        }
        notes.push(
            "persistent changes: set HandleLidSwitch* in /etc/systemd/logind.conf.d/*.conf (root) or \
             in your desktop's power settings"
                .into(),
        );
        Ok(LidPolicy {
            on_ac,
            on_battery,
            docked,
            source: "logind (org.freedesktop.login1)".into(),
            writable: false,
            overridden_by,
            notes,
        })
    }

    fn set_lid_policy(&self, _change: &LidPolicyChange) -> Result<LidPolicy> {
        Err(Error::unsupported(
            "Caffeine does not change the persistent lid policy on Linux (it lives in logind.conf or \
             your desktop's settings)",
        )
        .with_hint("keep running with the lid closed for one session with `caffeine start --lid keep-awake`"))
    }

    fn power_source(&self) -> Result<PowerSource> {
        sysfs::power_source(self.root())
            .ok_or_else(|| Error::unsupported("/sys/class/power_supply is not available"))
    }

    fn diagnose(&self) -> Vec<DiagnosticCheck> {
        let env = self.environment();
        let mut checks = Vec::new();
        let init = sysfs::init_system(self.root());
        checks.push(DiagnosticCheck::new(
            "linux.init",
            CheckStatus::Info,
            format!(
                "init system: {init}; distribution: {}",
                sysfs::distribution(self.root()).unwrap_or_else(|| "unknown".into())
            ),
        ));
        if sysfs::is_wsl(self.root()) {
            checks.push(
                DiagnosticCheck::new(
                    "linux.wsl",
                    CheckStatus::Warn,
                    "running inside WSL: Linux inhibitors do not keep the Windows host awake",
                )
                .with_hint("run the Windows build of caffeine (caffeine.exe) instead"),
            );
        }
        match (&env.system, env.logind) {
            (None, _) => checks.push(
                DiagnosticCheck::new("linux.system-bus", CheckStatus::Fail, "no D-Bus system bus")
                    .with_hint("install and start dbus; containers often lack it"),
            ),
            (Some(_), false) => checks.push(
                DiagnosticCheck::new(
                    "linux.logind",
                    CheckStatus::Warn,
                    "org.freedesktop.login1 is not running",
                )
                .with_hint("install systemd-logind or elogind for system-level inhibitors"),
            ),
            (Some(conn), true) => {
                checks.push(DiagnosticCheck::new(
                    "linux.logind",
                    CheckStatus::Ok,
                    "logind is available",
                ));
                match LogindInhibitor::acquire(
                    conn,
                    "idle",
                    "caffeine doctor probe",
                    Capability::IdleSleep,
                ) {
                    Ok(g) => {
                        drop(g);
                        checks.push(DiagnosticCheck::new(
                            "linux.logind-idle",
                            CheckStatus::Ok,
                            "logind granted and released an idle inhibitor",
                        ));
                    }
                    Err(e) => checks.push(DiagnosticCheck::new(
                        "linux.logind-idle",
                        CheckStatus::Fail,
                        e.to_string(),
                    )),
                }
                if let Ok(action) = dbus::logind_string(conn, "IdleAction") {
                    checks.push(DiagnosticCheck::new(
                        "linux.idle-action",
                        CheckStatus::Info,
                        format!("logind IdleAction={action} (desktop environments run their own idle timers)"),
                    ));
                }
                if sysfs::lid_present(self.root()) {
                    match LogindInhibitor::acquire(
                        conn,
                        "handle-lid-switch",
                        "caffeine doctor probe",
                        Capability::LidSessionOverride,
                    ) {
                        Ok(g) => {
                            drop(g);
                            checks.push(DiagnosticCheck::new(
                                "linux.lid-inhibit",
                                CheckStatus::Ok,
                                "logind permits lid-switch inhibitors for this session",
                            ));
                        }
                        Err(e) => checks.push(
                            DiagnosticCheck::new(
                                "linux.lid-inhibit",
                                CheckStatus::Warn,
                                e.to_string(),
                            )
                            .with_hint("`caffeine start --lid` will not work from this session"),
                        ),
                    }
                }
            }
        }
        match &env.session {
            None => checks.push(DiagnosticCheck::new(
                "linux.session-bus",
                CheckStatus::Info,
                "no session bus: desktop idle and display inhibitors are unavailable",
            )),
            Some(_) => {
                let found: Vec<String> = [env.idle_api, env.display_api]
                    .into_iter()
                    .flatten()
                    .map(|a| a.describe())
                    .collect();
                checks.push(if found.is_empty() {
                    DiagnosticCheck::new(
                        "linux.desktop",
                        CheckStatus::Warn,
                        format!(
                            "desktop {} exposes no known inhibitor interface",
                            desktop_name().unwrap_or_else(|| "(unknown)".into())
                        ),
                    )
                    .with_hint("its own idle timer may still suspend the machine")
                } else {
                    DiagnosticCheck::new(
                        "linux.desktop",
                        CheckStatus::Ok,
                        format!("desktop inhibitors: {}", found.join(", ")),
                    )
                });
            }
        }
        checks
    }
}

/// Holds a `handle-lid-switch` inhibitor only while on AC power, checking
/// the power source every few seconds on a background thread.
struct AcScopedLidInhibitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AcScopedLidInhibitor {
    fn start(conn: &Connection, reason: &str, root: &Path) -> Result<AcScopedLidInhibitor> {
        // Verify up front that polkit allows the inhibitor at all, so
        // permission problems are reported instead of failing silently later.
        drop(LogindInhibitor::acquire(
            conn,
            "handle-lid-switch",
            reason,
            Capability::LidSessionOverride,
        )?);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let conn = conn.clone();
        let reason = reason.to_string();
        let root = root.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("caffeine-lid-ac".into())
            .spawn(move || {
                let mut held: Option<LogindInhibitor> = None;
                let mut logged = false;
                while !thread_stop.load(Ordering::SeqCst) {
                    let on_ac =
                        sysfs::power_source(&root).is_some_and(|p| p.kind == PowerSourceKind::Ac);
                    if on_ac && held.is_none() {
                        match LogindInhibitor::acquire(
                            &conn,
                            "handle-lid-switch",
                            &reason,
                            Capability::LidSessionOverride,
                        ) {
                            Ok(g) => {
                                log::info!("on AC power: lid-switch inhibitor acquired");
                                held = Some(g);
                            }
                            Err(e) if !logged => {
                                logged = true;
                                log::error!("acquiring lid-switch inhibitor: {e}");
                            }
                            Err(_) => {}
                        }
                    } else if !on_ac && held.is_some() {
                        log::info!("on battery: lid-switch inhibitor released");
                        held = None;
                    }
                    for _ in 0..20 {
                        if thread_stop.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .map_err(|e| Error::failure(format!("spawning lid watcher thread: {e}")))?;
        Ok(AcScopedLidInhibitor {
            stop,
            thread: Some(thread),
        })
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AwakeGuard for AcScopedLidInhibitor {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        vec![ActiveAssertion {
            capability: Capability::LidSessionOverride,
            mechanism: "logind Inhibit(handle-lid-switch, block)".into(),
            detail: "held only while on AC power".into(),
        }]
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.shutdown();
        Ok(())
    }
}

impl Drop for AcScopedLidInhibitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> LinuxBackend {
        LinuxBackend::new(&BackendOptions::new(std::env::temp_dir())).unwrap()
    }

    fn caffeine_inhibitors(conn: &Connection) -> Vec<dbus::InhibitorInfo> {
        dbus::list_inhibitors(conn)
            .unwrap()
            .into_iter()
            .filter(|i| i.who == "caffeine" && i.pid == std::process::id())
            .collect()
    }

    /// Talks to the real logind when one is reachable; skipped otherwise
    /// (containers, CI runners without a system bus).
    #[test]
    fn logind_idle_inhibitor_is_listed_and_released() {
        let b = backend();
        let env = b.environment();
        let Ok(conn) = LinuxBackend::require_logind(&env) else {
            eprintln!("skipping: logind not available");
            return;
        };
        let guard = b.acquire(&AwakeRequest::default()).unwrap();
        let held = caffeine_inhibitors(conn);
        assert!(
            held.iter().any(|i| i.what == "idle" && i.mode == "block"),
            "logind should list our idle inhibitor: {held:?}"
        );
        assert!(
            guard
                .assertions()
                .iter()
                .any(|a| a.capability == Capability::IdleSleep)
        );
        guard.release().unwrap();
        // logind processes the closed descriptor asynchronously.
        for _ in 0..50 {
            if caffeine_inhibitors(conn).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("inhibitor still listed after release");
    }

    #[test]
    fn rejects_foreign_backend_names() {
        let mut opts = BackendOptions::new(std::env::temp_dir());
        opts.backend = "power-request".into();
        assert!(LinuxBackend::new(&opts).is_err());
    }

    #[test]
    fn capability_report_is_complete() {
        let report = backend().capabilities();
        assert_eq!(report.platform.os, "linux");
        for cap in Capability::ALL {
            assert!(report.get(cap).is_some(), "{cap} missing from report");
        }
        // Never claim persistent lid writes on Linux.
        assert_eq!(
            report.support(Capability::LidPolicyWrite),
            Support::Unsupported
        );
    }

    #[test]
    fn display_without_desktop_fails_cleanly() {
        let b = LinuxBackend {
            mode: Mode::LogindOnly,
            root: PathBuf::from("/"),
        };
        let req = AwakeRequest {
            idle_sleep: false,
            display: true,
            ..AwakeRequest::default()
        };
        let err = b
            .acquire(&req)
            .err()
            .expect("display needs a desktop session");
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }
}
