use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fmt::Write as FmtWrite,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

use crate::{
    azure::Api,
    display::{WatchDisplay, pipeline_info, timer_line},
    model::{Build, approval_stages},
    policy::Policy,
};

pub(crate) fn cycle(
    api: &impl Api,
    policy: &Policy,
    dry_run: bool,
    refresh_screen: bool,
    refresh_interval: Option<u64>,
    show_controls: bool,
    watcher_started: Instant,
    submitted: &mut HashSet<String>,
    session: &str,
    stopped: &AtomicBool,
) -> Result<(Build, WatchDisplay)> {
    let build = api.build()?;
    let records = api.timeline()?;
    let mut output = String::new();
    writeln!(output, "Approval policy: {policy:?}; dry-run: {dry_run}")
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Build {}: {} / {}",
        build.id,
        build.status,
        build.result.as_deref().unwrap_or("-")
    )
    .expect("writing to a String cannot fail");
    output.push_str(&pipeline_info(&build));
    let mut display = WatchDisplay {
        info: output,
        pipeline: String::new(),
        timers: timer_line(
            &build,
            watcher_started.elapsed(),
            refresh_interval.map(Duration::from_secs),
            SystemTime::now(),
        )?,
        controls_interval: if show_controls {
            Some(refresh_interval.context("Missing interactive refresh interval")?)
        } else {
            None
        },
        escape_pending: false,
    };
    let output = &mut display.pipeline;
    let mut stages: Vec<_> = records.iter().filter(|r| r.kind == "Stage").collect();
    stages.sort_by_key(|r| r.order);
    for stage in stages {
        writeln!(
            output,
            "  {:<24} {:<12} {:<20} {}",
            stage.stage_id()?,
            stage.state,
            stage.result.as_deref().unwrap_or("-"),
            stage.name
        )
        .expect("writing to a String cannot fail");
    }
    if records.iter().any(|r| {
        r.state != "completed"
            && r.task
                .as_ref()
                .is_some_and(|t| t.name == "ManualValidation")
    }) {
        writeln!(
            output,
            "WARNING: ManualValidation task detected; it must be handled manually."
        )
        .expect("writing to a String cannot fail");
    }
    let selected_stages = policy.selected_stages(&records)?;
    if build.status == "completed" || build.status == "cancelling" {
        display.render(refresh_screen)?;
        return Ok((build, display));
    }
    for (id, stage) in approval_stages(&records, false)? {
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        let approval = api.approval(&id)?;
        writeln!(output, "  Approval {id} [{stage}]: {}", approval.status)
            .expect("writing to a String cannot fail");
        if !selected_stages.contains(&stage)
            || submitted.contains(&id)
            || approval.status != "pending"
        {
            continue;
        }
        if !approval.actionable() {
            writeln!(
                output,
                "  No actionable update permission for {id}; leaving it untouched."
            )
            .expect("writing to a String cannot fail");
            continue;
        }
        if dry_run {
            writeln!(output, "  DRY RUN: would approve {id} for {stage}")
                .expect("writing to a String cannot fail");
            continue;
        }
        // Recheck immediately before writing; errors or uncertainty stop the watcher.
        if api.build()?.status != "inProgress" || !api.approval(&id)?.actionable() {
            writeln!(output, "  Approval/build changed; leaving {id} untouched.")
                .expect("writing to a String cannot fail");
            continue;
        }
        let comment = format!(
            "az-pipeline-watch build={} stage={stage} session={session}",
            build.id
        );
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        api.approve(&id, &comment).with_context(|| {
            format!("Approval {id} write failed; inspect Azure DevOps before restarting (no automatic retry)")
        })?;
        let verified = api.approval(&id).with_context(|| {
            format!(
                "Cannot verify approval {id} after PATCH; inspect Azure DevOps before restarting"
            )
        })?;
        ensure!(
            verified.status == "approved"
                || verified.steps.iter().any(|step| {
                    step.status == "approved" && step.comment.as_deref() == Some(&comment)
                }),
            "Approval {id} was not confirmed after PATCH; stopping without retry"
        );
        submitted.insert(id.clone());
        writeln!(
            output,
            "  Confirmed approval vote for {stage} ({id}); overall: {}",
            verified.status
        )
        .expect("writing to a String cannot fail");
    }
    display.render(refresh_screen)?;
    Ok((build, display))
}
