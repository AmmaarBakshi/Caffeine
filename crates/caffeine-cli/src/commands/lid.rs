//! `caffeine lid [status|set|restore]`.

use std::process::ExitCode;

use serde::Serialize;

use caffeine_core::power::{Capability, LidAction, LidPolicy, LidPolicyChange, LidStatus};
use caffeine_core::{Error, Result};

use crate::cli::LidAction as Action;
use crate::context::Ctx;
use crate::output::{Style, confirm};

#[derive(Debug, Serialize)]
struct LidReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    lid: Option<LidStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lid_error: Option<caffeine_core::error::ErrorReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<LidPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_error: Option<caffeine_core::error::ErrorReport>,
}

pub fn run(ctx: &Ctx, action: Option<&Action>) -> Result<ExitCode> {
    match action {
        None | Some(Action::Status) => status(ctx),
        Some(Action::Set { ac, battery, yes }) => set(ctx, ac.as_deref(), battery.as_deref(), *yes),
        Some(Action::Restore { yes }) => restore(ctx, *yes),
    }
}

fn show_action(a: &Option<LidAction>) -> String {
    a.as_ref().map_or_else(|| "unknown".to_string(), |a| a.to_string())
}

fn status(ctx: &Ctx) -> Result<ExitCode> {
    let backend = ctx.backend()?;
    let lid = backend.lid_status();
    let policy = backend.lid_policy();
    if let (Err(a), Err(b)) = (&lid, &policy)
        && a.kind() == caffeine_core::ErrorKind::Unsupported
        && b.kind() == caffeine_core::ErrorKind::Unsupported
    {
        return Err(Error::unsupported(format!("lid information is not available: {b}")));
    }
    let report = LidReport {
        lid_error: lid.as_ref().err().map(|e| e.to_report()),
        policy_error: policy.as_ref().err().map(|e| e.to_report()),
        lid: lid.ok(),
        policy: policy.ok(),
    };
    ctx.out.json(&report);
    let out = &ctx.out;
    match (&report.lid, &report.lid_error) {
        (Some(l), _) => out.field("Lid", format!("{} ({})", out.paint(Style::Bold, l.state.as_str()), l.source)),
        (None, Some(e)) => out.field("Lid", format!("unavailable: {}", e.message)),
        _ => {}
    }
    if let Some(l) = &report.lid {
        for n in &l.notes {
            out.field("", n);
        }
    }
    match (&report.policy, &report.policy_error) {
        (Some(p), _) => {
            out.field("Plugged in", show_action(&p.on_ac));
            out.field("On battery", show_action(&p.on_battery));
            if p.docked.is_some() {
                out.field("Docked", show_action(&p.docked));
            }
            out.field("Source", &p.source);
            out.field(
                "Changeable",
                if p.writable { "yes, with `caffeine lid set` (reversible)" } else { "no (see notes)" },
            );
            for o in &p.overridden_by {
                out.field("Inhibited by", o);
            }
            for n in &p.notes {
                out.field("Note", n);
            }
        }
        (None, Some(e)) => out.field("Policy", format!("unavailable: {}", e.message)),
        _ => {}
    }
    Ok(ExitCode::SUCCESS)
}

fn set(ctx: &Ctx, ac: Option<&str>, battery: Option<&str>, yes: bool) -> Result<ExitCode> {
    let change = LidPolicyChange {
        on_ac: ac.map(str::parse).transpose()?,
        on_battery: battery.map(str::parse).transpose()?,
    };
    if change.is_empty() {
        return Err(Error::usage("nothing to change: pass --ac and/or --battery"));
    }
    let backend = ctx.backend()?;
    let report = backend.capabilities();
    if !report.is_usable(Capability::LidPolicyWrite) {
        let detail = report.get(Capability::LidPolicyWrite).map(|c| c.detail.clone()).unwrap_or_default();
        return Err(Error::unsupported(format!("changing the lid-close action is not supported here: {detail}")));
    }
    let keeps_awake = change.on_ac == Some(LidAction::DoNothing) || change.on_battery == Some(LidAction::DoNothing);
    if keeps_awake && ctx.config.config().general.safety_warnings {
        ctx.out.warn(
            "with \"do-nothing\" the laptop keeps running when closed: it can overheat in a bag or on soft \
             surfaces, and drains the battery",
        );
    }
    if !yes {
        let summary = format!(
            "Change the lid-close action ({}{}{})?",
            change.on_ac.as_ref().map(|a| format!("plugged in: {a}")).unwrap_or_default(),
            if change.on_ac.is_some() && change.on_battery.is_some() { ", " } else { "" },
            change.on_battery.as_ref().map(|a| format!("on battery: {a}")).unwrap_or_default(),
        );
        match confirm(&summary) {
            Some(true) => {}
            Some(false) => return Err(Error::failure("cancelled; nothing was changed")),
            None => {
                return Err(Error::usage("refusing to change a persistent power setting without confirmation")
                    .with_hint("re-run with --yes"));
            }
        }
    }
    let policy = backend.set_lid_policy(&change)?;
    ctx.out.json(&serde_json::json!({ "changed": true, "policy": policy }));
    ctx.out.line(format!(
        "{} lid close: plugged in = {}, on battery = {}",
        ctx.out.paint(Style::Good, "Updated"),
        show_action(&policy.on_ac),
        show_action(&policy.on_battery)
    ));
    ctx.out.line("  The previous values were saved; `caffeine lid restore` reverts the change.");
    Ok(ExitCode::SUCCESS)
}

fn restore(ctx: &Ctx, yes: bool) -> Result<ExitCode> {
    let backend = ctx.backend()?;
    let pending = backend.pending_recovery()?;
    if pending.is_empty() {
        ctx.out.json(&serde_json::json!({ "restored": [] }));
        ctx.out.line("Nothing to restore: Caffeine has not changed any lid-close setting.");
        return Ok(ExitCode::SUCCESS);
    }
    if !yes {
        for p in &pending {
            ctx.out.line(format!("  - {}", p.description));
        }
        match confirm("Restore these settings?") {
            Some(true) => {}
            Some(false) => return Err(Error::failure("cancelled; nothing was changed")),
            None => {
                return Err(Error::usage("refusing to change power settings without confirmation")
                    .with_hint("re-run with --yes"));
            }
        }
    }
    let restored = backend.recover()?;
    ctx.out.json(&serde_json::json!({ "restored": restored }));
    for r in &restored {
        ctx.out.line(format!("{} {r}", ctx.out.paint(Style::Good, "Restored")));
    }
    Ok(ExitCode::SUCCESS)
}
