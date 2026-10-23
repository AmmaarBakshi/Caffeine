//! Platform-independent power-management model.
//!
//! Caffeine deliberately keeps four concerns apart and never reports one as
//! another:
//!
//! 1. **Idle-sleep prevention** — stop the *idle timer* from suspending the
//!    machine. Explicit suspend (menu, power button, critical battery) is
//!    never blocked.
//! 2. **Display-sleep prevention** — keep the screen from blanking.
//! 3. **Lid-close behavior** — what happens when a laptop lid is closed.
//!    Detecting the lid, reading the policy and changing the policy are three
//!    separate capabilities.
//! 4. **System suspend** — Caffeine never triggers or blocks an explicit
//!    suspend request.
//!
//! Native backends implement [`PowerBackend`]; the session engine only talks
//! to this trait, which keeps it testable with a fake backend.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A discrete power-management capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Prevent the idle timer from suspending the system.
    IdleSleep,
    /// Prevent the display from sleeping or blanking.
    DisplaySleep,
    /// Read whether the lid is open or closed.
    LidState,
    /// Read what the system does when the lid closes.
    LidPolicyRead,
    /// Persistently change the lid-close action (reversible via journal).
    LidPolicyWrite,
    /// Override the lid-close action only while a session is active.
    LidSessionOverride,
    /// Report AC/battery status.
    PowerSource,
    /// Notice that the system resumed from sleep while a session was running.
    ResumeDetection,
}

impl Capability {
    pub const ALL: [Capability; 8] = [
        Capability::IdleSleep,
        Capability::DisplaySleep,
        Capability::LidState,
        Capability::LidPolicyRead,
        Capability::LidPolicyWrite,
        Capability::LidSessionOverride,
        Capability::PowerSource,
        Capability::ResumeDetection,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::IdleSleep => "idle-sleep",
            Capability::DisplaySleep => "display-sleep",
            Capability::LidState => "lid-state",
            Capability::LidPolicyRead => "lid-policy-read",
            Capability::LidPolicyWrite => "lid-policy-write",
            Capability::LidSessionOverride => "lid-session-override",
            Capability::PowerSource => "power-source",
            Capability::ResumeDetection => "resume-detection",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Capability::IdleSleep => "Prevent idle-triggered system sleep",
            Capability::DisplaySleep => "Prevent the display from sleeping",
            Capability::LidState => "Detect whether the lid is open or closed",
            Capability::LidPolicyRead => "Read the lid-close action",
            Capability::LidPolicyWrite => "Persistently change the lid-close action",
            Capability::LidSessionOverride => "Ignore lid close only while a session runs",
            Capability::PowerSource => "Report AC / battery status",
            Capability::ResumeDetection => "Detect resume from sleep during a session",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How well a capability is supported on this machine right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    /// Works as described.
    Supported,
    /// Works with caveats explained in the detail text.
    Limited,
    /// Exists but the current user lacks permission.
    RequiresPermission,
    /// Not available on this platform, hardware or environment.
    Unsupported,
}

impl Support {
    /// Whether an operation relying on this capability may be attempted.
    pub fn is_usable(self) -> bool {
        matches!(self, Support::Supported | Support::Limited)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Support::Supported => "supported",
            Support::Limited => "limited",
            Support::RequiresPermission => "requires-permission",
            Support::Unsupported => "unsupported",
        }
    }
}

/// Status of one capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub capability: Capability,
    pub support: Support,
    /// Which native mechanism provides it, e.g. `PowerSetRequest`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<String>,
    /// Human-readable explanation, especially for limits and absence.
    pub detail: String,
}

impl CapabilityStatus {
    pub fn new(
        capability: Capability,
        support: Support,
        mechanism: Option<&str>,
        detail: impl Into<String>,
    ) -> Self {
        CapabilityStatus {
            capability,
            support,
            mechanism: mechanism.map(str::to_string),
            detail: detail.into(),
        }
    }

    pub fn unsupported(capability: Capability, detail: impl Into<String>) -> Self {
        Self::new(capability, Support::Unsupported, None, detail)
    }
}

/// Facts about the running platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInfo {
    /// `windows`, `macos` or `linux`.
    pub os: String,
    /// OS version or distribution name, when known.
    pub os_version: Option<String>,
    pub arch: String,
    /// Backend name selected for this run.
    pub backend: String,
    /// Init system on Linux (`systemd`, `openrc`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_system: Option<String>,
    /// Desktop environment, when one is detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    /// Extra facts (e.g. "Modern Standby").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Full capability report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub platform: PlatformInfo,
    pub capabilities: Vec<CapabilityStatus>,
}

impl CapabilityReport {
    pub fn get(&self, cap: Capability) -> Option<&CapabilityStatus> {
        self.capabilities.iter().find(|c| c.capability == cap)
    }

    pub fn support(&self, cap: Capability) -> Support {
        self.get(cap).map_or(Support::Unsupported, |c| c.support)
    }

    pub fn is_usable(&self, cap: Capability) -> bool {
        self.support(cap).is_usable()
    }
}

/// Physical lid state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LidState {
    Open,
    Closed,
    /// The machine has no lid (desktop, server, VM).
    NotPresent,
    /// The platform cannot determine the lid state.
    Unknown,
}

impl LidState {
    pub fn as_str(self) -> &'static str {
        match self {
            LidState::Open => "open",
            LidState::Closed => "closed",
            LidState::NotPresent => "not-present",
            LidState::Unknown => "unknown",
        }
    }
}

/// Result of a lid-state query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LidStatus {
    pub state: LidState,
    /// The native source of the answer.
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// What the system does when the lid closes. Serialized as a kebab-case
/// string; unknown platform values round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LidAction {
    DoNothing,
    Sleep,
    Hibernate,
    HybridSleep,
    SuspendThenHibernate,
    ShutDown,
    Lock,
    /// A platform-specific value Caffeine does not model.
    Other(String),
}

impl LidAction {
    pub fn as_str(&self) -> &str {
        match self {
            LidAction::DoNothing => "do-nothing",
            LidAction::Sleep => "sleep",
            LidAction::Hibernate => "hibernate",
            LidAction::HybridSleep => "hybrid-sleep",
            LidAction::SuspendThenHibernate => "suspend-then-hibernate",
            LidAction::ShutDown => "shut-down",
            LidAction::Lock => "lock",
            LidAction::Other(s) => s,
        }
    }
}

impl fmt::Display for LidAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LidAction {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "do-nothing" | "nothing" | "ignore" | "none" => LidAction::DoNothing,
            "sleep" | "suspend" => LidAction::Sleep,
            "hibernate" => LidAction::Hibernate,
            "hybrid-sleep" => LidAction::HybridSleep,
            "suspend-then-hibernate" => LidAction::SuspendThenHibernate,
            "shut-down" | "shutdown" | "poweroff" => LidAction::ShutDown,
            "lock" => LidAction::Lock,
            other => {
                return Err(Error::usage(format!(
                    "unknown lid action \"{other}\" (expected do-nothing, sleep, hibernate or shut-down)"
                )));
            }
        })
    }
}

impl Serialize for LidAction {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LidAction {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(s.parse().unwrap_or(LidAction::Other(s)))
    }
}

/// The lid-close policy currently in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LidPolicy {
    /// Action while on external power.
    pub on_ac: Option<LidAction>,
    /// Action while on battery.
    pub on_battery: Option<LidAction>,
    /// Action while docked / with external displays, where the platform
    /// distinguishes this case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docked: Option<LidAction>,
    /// Where the policy was read from.
    pub source: String,
    /// Whether Caffeine can change this policy persistently.
    pub writable: bool,
    /// Other processes currently overriding lid handling, if known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overridden_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A requested persistent change to the lid policy. `None` leaves the
/// corresponding value untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LidPolicyChange {
    pub on_ac: Option<LidAction>,
    pub on_battery: Option<LidAction>,
}

impl LidPolicyChange {
    pub fn is_empty(&self) -> bool {
        self.on_ac.is_none() && self.on_battery.is_none()
    }
}

/// When to override the lid-close action during a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LidSessionMode {
    /// Leave lid handling to the operating system.
    #[default]
    System,
    /// Keep running with the lid closed, but only on external power where the
    /// platform can distinguish it.
    KeepAwakeOnAc,
    /// Keep running with the lid closed regardless of power source.
    KeepAwake,
}

impl LidSessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LidSessionMode::System => "system",
            LidSessionMode::KeepAwakeOnAc => "keep-awake-on-ac",
            LidSessionMode::KeepAwake => "keep-awake",
        }
    }
}

impl FromStr for LidSessionMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" | "off" => Ok(LidSessionMode::System),
            "keep-awake-on-ac" | "ac" => Ok(LidSessionMode::KeepAwakeOnAc),
            "keep-awake" | "always" => Ok(LidSessionMode::KeepAwake),
            other => Err(Error::usage(format!(
                "unknown lid mode \"{other}\" (expected system, keep-awake-on-ac or keep-awake)"
            ))),
        }
    }
}

/// What a session asks the operating system to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwakeRequest {
    pub idle_sleep: bool,
    pub display: bool,
    pub lid: LidSessionMode,
    /// Shown to the OS where it records a reason (e.g. `powercfg /requests`).
    pub reason: String,
}

impl Default for AwakeRequest {
    fn default() -> Self {
        AwakeRequest {
            idle_sleep: true,
            display: false,
            lid: LidSessionMode::System,
            reason: "caffeine: keeping the system awake on user request".to_string(),
        }
    }
}

impl AwakeRequest {
    /// Whether the request asks for anything at all.
    pub fn is_empty(&self) -> bool {
        !self.idle_sleep && !self.display && self.lid == LidSessionMode::System
    }

    /// Capabilities this request needs.
    pub fn required_capabilities(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        if self.idle_sleep {
            caps.push(Capability::IdleSleep);
        }
        if self.display {
            caps.push(Capability::DisplaySleep);
        }
        if self.lid != LidSessionMode::System {
            caps.push(Capability::LidSessionOverride);
        }
        caps
    }
}

/// One native assertion held by an active guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveAssertion {
    pub capability: Capability,
    pub mechanism: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// RAII handle for everything a session acquired from the OS.
///
/// Dropping the guard must release every assertion and restore every setting
/// it changed. [`AwakeGuard::release`] does the same but reports failures.
pub trait AwakeGuard: Send {
    /// The assertions currently held.
    fn assertions(&self) -> Vec<ActiveAssertion>;

    /// Re-validate held assertions, e.g. after resume. Backends that hold
    /// connection-bound inhibitors may re-acquire them here.
    fn refresh(&mut self) -> Result<()> {
        Ok(())
    }

    /// Release everything, reporting the first failure encountered. All
    /// releases are still attempted when one fails.
    fn release(self: Box<Self>) -> Result<()>;
}

/// Power source information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerSourceKind {
    Ac,
    Battery,
    Ups,
    Unknown,
}

impl PowerSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PowerSourceKind::Ac => "ac",
            PowerSourceKind::Battery => "battery",
            PowerSourceKind::Ups => "ups",
            PowerSourceKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerSource {
    pub kind: PowerSourceKind,
    /// Whether a battery is installed at all.
    pub battery_present: bool,
    pub battery_percent: Option<u8>,
    pub charging: Option<bool>,
}

impl PowerSource {
    pub fn unknown() -> Self {
        PowerSource {
            kind: PowerSourceKind::Unknown,
            battery_present: false,
            battery_percent: None,
            charging: None,
        }
    }
}

/// Severity of a diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Info => "info",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

/// One `caffeine doctor` check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl DiagnosticCheck {
    pub fn new(id: impl Into<String>, status: CheckStatus, summary: impl Into<String>) -> Self {
        DiagnosticCheck {
            id: id.into(),
            status,
            summary: summary.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// A modification left behind by an earlier run that can be undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryItem {
    pub id: String,
    pub description: String,
    /// When the modification was made.
    pub created_at: u64,
}

/// A native power-management backend.
pub trait PowerBackend: Send + Sync {
    /// Short identifier, e.g. `windows-power-request`.
    fn name(&self) -> &str;

    /// Probe the machine and describe what is available.
    fn capabilities(&self) -> CapabilityReport;

    /// Acquire the assertions described by `request`. Must fail (not
    /// partially succeed silently) when any requested part cannot be
    /// provided; whatever was acquired before the failure is released.
    fn acquire(&self, request: &AwakeRequest) -> Result<Box<dyn AwakeGuard>>;

    fn lid_status(&self) -> Result<LidStatus>;

    fn lid_policy(&self) -> Result<LidPolicy>;

    /// Persistently change the lid policy. The previous values must be
    /// journaled so [`PowerBackend::recover`] can restore them.
    fn set_lid_policy(&self, change: &LidPolicyChange) -> Result<LidPolicy>;

    fn power_source(&self) -> Result<PowerSource>;

    /// Platform-specific diagnostics for `caffeine doctor`.
    fn diagnose(&self) -> Vec<DiagnosticCheck>;

    /// Modifications that are pending restoration (e.g. after a crash).
    fn pending_recovery(&self) -> Result<Vec<RecoveryItem>> {
        Ok(Vec::new())
    }

    /// Restore every pending modification. Returns a description of each
    /// restored item.
    fn recover(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lid_action_parsing_accepts_platform_spellings() {
        assert_eq!("ignore".parse::<LidAction>().unwrap(), LidAction::DoNothing);
        assert_eq!("suspend".parse::<LidAction>().unwrap(), LidAction::Sleep);
        assert_eq!("shutdown".parse::<LidAction>().unwrap(), LidAction::ShutDown);
        assert!("explode".parse::<LidAction>().is_err());
    }

    #[test]
    fn request_lists_required_capabilities() {
        let req = AwakeRequest {
            display: true,
            lid: LidSessionMode::KeepAwakeOnAc,
            ..AwakeRequest::default()
        };
        assert_eq!(
            req.required_capabilities(),
            vec![Capability::IdleSleep, Capability::DisplaySleep, Capability::LidSessionOverride]
        );
        let none = AwakeRequest {
            idle_sleep: false,
            ..AwakeRequest::default()
        };
        assert!(none.is_empty());
    }

    #[test]
    fn report_lookup_defaults_to_unsupported() {
        let report = CapabilityReport::default();
        assert_eq!(report.support(Capability::IdleSleep), Support::Unsupported);
        assert!(!report.is_usable(Capability::IdleSleep));
    }

    #[test]
    fn capability_ids_are_kebab_case_in_json() {
        let json = serde_json::to_string(&Capability::LidSessionOverride).unwrap();
        assert_eq!(json, "\"lid-session-override\"");
        for cap in Capability::ALL {
            let s = serde_json::to_string(&cap).unwrap();
            assert_eq!(s.trim_matches('"'), cap.as_str());
        }
    }
}
