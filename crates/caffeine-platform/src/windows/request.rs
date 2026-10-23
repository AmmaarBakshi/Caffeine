//! Power requests (`PowerCreateRequest` / `PowerSetRequest`) and the legacy
//! `SetThreadExecutionState` mechanism.
//!
//! Power requests are kernel objects owned by the process: they show up in
//! `powercfg /requests` with Caffeine's reason string and are cleared by the
//! kernel when the handle closes, including on abnormal process exit.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, POWER_REQUEST_TYPE,
    PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
    PowerRequestSystemRequired, PowerSetRequest, SetThreadExecutionState,
};
use windows_sys::Win32::System::Threading::{
    POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};

use caffeine_core::power::{ActiveAssertion, AwakeGuard, Capability};
use caffeine_core::{Error, Result};

use super::util::{last_error, wide};

const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;

/// A power request object with one or more request types set.
pub struct PowerRequest {
    handle: HANDLE,
    active: Vec<POWER_REQUEST_TYPE>,
    // Keep the reason string alive for the lifetime of the request.
    _reason: Vec<u16>,
    closed: bool,
}

// SAFETY: a power request HANDLE is a process-wide kernel handle that may be
// used and closed from any thread.
unsafe impl Send for PowerRequest {}

impl PowerRequest {
    pub fn create(reason: &str) -> Result<PowerRequest> {
        let mut reason_w = wide(reason);
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: reason_w.as_mut_ptr(),
            },
        };
        // SAFETY: `context` and the string it points to are valid for the call.
        let handle = unsafe { PowerCreateRequest(&context) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(last_error("PowerCreateRequest"));
        }
        Ok(PowerRequest {
            handle,
            active: Vec::new(),
            _reason: reason_w,
            closed: false,
        })
    }

    pub fn set(&mut self, kind: POWER_REQUEST_TYPE) -> Result<()> {
        // SAFETY: the handle is a live power request.
        if unsafe { PowerSetRequest(self.handle, kind) } == 0 {
            return Err(last_error(&format!("PowerSetRequest({})", request_name(kind))));
        }
        self.active.push(kind);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut first_err = None;
        for kind in self.active.drain(..).rev() {
            // SAFETY: the handle is still open.
            if unsafe { PowerClearRequest(self.handle, kind) } == 0 {
                first_err.get_or_insert_with(|| {
                    last_error(&format!("PowerClearRequest({})", request_name(kind)))
                });
            }
        }
        // SAFETY: the handle was returned by PowerCreateRequest and is closed once.
        if unsafe { CloseHandle(self.handle) } == 0 {
            first_err.get_or_insert_with(|| last_error("CloseHandle(power request)"));
        }
        first_err.map_or(Ok(()), Err)
    }
}

fn request_name(kind: POWER_REQUEST_TYPE) -> &'static str {
    if kind == PowerRequestSystemRequired {
        "SystemRequired"
    } else if kind == PowerRequestDisplayRequired {
        "DisplayRequired"
    } else {
        "other"
    }
}

impl AwakeGuard for PowerRequest {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        self.active
            .iter()
            .map(|kind| ActiveAssertion {
                capability: if *kind == PowerRequestDisplayRequired {
                    Capability::DisplaySleep
                } else {
                    Capability::IdleSleep
                },
                mechanism: format!("PowerSetRequest({})", request_name(*kind)),
                detail: "visible in `powercfg /requests` (elevated)".into(),
            })
            .collect()
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.close()
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            log::error!("{e}");
        }
    }
}

/// Acquire a power request for the given needs. All-or-nothing: on failure
/// the partially configured request is released by `Drop`.
pub fn power_request(reason: &str, idle: bool, display: bool) -> Result<PowerRequest> {
    let mut req = PowerRequest::create(reason)?;
    if idle {
        req.set(PowerRequestSystemRequired)?;
    }
    if display {
        req.set(PowerRequestDisplayRequired)?;
    }
    Ok(req)
}

/// `SetThreadExecutionState` guard. The state belongs to the calling
/// thread, so it must be released on the thread that acquired it.
pub struct ExecutionState {
    flags: u32,
    thread: std::thread::ThreadId,
    released: bool,
}

impl ExecutionState {
    pub fn acquire(idle: bool, display: bool) -> Result<ExecutionState> {
        let mut flags = ES_CONTINUOUS;
        if idle {
            flags |= ES_SYSTEM_REQUIRED;
        }
        if display {
            flags |= ES_DISPLAY_REQUIRED;
        }
        // SAFETY: no pointers involved.
        if unsafe { SetThreadExecutionState(flags) } == 0 {
            return Err(Error::failure("SetThreadExecutionState failed"));
        }
        Ok(ExecutionState {
            flags,
            thread: std::thread::current().id(),
            released: false,
        })
    }

    fn reset(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        if std::thread::current().id() != self.thread {
            return Err(Error::failure(
                "SetThreadExecutionState must be reset on the thread that set it",
            ));
        }
        // SAFETY: no pointers involved.
        if unsafe { SetThreadExecutionState(ES_CONTINUOUS) } == 0 {
            return Err(Error::failure("SetThreadExecutionState(ES_CONTINUOUS) failed"));
        }
        Ok(())
    }
}

impl AwakeGuard for ExecutionState {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        let mut out = Vec::new();
        if self.flags & ES_SYSTEM_REQUIRED != 0 {
            out.push(ActiveAssertion {
                capability: Capability::IdleSleep,
                mechanism: "SetThreadExecutionState(ES_SYSTEM_REQUIRED)".into(),
                detail: String::new(),
            });
        }
        if self.flags & ES_DISPLAY_REQUIRED != 0 {
            out.push(ActiveAssertion {
                capability: Capability::DisplaySleep,
                mechanism: "SetThreadExecutionState(ES_DISPLAY_REQUIRED)".into(),
                detail: String::new(),
            });
        }
        out
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.reset()
    }
}

impl Drop for ExecutionState {
    fn drop(&mut self) {
        if let Err(e) = self.reset() {
            log::error!("{e}");
        }
    }
}
