use std::sync::atomic::AtomicU64;
use std::thread;

use super::*;
use crate::power::{PowerSource, PowerSourceKind};
use crate::session::store::Probe;
use crate::testing::FakeBackend;

struct Fixture {
    _dir: tempfile::TempDir,
    store: SessionStore,
    backend: FakeBackend,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::with_dirs(&dir.path().join("run"), &dir.path().join("data"));
    Fixture {
        _dir: dir,
        store,
        backend: FakeBackend::new(),
    }
}

fn options(length: SessionLength) -> HostOptions {
    let mut o = HostOptions::new(AwakeRequest::default(), length, SessionMode::Foreground);
    o.tick = Duration::from_millis(10);
    o
}

fn run_host(
    f: &Fixture,
    opts: HostOptions,
    terminate: Arc<AtomicBool>,
    mut extra: impl FnMut() -> Option<EndReason>,
) -> (Result<SessionRecord>, Option<SessionState>) {
    let mut ready = None;
    let result = SessionHost::new(&f.backend, &f.store, opts, terminate)
        .run(&mut |s| ready = Some(s.clone()), &mut extra);
    (result, ready)
}

#[test]
fn stop_request_ends_session_and_cleans_up() {
    let f = fixture();
    let store = f.store.clone();
    let stopper = thread::spawn(move || {
        // Wait for the session to publish its state, then stop it.
        loop {
            if let Ok(Probe::Active(s)) = store.probe() {
                store.request_stop(&s.id).unwrap();
                assert!(store.wait_for_end(&s.id, Duration::from_secs(5)).unwrap());
                return s.id;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let (result, ready) = run_host(
        &f,
        options(SessionLength::Indefinite),
        Arc::new(AtomicBool::new(false)),
        || None,
    );
    let record = result.unwrap();
    let id = stopper.join().unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.end, EndReason::Stopped);
    assert_eq!(ready.unwrap().assertions.len(), 1);
    assert_eq!(f.store.probe().unwrap(), Probe::Inactive);
    assert_eq!(f.backend.snapshot(|s| s.held()), 0, "guard must be released");
    assert_eq!(f.store.read_last_session().unwrap().unwrap().end, EndReason::Stopped);
}

#[test]
fn duration_expiry_uses_wall_clock() {
    let f = fixture();
    let now = Arc::new(AtomicU64::new(1_000));
    let clock_now = Arc::clone(&now);
    let mut opts = options(SessionLength::Fixed(Duration::from_secs(60)));
    opts.clock = Clock::custom(move || clock_now.load(Ordering::SeqCst));
    let mut ticks = 0;
    let (result, ready) = run_host(&f, opts, Arc::new(AtomicBool::new(false)), || {
        ticks += 1;
        // Advance 10 seconds of wall-clock time per tick.
        now.fetch_add(10, Ordering::SeqCst);
        None
    });
    let record = result.unwrap();
    assert_eq!(record.end, EndReason::Expired);
    assert_eq!(ready.unwrap().expires_at, Some(1_060));
    assert!((6..=8).contains(&ticks), "expired after {ticks} ticks");
    assert_eq!(f.backend.snapshot(|s| s.held()), 0);
}

#[test]
fn resume_is_detected_and_expired_deadline_ends_session_immediately() {
    let f = fixture();
    let now = Arc::new(AtomicU64::new(1_000));
    let clock_now = Arc::clone(&now);
    let mut opts = options(SessionLength::Fixed(Duration::from_secs(600)));
    opts.clock = Clock::custom(move || clock_now.load(Ordering::SeqCst));
    let mut first = true;
    let (result, _) = run_host(&f, opts, Arc::new(AtomicBool::new(false)), || {
        if first {
            first = false;
            // Simulate the machine sleeping for an hour.
            now.fetch_add(3_600, Ordering::SeqCst);
        }
        None
    });
    let record = result.unwrap();
    assert_eq!(record.end, EndReason::Expired);
    assert_eq!(record.resumes, 1);
    assert_eq!(f.backend.snapshot(|s| s.refreshed), 1);
}

#[test]
fn termination_flag_ends_session() {
    let f = fixture();
    let flag = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&flag);
    let (result, _) = run_host(&f, options(SessionLength::Indefinite), flag, || {
        setter.store(true, Ordering::SeqCst);
        None
    });
    assert_eq!(result.unwrap().end, EndReason::Terminated);
    assert_eq!(f.backend.snapshot(|s| s.held()), 0);
}

#[test]
fn acquire_failure_leaves_no_state() {
    let f = fixture();
    f.backend.update(|s| s.fail_acquire = Some("denied by policy".into()));
    let (result, ready) = run_host(
        &f,
        options(SessionLength::Indefinite),
        Arc::new(AtomicBool::new(false)),
        || None,
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), crate::ErrorKind::PermissionDenied);
    assert!(ready.is_none(), "on_ready must not run on failure");
    assert_eq!(f.store.probe().unwrap(), Probe::Inactive);
    assert!(f.store.read_last_session().unwrap().is_none());
}

#[test]
fn release_failure_is_recorded_not_hidden() {
    let f = fixture();
    f.backend.update(|s| s.fail_release = Some("PowerClearRequest failed".into()));
    let (result, _) = run_host(
        &f,
        options(SessionLength::Indefinite),
        Arc::new(AtomicBool::new(false)),
        || Some(EndReason::Stopped),
    );
    let record = result.unwrap();
    assert_eq!(record.release_error.as_deref(), Some("PowerClearRequest failed"));
}

#[test]
fn second_host_is_rejected_while_first_runs() {
    let f = fixture();
    let lock = f.store.acquire_host_lock().unwrap();
    let (result, _) = run_host(
        &f,
        options(SessionLength::Indefinite),
        Arc::new(AtomicBool::new(false)),
        || None,
    );
    assert_eq!(result.unwrap_err().kind(), crate::ErrorKind::Conflict);
    assert_eq!(f.backend.snapshot(|s| s.acquired), 0, "nothing acquired on conflict");
    drop(lock);
}

#[test]
fn stale_state_from_crashed_host_is_replaced() {
    let f = fixture();
    // A previous host crashed: a (corrupt) state file is left behind and
    // nobody holds the lock.
    std::fs::create_dir_all(f.store.state_path().parent().unwrap()).unwrap();
    std::fs::write(f.store.state_path(), b"{ corrupt").unwrap();
    let (result, ready) = run_host(
        &f,
        options(SessionLength::Indefinite),
        Arc::new(AtomicBool::new(false)),
        || Some(EndReason::Stopped),
    );
    assert!(result.is_ok());
    assert!(ready.is_some());
    assert_eq!(f.store.probe().unwrap(), Probe::Inactive);
}

#[test]
fn battery_policies() {
    let f = fixture();
    f.backend.update(|s| {
        s.power = Some(PowerSource {
            kind: PowerSourceKind::Battery,
            battery_present: true,
            battery_percent: Some(15),
            charging: Some(false),
        })
    });
    let mut opts = options(SessionLength::Indefinite);
    opts.battery = BatteryWatch {
        on_battery: BatteryAction::Continue,
        threshold: 20,
    };
    let (result, _) = run_host(&f, opts, Arc::new(AtomicBool::new(false)), || None);
    assert_eq!(
        result.unwrap().end,
        EndReason::BatteryThreshold {
            percent: 15,
            threshold: 20
        }
    );

    let mut opts = options(SessionLength::Indefinite);
    opts.battery = BatteryWatch {
        on_battery: BatteryAction::Stop,
        threshold: 0,
    };
    let (result, _) = run_host(&f, opts, Arc::new(AtomicBool::new(false)), || None);
    assert_eq!(result.unwrap().end, EndReason::SwitchedToBattery);
}

#[test]
fn empty_request_is_rejected() {
    let f = fixture();
    let mut opts = options(SessionLength::Indefinite);
    opts.request.idle_sleep = false;
    let (result, _) = run_host(&f, opts, Arc::new(AtomicBool::new(false)), || None);
    assert_eq!(result.unwrap_err().kind(), crate::ErrorKind::Usage);
}
