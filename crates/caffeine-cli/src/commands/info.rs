//! `capabilities` and `doctor`.

use std::process::ExitCode;

use serde::Serialize;

use caffeine_core::power::{CapabilityReport, CheckStatus, DiagnosticCheck, Support};
use caffeine_core::{Paths, Result};

use crate::context::Ctx;
use crate::output::{Output, Style};

fn support_style(s: Support) -> Style {
    match s {
        Support::Supported => Style::Good,
        Support::Limited | Support::RequiresPermission => Style::Warn,
        Support::Unsupported => Style::Dim,
    }
}

pub fn print_platform(out: &Output, report: &CapabilityReport) {
    let p = &report.platform;
    let mut parts = vec![
        p.os_version.clone().unwrap_or_else(|| p.os.clone()),
        p.arch.clone(),
    ];
    if let Some(init) = &p.init_system {
        parts.push(format!("init {init}"));
    }
    if let Some(de) = &p.desktop {
        parts.push(format!("desktop {de}"));
    }
    out.line(format!(
        "{} {}",
        out.paint(Style::Bold, "Platform:"),
        parts.join(" · ")
    ));
    out.line(format!(
        "{} {}",
        out.paint(Style::Bold, "Backend: "),
        p.backend
    ));
    for n in &p.notes {
        out.line(format!("          {n}"));
    }
}

pub fn capabilities(ctx: &Ctx) -> Result<ExitCode> {
    let report = ctx.backend()?.capabilities();
    ctx.out.json(&report);
    let out = &ctx.out;
    print_platform(out, &report);
    out.line("");
    for c in &report.capabilities {
        let support = format!("{:<19}", c.support.as_str());
        out.line(format!(
            "  {:<22} {} {}",
            c.capability.as_str(),
            out.paint(support_style(c.support), support),
            c.mechanism.as_deref().unwrap_or("-")
        ));
        out.line(format!("  {:<22} {}", "", out.paint(Style::Dim, &c.detail)));
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    checks: Vec<DiagnosticCheck>,
}

fn check_dir_writable(id: &str, label: &str, dir: &std::path::Path) -> DiagnosticCheck {
    let probe = dir.join(format!(".caffeine-doctor-{}", std::process::id()));
    let result = Paths::ensure_dir(dir).and_then(|_| {
        std::fs::write(&probe, b"ok")
            .map_err(|e| caffeine_core::Error::io(format!("writing {}", probe.display()), e))
    });
    let _ = std::fs::remove_file(&probe);
    match result {
        Ok(()) => DiagnosticCheck::new(
            id,
            CheckStatus::Ok,
            format!("{label} is writable: {}", dir.display()),
        ),
        Err(e) => DiagnosticCheck::new(id, CheckStatus::Fail, e.to_string())
            .with_hint("set CAFFEINE_HOME to a writable directory, or fix the permissions"),
    }
}

/// Core checks that do not depend on the platform backend.
pub fn core_checks(ctx: &Ctx) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();
    match &ctx.config_error {
        Some(e) => checks.push(
            DiagnosticCheck::new("config.load", CheckStatus::Fail, e.to_string()).with_hint(
                e.hint()
                    .unwrap_or("run `caffeine config validate`")
                    .to_string(),
            ),
        ),
        None if ctx.config.exists_on_disk() => checks.push(DiagnosticCheck::new(
            "config.load",
            CheckStatus::Ok,
            format!("configuration loaded from {}", ctx.config.path().display()),
        )),
        None => checks.push(DiagnosticCheck::new(
            "config.load",
            CheckStatus::Info,
            format!(
                "no configuration file at {}; using defaults",
                ctx.config.path().display()
            ),
        )),
    }
    for note in ctx.config.notes() {
        checks.push(DiagnosticCheck::new(
            "config.notes",
            CheckStatus::Warn,
            note.clone(),
        ));
    }
    if let Some(reason) = ctx.config.read_only_reason()
        && ctx.config_error.is_none()
    {
        checks.push(DiagnosticCheck::new(
            "config.version",
            CheckStatus::Warn,
            reason.to_string(),
        ));
    }
    checks.push(check_dir_writable(
        "paths.runtime",
        "runtime directory",
        &ctx.paths.runtime_dir,
    ));
    checks.push(check_dir_writable(
        "paths.data",
        "data directory",
        &ctx.paths.data_dir,
    ));
    match crate::commands::session::probe_summary(&ctx.store) {
        Ok((status, summary, hint)) => {
            let mut c = DiagnosticCheck::new("session.state", status, summary);
            if let Some(h) = hint {
                c = c.with_hint(h);
            }
            checks.push(c);
        }
        Err(e) => checks.push(DiagnosticCheck::new(
            "session.state",
            CheckStatus::Fail,
            e.to_string(),
        )),
    }
    checks
}

pub fn doctor(ctx: &Ctx, extra: Vec<DiagnosticCheck>) -> Result<ExitCode> {
    let verbose = ctx.config.config().advanced.verbose_diagnostics || ctx.global.verbose > 0;
    let mut checks = core_checks(ctx);
    match ctx.backend() {
        Ok(backend) => {
            if verbose {
                let report = backend.capabilities();
                checks.push(DiagnosticCheck::new(
                    "platform.info",
                    CheckStatus::Info,
                    format!(
                        "{} {} · backend {}",
                        report
                            .platform
                            .os_version
                            .clone()
                            .unwrap_or(report.platform.os.clone()),
                        report.platform.arch,
                        report.platform.backend
                    ),
                ));
                for c in report.capabilities {
                    checks.push(DiagnosticCheck::new(
                        format!("capability.{}", c.capability),
                        CheckStatus::Info,
                        format!("{}: {}", c.support.as_str(), c.detail),
                    ));
                }
            }
            checks.extend(backend.diagnose());
            match backend.pending_recovery() {
                Ok(items) => {
                    for item in items {
                        checks.push(if item.left_by_crash {
                            DiagnosticCheck::new("recovery.pending", CheckStatus::Warn, item.description)
                                .with_hint("an earlier session ended without restoring it; run `caffeine lid restore`")
                        } else {
                            DiagnosticCheck::new("recovery.persistent", CheckStatus::Info, item.description)
                                .with_hint("deliberate change made with `caffeine lid set`; `caffeine lid restore` reverts it")
                        });
                    }
                }
                Err(e) => checks.push(DiagnosticCheck::new(
                    "recovery.journal",
                    CheckStatus::Fail,
                    e.to_string(),
                )),
            }
        }
        Err(e) => checks.push(DiagnosticCheck::new(
            "platform.backend",
            CheckStatus::Fail,
            e.to_string(),
        )),
    }
    checks.extend(extra);

    let ok = !checks.iter().any(|c| c.status == CheckStatus::Fail);
    let report = DoctorReport { ok, checks };
    ctx.out.json(&report);
    let out = &ctx.out;
    for c in &report.checks {
        if c.status == CheckStatus::Info && !verbose && c.hint.is_none() {
            continue;
        }
        let tag = match c.status {
            CheckStatus::Ok => out.paint(Style::Good, " ok "),
            CheckStatus::Info => out.paint(Style::Accent, "info"),
            CheckStatus::Warn => out.paint(Style::Warn, "warn"),
            CheckStatus::Fail => out.paint(Style::Bad, "FAIL"),
        };
        out.line(format!("[{tag}] {}", c.summary));
        if let Some(h) = &c.hint {
            out.line(format!(
                "       {}",
                out.paint(Style::Dim, format!("hint: {h}"))
            ));
        }
    }
    let (fails, warns) = report
        .checks
        .iter()
        .fold((0, 0), |(f, w), c| match c.status {
            CheckStatus::Fail => (f + 1, w),
            CheckStatus::Warn => (f, w + 1),
            _ => (f, w),
        });
    out.line(format!(
        "\n{} {fails} failure(s), {warns} warning(s)",
        if ok {
            out.paint(Style::Good, "Summary:")
        } else {
            out.paint(Style::Bad, "Summary:")
        }
    ));
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
