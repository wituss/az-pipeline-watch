use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    queue,
    terminal::{Clear, ClearType, is_raw_mode_enabled},
};
use std::io::Write;

use crate::{azure::Api, model::approval_stages};

pub(crate) fn list_stages(api: &impl Api, output: &mut impl Write) -> Result<()> {
    let build = api.build()?;
    let records = api.timeline()?;
    let approvals = approval_stages(&records, true)?;
    writeln!(
        output,
        "Build {}: {} / {} (read-only)",
        build.id,
        build.status,
        build.result.as_deref().unwrap_or("-")
    )?;
    let mut stages: Vec<_> = records.iter().filter(|r| r.kind == "Stage").collect();
    stages.sort_by_key(|r| r.order);
    for stage in stages {
        let name = stage.stage_id()?;
        writeln!(
            output,
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
                output,
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
                output,
                "  No approval records yet (this does not mean approval is not required)."
            )?;
        }
    }
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

pub(crate) fn render_refresh_interval(interval: u64) -> Result<()> {
    let mut output = std::io::stdout().lock();
    queue!(output, MoveTo(0, 2), Clear(ClearType::CurrentLine))?;
    write!(
        output,
        "Refresh interval: {interval}s | r refresh, + faster, - slower, Ctrl-C exit"
    )?;
    output.flush()?;
    Ok(())
}
