use anyhow::Result;
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
        output.write_all(b"\x1b[2J\x1b[H")?;
    }
    output.write_all(frame.as_bytes())?;
    output.flush()?;
    Ok(())
}
