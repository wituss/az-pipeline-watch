use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as FmtWrite,
    io::{IsTerminal, Write},
    time::Duration,
};

use crate::{
    azure::Api,
    controls::{Key, TerminalInput},
    display::{pipeline_info, render_frame, section},
    model::{Build, Record, approval_stages},
};

struct StageChoice {
    id: String,
    name: String,
    state: String,
    actionable_approvals: usize,
}

pub(crate) fn prompt_for_url() -> Result<String> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "Interactive mode requires a terminal"
    );
    print!("Azure DevOps build URL: ");
    std::io::stdout().flush()?;
    let mut url = String::new();
    std::io::stdin().read_line(&mut url)?;
    let url = url.trim().to_owned();
    ensure!(!url.is_empty(), "A build URL is required");
    Ok(url)
}

fn stage_choices(api: &impl Api, records: &[Record]) -> Result<Vec<StageChoice>> {
    let mut actionable_by_stage = BTreeMap::<String, usize>::new();
    for (approval_id, stage_id) in approval_stages(records, false)? {
        let approval = api.approval(&approval_id)?;
        if approval.status == "pending" && approval.actionable() {
            *actionable_by_stage.entry(stage_id).or_default() += 1;
        }
    }

    let mut stages: Vec<_> = records
        .iter()
        .filter(|record| record.kind == "Stage")
        .collect();
    stages.sort_by_key(|record| record.order);
    stages
        .into_iter()
        .map(|record| {
            let id = record.stage_id()?.to_owned();
            Ok(StageChoice {
                actionable_approvals: actionable_by_stage.get(&id).copied().unwrap_or_default(),
                id,
                name: record.name.clone(),
                state: record.state.clone(),
            })
        })
        .collect()
}

fn render_choices(
    build: &Build,
    stages: &[StageChoice],
    selected: &BTreeSet<String>,
    cursor: usize,
    output: &mut impl Write,
) -> Result<()> {
    let mut frame = String::from(
        "Select stages to approve\nUp/Down move  Space select  Enter continue  q/Esc/Ctrl-C cancel\n\n",
    );
    frame.push_str(&section("Pipeline info", &pipeline_info(build)));
    let mut choices = String::new();
    for (index, stage) in stages.iter().enumerate() {
        let marker = if selected.contains(&stage.id) {
            "[x]"
        } else {
            "[ ]"
        };
        let availability = if stage.actionable_approvals == 0 {
            "no approvals available yet".to_owned()
        } else {
            format!("{} approval(s) available", stage.actionable_approvals)
        };
        writeln!(
            choices,
            "{} {marker} {:<24} {:<12} {:<28} {}",
            if index == cursor { ">" } else { " " },
            stage.id,
            stage.state,
            availability,
            stage.name
        )?;
    }
    frame.push_str(&section("Stages & approvals", &choices));
    render_frame(output, &frame, true)?;
    Ok(())
}

pub(crate) fn select_stages(api: &impl Api) -> Result<Option<BTreeSet<String>>> {
    let build = api.build()?;
    ensure!(
        build.status == "inProgress",
        "Build {} is {}; interactive approvals require an in-progress build",
        build.id,
        build.status
    );
    let records = api.timeline()?;
    let stages = stage_choices(api, &records)?;
    ensure!(!stages.is_empty(), "Build timeline contains no stages");

    let terminal = TerminalInput::new(true)?;
    let mut selected = BTreeSet::new();
    let mut cursor = stages
        .iter()
        .position(|stage| stage.actionable_approvals > 0)
        .unwrap_or(0);
    loop {
        render_choices(
            &build,
            &stages,
            &selected,
            cursor,
            &mut std::io::stdout().lock(),
        )?;
        let key = loop {
            if let Some(key) = terminal.read_key(Duration::from_millis(100))? {
                break key;
            }
        };
        match key {
            Key::Up => cursor = cursor.saturating_sub(1),
            Key::Down => cursor = (cursor + 1).min(stages.len() - 1),
            Key::Toggle => {
                let id = stages[cursor].id.clone();
                if !selected.remove(&id) {
                    selected.insert(id);
                }
            }
            Key::Confirm => {
                if !selected.is_empty() {
                    return Ok(Some(selected));
                }
            }
            Key::Cancel | Key::Escape | Key::Interrupt => {
                if terminal.confirm_exit()? {
                    return Ok(None);
                }
            }
            Key::Other(_) => {}
        }
    }
}

pub(crate) fn show_selection(selected: &BTreeSet<String>) -> Result<()> {
    println!("Selected stages for approval:");
    for stage in selected {
        println!("  {stage}");
    }
    Ok(())
}

pub(crate) fn confirm_selection(selected: &BTreeSet<String>) -> Result<bool> {
    show_selection(selected)?;
    confirm_approval(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}

fn confirm_approval(input: &mut impl std::io::BufRead, output: &mut impl Write) -> Result<bool> {
    loop {
        write!(
            output,
            "Type 'approve' to submit these approvals, or anything else to cancel: "
        )?;
        output.flush()?;
        let mut response = String::new();
        if input.read_line(&mut response)? == 0 {
            return Ok(false);
        }
        if is_approval_confirmation(&response) {
            return Ok(true);
        }
        loop {
            write!(output, "Are you sure you want to exit? [y/N]: ")?;
            output.flush()?;
            response.clear();
            if input.read_line(&mut response)? == 0 {
                return Ok(false);
            }
            match response.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => return Ok(false),
                "" | "n" | "no" => break,
                _ => writeln!(output, "Please answer yes or no.")?,
            }
        }
    }
}

fn is_approval_confirmation(response: &str) -> bool {
    response.trim().eq_ignore_ascii_case("approve")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn approval_confirmation_requires_explicit_approval_token() {
        assert!(is_approval_confirmation(" approve \n"));
        assert!(!is_approval_confirmation("yes"));
        assert!(!is_approval_confirmation(""));
    }

    #[test]
    fn declining_exit_retries_approval_and_never_approves_from_yes_alone() {
        for input in [
            "wrong\nno\napprove\n",
            "\n\napprove\n",
            "no\ninvalid\nn\napprove\n",
        ] {
            assert!(confirm_approval(&mut input.as_bytes(), &mut Vec::new()).unwrap());
        }
        for input in ["no\nyes\n", "yes\ny\n", "", "no\n"] {
            assert!(!confirm_approval(&mut input.as_bytes(), &mut Vec::new()).unwrap());
        }
    }

    #[test]
    fn stage_choices_follow_timeline_and_only_mark_stages_with_approvals() {
        let records: Vec<Record> = serde_json::from_value(json!([
            {"id":"qa","identifier":"QA","name":"Quality","type":"Stage","state":"pending","order":2},
            {"id":"qa-check","parentId":"qa","name":"Check","type":"Checkpoint","state":"inProgress"},
            {"id":"qa-approval","parentId":"qa-check","name":"Approval","type":"Checkpoint.Approval","state":"inProgress"},
            {"id":"build","identifier":"Build","name":"Build","type":"Stage","state":"completed","order":1},
            {"id":"build-check","parentId":"build","name":"Check","type":"Checkpoint","state":"inProgress"},
            {"id":"build-approval","parentId":"build-check","name":"Approval","type":"Checkpoint.Approval","state":"inProgress"}
        ]))
        .unwrap();
        let choices = stage_choices(&FakeApi, &records).unwrap();
        assert_eq!(choices[0].id, "Build");
        assert_eq!(choices[0].actionable_approvals, 0);
        assert_eq!(choices[1].id, "QA");
        assert_eq!(choices[1].actionable_approvals, 1);
    }

    struct FakeApi;

    impl Api for FakeApi {
        fn build(&self) -> Result<crate::model::Build> {
            unreachable!()
        }

        fn timeline(&self) -> Result<Vec<Record>> {
            unreachable!()
        }

        fn approval(&self, id: &str) -> Result<crate::model::Approval> {
            Ok(crate::model::Approval {
                id: id.into(),
                status: "pending".into(),
                permissions: Some("view, update".into()),
                steps: Vec::new(),
            })
        }

        fn approve(&self, _id: &str, _comment: &str) -> Result<()> {
            unreachable!()
        }
    }
}
