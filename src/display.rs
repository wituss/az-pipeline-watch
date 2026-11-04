use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    queue,
    terminal::{Clear, ClearType, is_raw_mode_enabled},
};
use std::{
    io::Write,
    time::{Duration, SystemTime},
};

use crate::{
    azure::Api,
    model::{Build, approval_stages},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) struct WatchDisplay {
    pub(crate) info: String,
    pub(crate) pipeline: String,
    pub(crate) timers: String,
    pub(crate) controls_interval: Option<u64>,
}

impl WatchDisplay {
    pub(crate) fn render(&self, refresh_screen: bool) -> Result<()> {
        let mut timers = self.timers.clone();
        if let Some(interval) = self.controls_interval {
            timers.push_str(&format!(
                "\nRefresh interval: {interval}s | r refresh, + faster, - slower, Esc twice / Ctrl-C exit\n"
            ));
        }
        let frame = format!(
            "{}{}{}",
            section("Pipeline info", &self.info),
            section("Timers", &timers),
            section("Stages & approvals", &self.pipeline),
        );
        render_frame(&mut std::io::stdout().lock(), &frame, refresh_screen)
    }
}

pub(crate) fn list_stages(api: &impl Api, output: &mut impl Write) -> Result<()> {
    let build = api.build()?;
    let records = api.timeline()?;
    let approvals = approval_stages(&records, true)?;
    let mut info = format!(
        "Build {}: {} / {} (read-only)",
        build.id,
        build.status,
        build.result.as_deref().unwrap_or("-")
    );
    info.push('\n');
    info.push_str(&pipeline_info(&build));
    write!(output, "{}", section("Pipeline info", &info))?;
    let mut pipeline = Vec::new();
    let mut stages: Vec<_> = records.iter().filter(|r| r.kind == "Stage").collect();
    stages.sort_by_key(|r| r.order);
    for stage in stages {
        let name = stage.stage_id()?;
        writeln!(
            pipeline,
            "{:<24} {:<12} {:<20} {}",
            name,
            stage.state,
            stage.result.as_deref().unwrap_or("-"),
            stage.name
        )?;
        let mut found = false;
        for (id, _) in approvals.iter().filter(|(_, owner)| owner.as_str() == name) {
            found = true;
            let approval = api.approval(id)?;
            writeln!(
                pipeline,
                "  Approval {id}: {}; can approve now: {}",
                approval.status,
                if build.status == "inProgress"
                    && stage.state != "completed"
                    && approval.actionable()
                {
                    "yes"
                } else {
                    "no"
                }
            )?;
        }
        if !found {
            writeln!(
                pipeline,
                "  No approval records yet (this does not mean approval is not required)."
            )?;
        }
    }
    write!(
        output,
        "{}",
        section("Stages & approvals", std::str::from_utf8(&pipeline)?)
    )?;
    Ok(())
}

pub(crate) fn render_frame(
    output: &mut impl Write,
    frame: &str,
    refresh_screen: bool,
) -> Result<()> {
    if refresh_screen {
        queue!(output, Clear(ClearType::All), MoveTo(0, 0))?;
    }
    if is_raw_mode_enabled()? {
        output.write_all(frame.replace('\n', "\r\n").as_bytes())?;
    } else {
        output.write_all(frame.as_bytes())?;
    }
    output.flush()?;
    Ok(())
}

pub(crate) fn pipeline_info(build: &Build) -> String {
    let title = build
        .definition
        .as_ref()
        .and_then(|value| value.name.as_deref());
    let repository = build
        .trigger_info
        .get("ci.triggerRepository")
        .map(String::as_str)
        .or_else(|| {
            build
                .repository
                .as_ref()
                .and_then(|value| value.name.as_deref().or(value.id.as_deref()))
        });
    let author = build
        .source_change
        .as_ref()
        .and_then(|change| change.author.as_ref())
        .and_then(|value| value.display_name.as_deref());
    let message = build
        .trigger_info
        .get("ci.message")
        .map(String::as_str)
        .or_else(|| {
            build
                .source_change
                .as_ref()
                .and_then(|change| change.message.as_deref())
        })
        .and_then(|message| message.lines().next());
    format!(
        "Pipeline: {} | Run: {}\nTitle: {}\nRepository: {} | Commit author: {}\n",
        title.unwrap_or("-"),
        build.build_number.as_deref().unwrap_or("-"),
        message.unwrap_or("-"),
        repository.unwrap_or("-"),
        author.unwrap_or("-"),
    )
}

pub(crate) fn section(title: &str, body: &str) -> String {
    let width = crossterm::terminal::size().map_or(100, |(columns, _)| usize::from(columns));
    section_with_width(title, body, width)
}

pub(crate) fn section_with_width(title: &str, body: &str, width: usize) -> String {
    let inner = width.max(title.width() + 8) - 4;
    let mut output = format!("+-- {title} {}+\n", "-".repeat(inner - title.width() - 2));
    for line in body.lines() {
        let mut wrapped = String::new();
        let mut used = 0;
        for character in line.chars() {
            let columns = character.width().unwrap_or(0);
            if used + columns > inner {
                output.push_str(&format!("| {wrapped}{} |\n", " ".repeat(inner - used)));
                wrapped.clear();
                used = 0;
            }
            wrapped.push(character);
            used += columns;
        }
        output.push_str(&format!("| {wrapped}{} |\n", " ".repeat(inner - used)));
    }
    output.push_str(&format!("+{}+\n", "-".repeat(inner + 2)));
    output
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

pub(crate) fn timer_line(
    build: &Build,
    watcher_elapsed: Duration,
    next_refresh: Option<Duration>,
    now: SystemTime,
) -> Result<String> {
    let pipeline = build
        .duration_at(now)?
        .map(format_duration)
        .unwrap_or_else(|| "-".into());
    let next = next_refresh
        .filter(|_| build.status != "completed")
        .map(|remaining| {
            // Round up so the countdown does not show zero before polling starts.
            format!(
                "{}s",
                remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
            )
        })
        .unwrap_or_else(|| "-".into());
    Ok(format!(
        "Watcher: {} | Next refresh: {next} | Pipeline: {pipeline}",
        format_duration(watcher_elapsed)
    ))
}
