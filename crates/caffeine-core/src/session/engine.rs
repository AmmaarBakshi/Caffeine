//! The session host loop.
//!
//! A host owns exactly one session. It takes the host lock, acquires native
//! assertions through a [`PowerBackend`], publishes `session.json`, and then
//! polls for its end conditions:
//!
//! * a stop request addressed to its session id,
//! * the termination flag (Ctrl+C / SIGTERM / SIGHUP),
//! * the wall-clock deadline,
//! * the battery policy,
//! * an optional caller-supplied check (used by `caffeine run`).
//!
//! The loop also detects suspected suspend/resume: if far more wall-clock
//! time passed between two ticks than the tick interval, the process was not
//! scheduled — almost always because the machine slept. The host logs it,
//! asks the guard to refresh its assertions and re-evaluates the deadline
//! immediately.
//!
//! Whatever ends the loop, the guard is released, the state file is removed
//! and the end is recorded. A forced kill skips this cleanup, but the OS
//! releases process-bound assertions by itself and the stale state is
//! detected by the next `caffeine` command through the host lock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::config::BatteryAction;
use crate::duration::SessionLength;
use crate::error::{Error, Result};
use crate::power::{AwakeGuard, AwakeRequest, PowerBackend, PowerSourceKind};
use crate::time::unix_now;

use super::store::{
    BatteryWatch, EndReason, STATE_FORMAT, SessionMode, SessionRecord, SessionState, SessionStore,
    new_session_id,
};

/// Parameters for one session.
#[derive(Debug, Clone)]
pub struct HostOptions {
    pub request: AwakeRequest,
    pub length: SessionLength,
    pub mode: SessionMode,
    pub battery: BatteryWatch,
    /// Polling interval for end conditions.
    pub tick: Duration,
    /// How often the battery is checked.
    pub battery_interval: Duration,
    /// How often `heartbeat_at` is refreshed in `session.json`.
    pub heartbeat_interval: Duration,
    /// A wall-clock gap larger than this between ticks counts as a resume.
    pub resume_gap: Duration,
    pub record_last_session: bool,
    pub log_file: Option<std::path::PathBuf>,
    /// Wall-clock source (UNIX seconds); replaceable for tests.
    pub clock: Clock,
}

/// A wall-clock source.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> u64 + Send + Sync>);

impl Clock {
    pub fn system() -> Clock {
        Clock(Arc::new(unix_now))
    }

    pub fn custom(f: impl Fn() -> u64 + Send + Sync + 'static) -> Clock {
        Clock(Arc::new(f))
    }

    pub fn now(&self) -> u64 {
        (self.0)()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

impl HostOptions {
    pub fn new(request: AwakeRequest, length: SessionLength, mode: SessionMode) -> HostOptions {
        HostOptions {
            request,
            length,
            mode,
            battery: BatteryWatch::default(),
            tick: Duration::from_millis(250),
            battery_interval: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(30),
            resume_gap: Duration::from_secs(15),
            record_last_session: true,
            log_file: None,
            clock: Clock::system(),
        }
    }
}

/// Runs a session to completion.
pub struct SessionHost<'a> {
    backend: &'a dyn PowerBackend,
    store: &'a SessionStore,
    options: HostOptions,
    terminate: Arc<AtomicBool>,
}

impl std::fmt::Debug for SessionHost<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHost")
            .field("backend", &self.backend.name())
            .field("options", &self.options)
            .finish()
    }
}

impl<'a> SessionHost<'a> {
    pub fn new(
        backend: &'a dyn PowerBackend,
        store: &'a SessionStore,
        options: HostOptions,
        terminate: Arc<AtomicBool>,
    ) -> SessionHost<'a> {
        SessionHost {
            backend,
            store,
            options,
            terminate,
        }
    }

    /// Run the session.
    ///
    /// `on_ready` is called once the assertions are held and the state is
    /// published. `extra_check` is polled every tick and may end the session.
    /// Errors before `on_ready` mean nothing was acquired and no state was
    /// left behind.
    pub fn run(
        self,
        on_ready: &mut dyn FnMut(&SessionState),
        extra_check: &mut dyn FnMut() -> Option<EndReason>,
    ) -> Result<SessionRecord> {
        if self.options.request.is_empty() {
            return Err(Error::usage(
                "nothing to do: idle-sleep prevention, display and lid overrides are all disabled",
            ));
        }
        let _lock = self.store.acquire_host_lock()?;
        if let Ok(Some(stale)) = self.store.read_state() {
            log::warn!(
                "removing stale state of session {} (pid {})",
                stale.id,
                stale.pid
            );
        }
        self.store.remove_state()?;
        self.store.clear_stop_request()?;

        let mut guard = self.backend.acquire(&self.options.request)?;
        let started_at = self.options.clock.now();
        let mut state = SessionState {
            format: STATE_FORMAT,
            id: new_session_id(),
            pid: std::process::id(),
            mode: self.options.mode.clone(),
            backend: self.backend.name().to_string(),
            started_at,
            expires_at: self
                .options
                .length
                .as_duration()
                .map(|d| started_at + d.as_secs()),
            request: self.options.request.clone(),
            assertions: guard.assertions(),
            battery: self.options.battery,
            heartbeat_at: started_at,
            resumes: 0,
            last_resume_at: None,
            log_file: self.options.log_file.clone(),
        };
        if let Err(e) = self.store.write_state(&state) {
            if let Err(release) = guard.release() {
                log::error!("releasing assertions after a state write failure: {release}");
            }
            return Err(e.context("publishing session state"));
        }
        log::info!(
            "session {} started (mode {}, expires {:?}, assertions {:?})",
            state.id,
            state.mode.describe(),
            state.expires_at,
            state.assertions
        );
        on_ready(&state);

        let end = self.wait(&mut state, guard.as_mut(), extra_check);
        log::info!("session {} ending: {}", state.id, end.describe());

        let release_error = guard.release().err().map(|e| {
            log::error!("releasing assertions for session {}: {e}", state.id);
            e.to_string()
        });
        let record = SessionRecord {
            id: state.id.clone(),
            mode: state.mode.clone(),
            started_at: state.started_at,
            ended_at: self.options.clock.now(),
            end,
            resumes: state.resumes,
            release_error,
        };
        if let Err(e) = self.store.remove_state() {
            log::error!("removing session state: {e}");
        }
        let _ = self.store.clear_stop_request();
        if self.options.record_last_session
            && let Err(e) = self.store.write_last_session(&record)
        {
            log::warn!("recording the finished session: {e}");
        }
        Ok(record)
    }

    fn wait(
        &self,
        state: &mut SessionState,
        guard: &mut dyn AwakeGuard,
        extra_check: &mut dyn FnMut() -> Option<EndReason>,
    ) -> EndReason {
        let opts = &self.options;
        let mut last_wall = opts.clock.now();
        let mut last_tick = Instant::now();
        let mut last_heartbeat = Instant::now();
        // Check the battery on the first tick.
        let mut last_battery: Option<Instant> = None;
        let mut battery_error_logged = false;

        loop {
            if self.terminate.load(Ordering::SeqCst) {
                return EndReason::Terminated;
            }
            if self.store.take_stop_request(&state.id) {
                return EndReason::Stopped;
            }
            if let Some(reason) = extra_check() {
                return reason;
            }

            let now = opts.clock.now();
            // The loop sleeps for one short tick, so a large gap on either
            // clock means the process was not scheduled: almost certainly the
            // machine was suspended. (Monotonic clocks stop during sleep on
            // Linux and macOS, hence checking wall-clock time as well.)
            let gap = Duration::from_secs(now.saturating_sub(last_wall)).max(last_tick.elapsed());
            if gap > opts.resume_gap {
                state.resumes += 1;
                state.last_resume_at = Some(now);
                log::info!(
                    "session {}: system appears to have resumed after ~{}s",
                    state.id,
                    gap.as_secs()
                );
                if let Err(e) = guard.refresh() {
                    log::error!("refreshing assertions after resume: {e}");
                }
                state.assertions = guard.assertions();
                last_battery = None;
                last_heartbeat = Instant::now() - opts.heartbeat_interval;
            }
            last_wall = now;
            last_tick = Instant::now();

            if let Some(expires) = state.expires_at
                && now >= expires
            {
                return EndReason::Expired;
            }

            if opts.battery.is_active()
                && last_battery.is_none_or(|t| t.elapsed() >= opts.battery_interval)
            {
                last_battery = Some(Instant::now());
                match self.backend.power_source() {
                    Ok(src) if src.kind == PowerSourceKind::Battery => {
                        if opts.battery.on_battery == BatteryAction::Stop {
                            return EndReason::SwitchedToBattery;
                        }
                        if let Some(percent) = src.battery_percent
                            && opts.battery.threshold > 0
                            && percent < opts.battery.threshold
                        {
                            return EndReason::BatteryThreshold {
                                percent,
                                threshold: opts.battery.threshold,
                            };
                        }
                    }
                    Ok(_) => {}
                    Err(e) if !battery_error_logged => {
                        battery_error_logged = true;
                        log::warn!("battery policy disabled: cannot read power source: {e}");
                    }
                    Err(_) => {}
                }
            }

            if last_heartbeat.elapsed() >= opts.heartbeat_interval {
                last_heartbeat = Instant::now();
                state.heartbeat_at = now;
                if let Err(e) = self.store.write_state(state) {
                    log::warn!("updating session heartbeat: {e}");
                }
            }

            std::thread::sleep(opts.tick);
        }
    }
}

#[cfg(test)]
mod tests;
