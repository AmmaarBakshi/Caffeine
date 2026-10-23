//! `start`, `stop`, `status`, `run` and the hidden `__host` command.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use caffeine_core::config::{
    BatteryAction, ConflictPolicy, DisplayPolicy, ExpireAction, LogLevel, Notifications,
};
use caffeine_core::duration::{SessionLength, format_duration};
use caffeine_core::error::ErrorReport;
use caffeine_core::power::{
    AwakeRequest, Capability, LidSessionMode, PowerSourceKind, RecoveryItem,
};
use caffeine_core::session::{
    BatteryWatch, EndReason, HostOptions, Probe, SessionHost, SessionMode, SessionRecord,
    SessionState, SessionStore,
};
use caffeine_core::time::{format_relative, format_rfc3339, unix_now};
use caffeine_core::{Error, Result};
use caffeine_platform::{BackendOptions, process};

use crate::cli::{RunArgs, SessionArgs, StartArgs, StopArgs};
use crate::context::Ctx;
use crate::output::{Output, Style};

/// Exit code of `status` when no session is active.
pub const EXIT_INACTIVE: u8 = 3;

/// What the user asked for, after merging flags with configuration.
#[derive(Debug, Clone)]
struct Plan {
    request: AwakeRequest,
    length: SessionLength,
    explicit_length: bool,
    clamped: bool,
    battery: BatteryWatch,
}

fn plan(ctx: &Ctx, args: &SessionArgs) -> Result<Plan> {
    let cfg = ctx.config.config();
    let (requested, explicit_length) = match &args.duration {
        Some(d) => (d.parse::<SessionLength>()?, true),
        None => (cfg.session.default_duration, false),
    };
    let (length, clamped) = cfg.clamp_length(requested);
    let lid = match &args.lid {
        Some(m) => m.parse::<LidSessionMode>()?,
        None => cfg.power.lid_session,
    };
    let display = if args.no_display {
        false
    } else {
        args.display || cfg.power.display == DisplayPolicy::KeepOn
    };
    let request = AwakeRequest {
        idle_sleep: !args.no_idle && cfg.power.prevent_idle_sleep,
        display,
        lid,
        reason: args
            .reason
            .clone()
            .unwrap_or_else(|| AwakeRequest::default().reason),
    };
    if request.is_empty() {
        return Err(Error::usage(
            "nothing to keep awake: idle-sleep prevention is disabled and no display or lid override was requested",
        )
        .with_hint("pass --display, or enable power.prevent_idle_sleep"));
    }
    Ok(Plan {
        request,
        length,
        explicit_length,
        clamped,
        battery: BatteryWatch {
            on_battery: cfg.power.on_battery,
            threshold: cfg.power.battery_threshold,
        },
    })
}

/// Fail early, with the platform's explanation, if a requested capability
/// is unavailable. Never start a session that cannot do what was asked.
/// Returns caveats for capabilities that are only partially supported.
fn preflight(ctx: &Ctx, plan: &Plan) -> Result<Vec<String>> {
    let mut caveats = Vec::new();
    let backend = ctx.backend()?;
    let report = backend.capabilities();
    for cap in plan.request.required_capabilities() {
        if !report.is_usable(cap) {
            let detail = report
                .get(cap)
                .map(|c| c.detail.clone())
                .unwrap_or_default();
            let mut err = Error::unsupported(format!(
                "{} is not available here: {detail}",
                cap.description()
            ));
            if report.support(cap) == caffeine_core::power::Support::RequiresPermission {
                err = Error::permission(err.message().to_string());
            }
            return Err(err.with_hint("run `caffeine capabilities` for details"));
        }
        // Caveats about idle sleep are moot when the display is kept on too.
        let moot = cap == Capability::IdleSleep && plan.request.display;
        if let Some(status) = report.get(cap)
            && status.support == caffeine_core::power::Support::Limited
            && !moot
        {
            caveats.push(format!("{}: {}", cap.as_str(), status.detail));
        }
    }
    if plan.battery.is_active() && !report.is_usable(Capability::PowerSource) {
        caveats.push(
            "battery policy is configured but the power source cannot be read; it will be ignored"
                .into(),
        );
    }
    Ok(caveats)
}

fn print_caveats(ctx: &Ctx, caveats: Vec<String>) {
    for c in caveats {
        ctx.out.warn(c);
    }
}

fn safety_warnings(ctx: &Ctx, plan: &Plan) {
    if !ctx.config.config().general.safety_warnings {
        return;
    }
    if let Ok(backend) = ctx.backend()
        && let Ok(src) = backend.power_source()
        && src.kind == PowerSourceKind::Battery
    {
        let pct = src
            .battery_percent
            .map(|p| format!(" ({p}%)"))
            .unwrap_or_default();
        ctx.out.warn(format!(
            "running on battery{pct}: keeping the system awake drains it faster"
        ));
    }
    if plan.request.lid != LidSessionMode::System {
        ctx.out.warn(
            "closed-lid operation: keep vents unobstructed and never put a running laptop in a bag or sleeve",
        );
    }
}

fn recovery_warnings(ctx: &Ctx) {
    let Ok(backend) = ctx.backend() else { return };
    if let Ok(items) = backend.pending_recovery() {
        for item in items.iter().filter(|i| i.left_by_crash) {
            ctx.out.warn(format!(
                "an earlier session did not clean up: {}; run `caffeine lid restore`",
                item.description
            ));
        }
    }
}

// ---------------------------------------------------------------- start --

#[derive(Debug, Serialize)]
struct StartReport<'a> {
    result: &'a str,
    session: &'a SessionState,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    clamped_to_max_duration: bool,
}

pub fn start(ctx: &Ctx, args: &StartArgs) -> Result<ExitCode> {
    ctx.warn_config_problems();
    let plan = plan(ctx, &args.session)?;
    let caveats = preflight(ctx, &plan)?;
    recovery_warnings(ctx);
    if plan.clamped {
        ctx.out.warn(format!(
            "session limited to session.max_duration ({})",
            ctx.config.config().session.max_duration
        ));
    }
    if args.foreground {
        print_caveats(ctx, caveats);
        safety_warnings(ctx, &plan);
        return run_foreground(ctx, plan, SessionMode::Foreground, None);
    }

    let _start_lock = ctx.store.acquire_start_lock(Duration::from_secs(15))?;
    let mut replaced = false;
    if let Some(active) = current_session(&ctx.store)? {
        let same = active.request == plan.request
            && (!plan.explicit_length || same_length(&active, plan.length));
        let policy = if args.replace {
            ConflictPolicy::Replace
        } else {
            ctx.config.config().session.on_conflict
        };
        if same {
            report_start(ctx, "already-active", &active, false);
            return Ok(ExitCode::SUCCESS);
        }
        match policy {
            ConflictPolicy::Keep => {
                ctx.out.note("a session with different settings is running; keeping it (session.on_conflict = keep)");
                report_start(ctx, "kept", &active, false);
                return Ok(ExitCode::SUCCESS);
            }
            ConflictPolicy::Error => {
                return Err(Error::conflict(format!(
                    "a session with different settings is already active (started {}, pid {})",
                    format_relative(active.started_at, unix_now()),
                    active.pid
                ))
                .with_hint("use `caffeine start --replace`, or `caffeine stop` first"));
            }
            ConflictPolicy::Replace => {
                stop_session(ctx, &active, false, stop_timeout(ctx, None))?;
                replaced = true;
            }
        }
    }

    print_caveats(ctx, caveats);
    safety_warnings(ctx, &plan);
    let state = spawn_host(ctx, &plan)?;
    report_start(
        ctx,
        if replaced { "replaced" } else { "started" },
        &state,
        plan.clamped,
    );
    Ok(ExitCode::SUCCESS)
}

/// Whether a running session was started with `length`, so that repeating
/// the same `caffeine start --duration` is idempotent.
fn same_length(active: &SessionState, length: SessionLength) -> bool {
    let original = active
        .expires_at
        .map(|e| e.saturating_sub(active.started_at));
    original == length.as_duration().map(|d| d.as_secs())
}

/// Return the live session, cleaning up stale state left by a crashed host.
fn current_session(store: &SessionStore) -> Result<Option<SessionState>> {
    for _ in 0..100 {
        match store.probe()? {
            Probe::Active(s) => return Ok(Some(s)),
            Probe::Inactive => return Ok(None),
            Probe::Stale(s) => {
                log::info!(
                    "removing stale state of session {} (pid {} is gone)",
                    s.id,
                    s.pid
                );
                store.clear_stale()?;
                return Ok(None);
            }
            Probe::Starting => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Err(Error::conflict(
        "a session host is starting but has not published its state",
    ))
}

fn report_start(ctx: &Ctx, result: &str, state: &SessionState, clamped: bool) {
    ctx.out.json(&StartReport {
        result,
        session: state,
        clamped_to_max_duration: clamped,
    });
    if ctx.out.is_json() {
        return;
    }
    let headline = match result {
        "already-active" => ctx.out.paint(Style::Good, "Already awake"),
        "kept" => ctx
            .out
            .paint(Style::Good, "Already awake (kept running session)"),
        "replaced" => ctx
            .out
            .paint(Style::Good, "Awake (replaced previous session)"),
        _ => ctx.out.paint(Style::Good, "Awake"),
    };
    let until = match state.expires_at {
        Some(t) => format!(
            "for {} (until {})",
            format_duration(Duration::from_secs(t.saturating_sub(unix_now()))),
            format_rfc3339(t)
        ),
        None => "until `caffeine stop`".into(),
    };
    ctx.out.line(format!("{headline} {until}"));
    ctx.out.line(format!("  {}", describe_assertions(state)));
}

fn describe_assertions(state: &SessionState) -> String {
    state
        .assertions
        .iter()
        .map(|a| format!("{} via {}", a.capability, a.mechanism))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Everything the detached host needs, sent on its stdin as JSON.
#[derive(Debug, Serialize, Deserialize)]
struct HostSpec {
    request: AwakeRequest,
    length: SessionLength,
    battery: BatteryWatch,
    record_last_session: bool,
    log_file: PathBuf,
    log_level: LogLevel,
    backend: String,
    experimental: Vec<String>,
    runtime_dir: PathBuf,
    data_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct HostReady {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorReport>,
}

fn log_file(ctx: &Ctx) -> PathBuf {
    ctx.config
        .config()
        .advanced
        .log_file
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| ctx.paths.log_file())
}

fn spawn_host(ctx: &Ctx, plan: &Plan) -> Result<SessionState> {
    let cfg = ctx.config.config();
    let spec = HostSpec {
        request: plan.request.clone(),
        length: plan.length,
        battery: plan.battery,
        record_last_session: cfg.session.record_last_session,
        log_file: log_file(ctx),
        log_level: cfg.general.log_level,
        backend: cfg.advanced.backend.clone(),
        experimental: cfg.advanced.experimental.clone(),
        runtime_dir: ctx.paths.runtime_dir.clone(),
        data_dir: ctx.paths.data_dir.clone(),
    };
    let exe =
        std::env::current_exe().map_err(|e| Error::io("locating the caffeine executable", e))?;
    let build = |breakaway: bool| {
        let mut cmd = ProcessCommand::new(&exe);
        cmd.arg("__host")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        if breakaway {
            process::configure_detached_breakaway(&mut cmd);
        } else {
            process::configure_detached(&mut cmd);
        }
        #[cfg(not(windows))]
        {
            let _ = breakaway;
            process::configure_detached(&mut cmd);
        }
        cmd
    };
    let mut child = match build(true).spawn() {
        Ok(c) => c,
        // Not every job object permits breakaway; retry without it.
        Err(_) if cfg!(windows) => build(false)
            .spawn()
            .map_err(|e| Error::io("starting the session host", e))?,
        Err(e) => return Err(Error::io("starting the session host", e)),
    };
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let payload = serde_json::to_vec(&spec).expect("host spec serializes");
        stdin
            .write_all(&payload)
            .map_err(|e| Error::io("sending the session request to the host", e))?;
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(line) => line,
        Err(_) => {
            let _ = child.kill();
            return Err(Error::failure(
                "the session host did not report readiness within 30 seconds",
            )
            .with_hint(format!("see the log at {}", spec.log_file.display())));
        }
    };
    if line.trim().is_empty() {
        let status = child.wait().ok();
        return Err(Error::failure(format!(
            "the session host exited before acquiring any assertion ({})",
            status.map_or("unknown status".into(), |s| s.to_string())
        ))
        .with_hint(format!("see the log at {}", spec.log_file.display())));
    }
    let ready: HostReady = serde_json::from_str(line.trim())
        .map_err(|e| Error::failure(format!("unexpected reply from the session host: {e}")))?;
    match (ready.ok, ready.session, ready.error) {
        (true, Some(state), _) => Ok(state),
        (_, _, Some(err)) => {
            let _ = child.wait();
            Err(Error::from(err))
        }
        _ => Err(Error::failure("malformed reply from the session host")),
    }
}

/// Entry point of the detached session host (`caffeine __host`).
pub fn host_main() -> ExitCode {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return ExitCode::from(2);
    }
    let spec: HostSpec = match serde_json::from_str(&input) {
        Ok(s) => s,
        Err(e) => {
            reply(&HostReady {
                ok: false,
                session: None,
                error: Some(Error::usage(format!("invalid host request: {e}")).to_report()),
            });
            return ExitCode::from(2);
        }
    };
    crate::logging::init_file(spec.log_level.to_filter(), &spec.log_file);

    let terminate = Arc::new(AtomicBool::new(false));
    install_termination_handler(&terminate);

    let options = BackendOptions {
        backend: spec.backend.clone(),
        experimental: spec.experimental.clone(),
        data_dir: spec.data_dir.clone(),
    };
    let backend = match caffeine_platform::native(&options) {
        Ok(b) => b,
        Err(e) => {
            log::error!("creating backend: {e}");
            reply(&HostReady {
                ok: false,
                session: None,
                error: Some(e.to_report()),
            });
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    let store = SessionStore::with_dirs(&spec.runtime_dir, &spec.data_dir);
    let mut opts = HostOptions::new(spec.request, spec.length, SessionMode::Background);
    opts.battery = spec.battery;
    opts.record_last_session = spec.record_last_session;
    opts.log_file = Some(spec.log_file);
    let host = SessionHost::new(backend.as_ref(), &store, opts, terminate);
    match host.run(
        &mut |state| {
            reply(&HostReady {
                ok: true,
                session: Some(state.clone()),
                error: None,
            })
        },
        &mut || None,
    ) {
        Ok(record) => {
            log::info!("session {} ended: {}", record.id, record.end.describe());
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("session host failed: {e}");
            reply(&HostReady {
                ok: false,
                session: None,
                error: Some(e.to_report()),
            });
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn reply(ready: &HostReady) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, ready);
    let _ = writeln!(out);
    let _ = out.flush();
}

fn install_termination_handler(flag: &Arc<AtomicBool>) {
    let flag = Arc::clone(flag);
    if let Err(e) = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst)) {
        log::debug!("no termination handler installed: {e}");
    }
}

// ----------------------------------------------------------- foreground --

fn run_foreground(
    ctx: &Ctx,
    plan: Plan,
    mode: SessionMode,
    command: Option<&[String]>,
) -> Result<ExitCode> {
    if let Some(active) = current_session(&ctx.store)? {
        if let Some(argv) = command
            && covers(&active.request, &plan.request)
        {
            ctx.out.note(format!(
                "session {} (pid {}) already keeps the system awake; running the command directly",
                active.id, active.pid
            ));
            return run_plain(argv);
        }
        return Err(Error::conflict(format!(
            "a session is already active (pid {}, started {})",
            active.pid,
            format_relative(active.started_at, unix_now())
        ))
        .with_hint("stop it with `caffeine stop`, or use `caffeine start --replace`"));
    }

    let terminate = Arc::new(AtomicBool::new(false));
    install_termination_handler(&terminate);
    let backend = ctx.backend()?;
    let cfg = ctx.config.config();
    let mut opts = HostOptions::new(plan.request.clone(), plan.length, mode);
    opts.battery = plan.battery;
    opts.record_last_session = cfg.session.record_last_session;
    let host = SessionHost::new(backend, &ctx.store, opts, Arc::clone(&terminate));

    let child: std::cell::RefCell<Option<std::io::Result<std::process::Child>>> =
        Default::default();
    let out = &ctx.out;
    let record = host.run(
        &mut |state| {
            out.json_event(&serde_json::json!({"event": "started", "session": state}));
            if !out.is_json() {
                let until = match state.expires_at {
                    Some(t) => format!(
                        " for {}",
                        format_duration(Duration::from_secs(t.saturating_sub(unix_now())))
                    ),
                    None => String::new(),
                };
                match command {
                    Some(argv) => out.line(format!(
                        "{} while `{}` runs{until}",
                        out.paint(Style::Good, "Awake"),
                        argv.join(" ")
                    )),
                    None => out.line(format!(
                        "{}{until}. Press Ctrl+C to stop.",
                        out.paint(Style::Good, "Awake")
                    )),
                }
                out.line(format!("  {}", describe_assertions(state)));
            }
            if let Some(argv) = command {
                *child.borrow_mut() = Some(ProcessCommand::new(&argv[0]).args(&argv[1..]).spawn());
            }
        },
        &mut || {
            let mut slot = child.borrow_mut();
            match slot.as_mut() {
                Some(Ok(c)) => match c.try_wait() {
                    Ok(Some(status)) => Some(EndReason::CommandExited {
                        code: status.code(),
                    }),
                    Ok(None) => None,
                    Err(_) => Some(EndReason::CommandExited { code: None }),
                },
                Some(Err(_)) => Some(EndReason::CommandExited { code: None }),
                None => None,
            }
        },
    )?;

    // A wrapped command keeps running even if the session ended first
    // (e.g. `caffeine stop` from another terminal); wait for it.
    let mut exit = ExitCode::SUCCESS;
    if let Some(spawned) = child.into_inner() {
        let mut c = spawned.map_err(|e| {
            Error::io(
                format!("running `{}`", command.map(|a| a[0].as_str()).unwrap_or("")),
                e,
            )
        })?;
        if !matches!(record.end, EndReason::CommandExited { .. }) {
            out.note(format!(
                "session ended ({}); waiting for the command to finish",
                record.end.describe()
            ));
        }
        let status = c
            .wait()
            .map_err(|e| Error::io("waiting for the command", e))?;
        exit = exit_code_of(status);
    }
    finish_foreground(ctx, &record);
    Ok(exit)
}

fn covers(active: &AwakeRequest, wanted: &AwakeRequest) -> bool {
    (active.idle_sleep || !wanted.idle_sleep)
        && (active.display || !wanted.display)
        && (active.lid == wanted.lid || wanted.lid == LidSessionMode::System)
}

fn run_plain(argv: &[String]) -> Result<ExitCode> {
    let status = ProcessCommand::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::io(format!("running `{}`", argv[0]), e))?;
    Ok(exit_code_of(status))
}

fn exit_code_of(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(c) => ExitCode::from((c & 0xff) as u8),
        None => ExitCode::from(1),
    }
}

fn finish_foreground(ctx: &Ctx, record: &SessionRecord) {
    let cfg = ctx.config.config();
    let notify = match record.end {
        EndReason::Expired => cfg.session.on_expire == ExpireAction::StopAndNotify,
        EndReason::BatteryThreshold { .. } | EndReason::SwitchedToBattery => true,
        _ => false,
    };
    if notify && !ctx.out.is_json() {
        match cfg.general.notifications {
            Notifications::None => {}
            Notifications::Bell => eprint!("\x07"),
            Notifications::Osc9 => eprint!(
                "\x1b]9;Caffeine: session ended ({})\x07",
                record.end.describe()
            ),
        }
    }
    if let Some(err) = &record.release_error {
        ctx.out
            .warn(format!("releasing assertions reported an error: {err}"));
    }
    if ctx.out.is_json() {
        ctx.out
            .json_event(&serde_json::json!({"event": "ended", "record": record}));
    } else {
        ctx.out.line(format!(
            "Session ended after {}: {}",
            format_duration(Duration::from_secs(
                record.ended_at.saturating_sub(record.started_at)
            )),
            record.end.describe()
        ));
    }
}

pub fn run(ctx: &Ctx, args: &RunArgs) -> Result<ExitCode> {
    ctx.warn_config_problems();
    let plan = plan(ctx, &args.session)?;
    let caveats = preflight(ctx, &plan)?;
    recovery_warnings(ctx);
    print_caveats(ctx, caveats);
    safety_warnings(ctx, &plan);
    let program = args.command[0].clone();
    run_foreground(
        ctx,
        plan,
        SessionMode::Command { program },
        Some(&args.command),
    )
}

/// Keep the system awake (with the configured defaults) while `work` runs
/// on another thread; used for plugin commands declared `keep_awake`.
/// Returns what `work` returned and the session record, if this call
/// started a session (an already-running one that covers the request is
/// reused, as with `caffeine run`).
pub fn keep_awake_while<T: Send>(
    ctx: &Ctx,
    label: &str,
    work: impl FnOnce() -> Result<(i32, T)> + Send,
) -> Result<((i32, T), Option<SessionRecord>)> {
    let args = SessionArgs {
        duration: None,
        display: false,
        no_display: false,
        no_idle: false,
        lid: None,
        reason: Some(format!("{label} is running")),
    };
    let plan = plan(ctx, &args)?;
    let caveats = preflight(ctx, &plan)?;
    if let Some(active) = current_session(&ctx.store)? {
        if covers(&active.request, &plan.request) {
            ctx.out.note(format!(
                "session {} (pid {}) already keeps the system awake",
                active.id, active.pid
            ));
            return Ok((work()?, None));
        }
        return Err(Error::conflict(format!(
            "a session with different settings is active (pid {}); `{label}` needs its own",
            active.pid
        ))
        .with_hint("stop it with `caffeine stop` first"));
    }
    recovery_warnings(ctx);
    print_caveats(ctx, caveats);
    safety_warnings(ctx, &plan);

    let terminate = Arc::new(AtomicBool::new(false));
    install_termination_handler(&terminate);
    let backend = ctx.backend()?;
    let mut opts = HostOptions::new(
        plan.request.clone(),
        plan.length,
        SessionMode::Command {
            program: label.to_string(),
        },
    );
    opts.battery = plan.battery;
    opts.record_last_session = ctx.config.config().session.record_last_session;
    let host = SessionHost::new(backend, &ctx.store, opts, terminate);
    let out = &ctx.out;

    std::thread::scope(|scope| {
        let work = std::cell::Cell::new(Some(work));
        let handle = std::cell::RefCell::new(None);
        let finished = std::cell::RefCell::new(None);
        let record = host.run(
            &mut |state| {
                if !out.is_json() {
                    out.note(format!(
                        "keeping the system awake while `{label}` runs ({})",
                        describe_assertions(state)
                    ));
                }
                let work = work.take().expect("on_ready runs once");
                *handle.borrow_mut() = Some(scope.spawn(work));
            },
            &mut || {
                let mut slot = handle.borrow_mut();
                if !slot.as_ref().is_some_and(|h| h.is_finished()) {
                    return None;
                }
                let result = join(slot.take().expect("checked above"));
                let code = match &result {
                    Ok((c, _)) => *c,
                    Err(e) => e.exit_code(),
                };
                let code = Some(code);
                *finished.borrow_mut() = Some(result);
                Some(EndReason::CommandExited { code })
            },
        );
        // The work is never abandoned: if the session ended first (stop,
        // battery, Ctrl+C), wait for it before returning.
        let pending = handle.into_inner();
        let record = record?;
        let result = match (finished.into_inner(), pending) {
            (Some(r), _) => r,
            (None, Some(h)) => {
                out.note(format!(
                    "session ended ({}); waiting for `{label}` to finish",
                    record.end.describe()
                ));
                join(h)
            }
            (None, None) => Err(Error::failure("the session ended before the work started")),
        };
        if let Some(err) = &record.release_error {
            out.warn(format!("releasing assertions reported an error: {err}"));
        }
        Ok((result?, Some(record)))
    })
}

fn join<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T>>) -> Result<T> {
    handle
        .join()
        .unwrap_or_else(|_| Err(Error::failure("the command panicked")))
}

// ----------------------------------------------------------------- stop --

fn stop_timeout(ctx: &Ctx, flag: Option<u32>) -> Duration {
    Duration::from_secs(u64::from(
        flag.unwrap_or(ctx.config.config().session.stop_timeout_secs),
    ))
}

/// Stop a live session; returns its end record if one was written.
fn stop_session(
    ctx: &Ctx,
    state: &SessionState,
    force: bool,
    timeout: Duration,
) -> Result<Option<SessionRecord>> {
    ctx.store.request_stop(&state.id)?;
    if ctx.store.wait_for_end(&state.id, timeout)? {
        return Ok(ctx.store.read_last_session()?.filter(|r| r.id == state.id));
    }
    if !force {
        return Err(Error::failure(format!(
            "the session host (pid {}) did not stop within {}s",
            state.pid,
            timeout.as_secs()
        ))
        .with_hint("retry with `caffeine stop --force` to terminate it"));
    }
    ctx.out.warn(format!(
        "terminating unresponsive session host (pid {})",
        state.pid
    ));
    process::kill(state.pid)?;
    // The OS releases process-bound assertions; wait for the lock to drop.
    if !ctx.store.wait_for_end(&state.id, Duration::from_secs(5))? {
        return Err(Error::failure(format!(
            "process {} is still holding the session lock",
            state.pid
        )));
    }
    ctx.store.clear_stale()?;
    // Session-scoped settings owned by the killed host are now restorable.
    if let Ok(backend) = ctx.backend() {
        let crash_items = backend.pending_recovery().unwrap_or_default();
        if crash_items.iter().any(|i| i.left_by_crash) {
            for note in backend.recover()? {
                ctx.out.note(note);
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Serialize)]
struct StopReport {
    stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<SessionRecord>,
}

pub fn stop(ctx: &Ctx, args: &StopArgs) -> Result<ExitCode> {
    let timeout = stop_timeout(ctx, args.timeout);
    let mut probe = ctx.store.probe()?;
    for _ in 0..100 {
        if probe != Probe::Starting {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        probe = ctx.store.probe()?;
    }
    let report = match probe {
        Probe::Inactive => StopReport {
            stopped: false,
            session_id: None,
            detail: "no active session".into(),
            record: None,
        },
        Probe::Starting => {
            return Err(Error::conflict(
                "a session host is still starting; try again",
            ));
        }
        Probe::Stale(s) => {
            ctx.store.clear_stale()?;
            if let Ok(backend) = ctx.backend()
                && backend
                    .pending_recovery()
                    .unwrap_or_default()
                    .iter()
                    .any(|i| i.left_by_crash)
            {
                for note in backend.recover()? {
                    ctx.out.note(note);
                }
            }
            StopReport {
                stopped: false,
                session_id: Some(s.id),
                detail: format!(
                    "removed stale state of a session whose host (pid {}) is gone",
                    s.pid
                ),
                record: None,
            }
        }
        Probe::Active(s) => {
            let record = stop_session(ctx, &s, args.force, timeout)?;
            StopReport {
                stopped: true,
                session_id: Some(s.id.clone()),
                detail: match &record {
                    Some(r) => format!(
                        "session {} after {}",
                        s.id,
                        format_duration(Duration::from_secs(
                            r.ended_at.saturating_sub(r.started_at)
                        ))
                    ),
                    None => format!("session {}", s.id),
                },
                record,
            }
        }
    };
    if let Some(err) = report.record.as_ref().and_then(|r| r.release_error.clone()) {
        ctx.out.warn(format!(
            "the session host reported an error while releasing assertions: {err}"
        ));
    }
    ctx.out.json(&report);
    if report.stopped {
        ctx.out.line(format!(
            "{} {}",
            ctx.out.paint(Style::Good, "Stopped"),
            report.detail
        ));
    } else {
        ctx.out.line(capitalize(&report.detail));
    }
    Ok(ExitCode::SUCCESS)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

// --------------------------------------------------------------- status --

#[derive(Debug, Serialize)]
struct StatusReport {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_session: Option<SessionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lid: Option<caffeine_core::power::LidStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    power: Option<caffeine_core::power::PowerSource>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    recovery: Vec<RecoveryItem>,
}

pub fn status(ctx: &Ctx) -> Result<ExitCode> {
    ctx.warn_config_problems();
    let now = unix_now();
    let probe = ctx.store.probe()?;
    let backend = ctx.backend().ok();
    let lid = if ctx.config.config().power.lid_detection {
        backend
            .filter(|b| b.capabilities().is_usable(Capability::LidState))
            .and_then(|b| b.lid_status().ok())
    } else {
        None
    };
    let power = backend.and_then(|b| b.power_source().ok());
    let recovery = backend
        .and_then(|b| b.pending_recovery().ok())
        .unwrap_or_default();
    let last = ctx.store.read_last_session().ok().flatten();

    let (state, session, host_alive) = match probe {
        Probe::Active(s) => ("active", Some(s), None),
        Probe::Starting => ("starting", None, None),
        Probe::Stale(s) => {
            let alive = process::is_alive(s.pid);
            ("stale", Some(s), Some(alive))
        }
        Probe::Inactive => ("inactive", None, None),
    };
    let report = StatusReport {
        state,
        remaining_secs: session.as_ref().and_then(|s| s.remaining(now)),
        session,
        host_alive,
        last_session: if state == "active" { None } else { last },
        lid,
        power,
        recovery,
    };
    ctx.out.json(&report);
    if !ctx.out.is_json() {
        print_status(&ctx.out, &report, now);
    }
    Ok(match state {
        "active" | "starting" => ExitCode::SUCCESS,
        _ => ExitCode::from(EXIT_INACTIVE),
    })
}

fn print_status(out: &Output, r: &StatusReport, now: u64) {
    match (r.state, &r.session) {
        ("active", Some(s)) => {
            out.line(format!(
                "{} — session {}",
                out.paint(Style::Good, "Awake"),
                s.id
            ));
            out.field("Mode", s.mode.describe());
            out.field(
                "Started",
                format!(
                    "{} ({})",
                    format_relative(s.started_at, now),
                    format_rfc3339(s.started_at)
                ),
            );
            match (s.expires_at, r.remaining_secs) {
                (Some(t), Some(rem)) => out.field(
                    "Remaining",
                    format!(
                        "{} (until {})",
                        format_duration(Duration::from_secs(rem)),
                        format_rfc3339(t)
                    ),
                ),
                _ => out.field("Remaining", "no time limit"),
            }
            for a in &s.assertions {
                out.field("Holding", format!("{} — {}", a.capability, a.mechanism));
            }
            if s.battery.is_active() {
                let mut parts = Vec::new();
                if s.battery.on_battery == BatteryAction::Stop {
                    parts.push("stop on battery".to_string());
                }
                if s.battery.threshold > 0 {
                    parts.push(format!("stop below {}%", s.battery.threshold));
                }
                out.field("Battery rule", parts.join(", "));
            }
            if s.resumes > 0 {
                out.field(
                    "Resumes",
                    format!(
                        "{} (last {})",
                        s.resumes,
                        s.last_resume_at
                            .map_or(String::new(), |t| format_relative(t, now))
                    ),
                );
            }
            out.field("Host", format!("pid {} ({})", s.pid, s.backend));
        }
        ("starting", _) => out.line("A session is starting."),
        ("stale", Some(s)) => {
            out.line(format!(
                "{} — session {} was not shut down cleanly (host pid {} is gone)",
                out.paint(Style::Warn, "Not awake"),
                s.id,
                s.pid
            ));
            out.line("  The OS released its assertions when the host exited. Run `caffeine stop` to clean up.");
        }
        _ => {
            out.line(format!(
                "{} — no active session",
                out.paint(Style::Dim, "Not awake")
            ));
            if let Some(last) = &r.last_session {
                out.field(
                    "Last session",
                    format!(
                        "ended {} after {}: {}",
                        format_relative(last.ended_at, now),
                        format_duration(Duration::from_secs(
                            last.ended_at.saturating_sub(last.started_at)
                        )),
                        last.end.describe()
                    ),
                );
            }
        }
    }
    if let Some(lid) = &r.lid {
        out.field("Lid", lid.state.as_str());
    }
    if let Some(p) = &r.power {
        let mut text = p.kind.as_str().to_string();
        if let Some(pct) = p.battery_percent {
            text.push_str(&format!(", battery {pct}%"));
        }
        if p.charging == Some(true) {
            text.push_str(" (charging)");
        }
        out.field("Power", text);
    }
    for item in &r.recovery {
        let kind = if item.left_by_crash {
            "needs restore"
        } else {
            "changed by caffeine"
        };
        out.field(
            "Lid policy",
            format!("{kind}: {} — `caffeine lid restore`", item.description),
        );
    }
}

/// Used by `doctor`.
pub fn probe_summary(
    store: &SessionStore,
) -> Result<(caffeine_core::power::CheckStatus, String, Option<String>)> {
    use caffeine_core::power::CheckStatus;
    Ok(match store.probe()? {
        Probe::Inactive => (CheckStatus::Ok, "no active session".into(), None),
        Probe::Starting => (CheckStatus::Info, "a session host is starting".into(), None),
        Probe::Active(s) => (
            CheckStatus::Ok,
            format!(
                "session {} active (pid {}, {})",
                s.id,
                s.pid,
                s.mode.describe()
            ),
            None,
        ),
        Probe::Stale(s) => (
            CheckStatus::Warn,
            format!(
                "stale state of session {} (host pid {} exited without cleanup)",
                s.id, s.pid
            ),
            Some("run `caffeine stop` to remove it".into()),
        ),
    })
}
