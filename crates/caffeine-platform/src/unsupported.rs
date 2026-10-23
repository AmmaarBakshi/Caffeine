//! Fallback for platforms without a native backend (e.g. the BSDs). Every
//! operation reports `unsupported` instead of pretending to work.

use caffeine_core::power::*;
use caffeine_core::{Error, Result};

use crate::BackendOptions;

#[derive(Debug)]
pub struct UnsupportedBackend;

impl UnsupportedBackend {
    pub fn new(_options: &BackendOptions) -> Result<UnsupportedBackend> {
        Ok(UnsupportedBackend)
    }
}

fn err() -> Error {
    Error::unsupported(format!(
        "Caffeine has no power-management backend for {}",
        std::env::consts::OS
    ))
}

impl PowerBackend for UnsupportedBackend {
    fn name(&self) -> &str {
        "unsupported"
    }

    fn capabilities(&self) -> CapabilityReport {
        CapabilityReport {
            platform: PlatformInfo {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                backend: "unsupported".into(),
                ..PlatformInfo::default()
            },
            capabilities: Capability::ALL
                .iter()
                .map(|c| CapabilityStatus::unsupported(*c, "no native backend for this platform"))
                .collect(),
        }
    }

    fn acquire(&self, _request: &AwakeRequest) -> Result<Box<dyn AwakeGuard>> {
        Err(err())
    }

    fn lid_status(&self) -> Result<LidStatus> {
        Err(err())
    }

    fn lid_policy(&self) -> Result<LidPolicy> {
        Err(err())
    }

    fn set_lid_policy(&self, _change: &LidPolicyChange) -> Result<LidPolicy> {
        Err(err())
    }

    fn power_source(&self) -> Result<PowerSource> {
        Err(err())
    }

    fn diagnose(&self) -> Vec<DiagnosticCheck> {
        vec![DiagnosticCheck::new("platform", CheckStatus::Fail, err().to_string())]
    }
}
