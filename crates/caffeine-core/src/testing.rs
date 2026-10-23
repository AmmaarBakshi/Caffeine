//! An in-memory [`PowerBackend`] for tests (enabled by the `testing`
//! feature). It records every acquire and release so lifecycle tests can
//! assert that nothing leaks. It is never used by the shipped binary.

use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::power::*;

/// Shared, inspectable state of a [`FakeBackend`].
#[derive(Debug, Default)]
pub struct FakeState {
    pub acquired: u32,
    pub released: u32,
    pub refreshed: u32,
    pub fail_acquire: Option<String>,
    pub fail_release: Option<String>,
    pub power: Option<PowerSource>,
}

impl FakeState {
    /// Number of guards currently held.
    pub fn held(&self) -> i64 {
        i64::from(self.acquired) - i64::from(self.released)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FakeBackend {
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeBackend {
    pub fn new() -> FakeBackend {
        FakeBackend::default()
    }

    pub fn snapshot<R>(&self, f: impl FnOnce(&FakeState) -> R) -> R {
        f(&self.state.lock().unwrap())
    }

    pub fn update(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.state.lock().unwrap())
    }
}

struct FakeGuard {
    state: Arc<Mutex<FakeState>>,
    request: AwakeRequest,
    released: bool,
}

impl FakeGuard {
    fn do_release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let mut s = self.state.lock().unwrap();
        s.released += 1;
        match &s.fail_release {
            Some(msg) => Err(Error::failure(msg.clone())),
            None => Ok(()),
        }
    }
}

impl AwakeGuard for FakeGuard {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        self.request
            .required_capabilities()
            .into_iter()
            .map(|capability| ActiveAssertion {
                capability,
                mechanism: "fake".into(),
                detail: String::new(),
            })
            .collect()
    }

    fn refresh(&mut self) -> Result<()> {
        self.state.lock().unwrap().refreshed += 1;
        Ok(())
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.do_release()
    }
}

impl Drop for FakeGuard {
    fn drop(&mut self) {
        let _ = self.do_release();
    }
}

impl PowerBackend for FakeBackend {
    fn name(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> CapabilityReport {
        CapabilityReport {
            platform: PlatformInfo {
                os: "test".into(),
                backend: "fake".into(),
                ..PlatformInfo::default()
            },
            capabilities: Capability::ALL
                .iter()
                .map(|c| {
                    CapabilityStatus::new(*c, Support::Supported, Some("fake"), "test backend")
                })
                .collect(),
        }
    }

    fn acquire(&self, request: &AwakeRequest) -> Result<Box<dyn AwakeGuard>> {
        let mut s = self.state.lock().unwrap();
        if let Some(msg) = &s.fail_acquire {
            return Err(Error::permission(msg.clone()));
        }
        s.acquired += 1;
        Ok(Box::new(FakeGuard {
            state: Arc::clone(&self.state),
            request: request.clone(),
            released: false,
        }))
    }

    fn lid_status(&self) -> Result<LidStatus> {
        Ok(LidStatus {
            state: LidState::NotPresent,
            source: "fake".into(),
            notes: vec![],
        })
    }

    fn lid_policy(&self) -> Result<LidPolicy> {
        Err(Error::unsupported("fake backend has no lid"))
    }

    fn set_lid_policy(&self, _change: &LidPolicyChange) -> Result<LidPolicy> {
        Err(Error::unsupported("fake backend has no lid"))
    }

    fn power_source(&self) -> Result<PowerSource> {
        self.state
            .lock()
            .unwrap()
            .power
            .ok_or_else(|| Error::unsupported("no power source configured"))
    }

    fn diagnose(&self) -> Vec<DiagnosticCheck> {
        vec![DiagnosticCheck::new(
            "fake",
            CheckStatus::Ok,
            "fake backend",
        )]
    }
}
