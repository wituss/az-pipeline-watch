use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{IsTerminal, Write},
    thread,
    time::Duration,
};

use crate::{
    azure::Api,
    controls::{Key, TerminalInput, next_key},
    model::{Record, approval_stages},
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
    stages: &[StageChoice],
    selected: &BTreeSet<String>,
    cursor: usize,
    output: &mut impl Write,
) -> Result<()> {
    write!(
        output,
        "\x1b[2J\x1b[HSelect stages to approve\n↑/↓ move  Space select  Enter continue  q cancel\n\n"
    )?;
    for (index, stage) in stages.iter().enumerate() {
        let marker = if selected.contains(&stage.id) {
            "[x]"
        } else {
            "[ ]"
        };
        let availability = if stage.actionable_approvals == 0 {
            "no actionable approvals".to_owned()
        } else {
            format!("{} approval(s) available", stage.actionable_approvals)
        };
        writeln!(
            output,
            "{} {marker} {:<24} {:<12} {:<28} {}",
            if index == cursor { ">" } else { " " },
            stage.id,
            stage.state,
            availability,
            stage.name
        )?;
    }
    output.flush()?;
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
    let mut pending = VecDeque::new();
    loop {
        render_choices(&stages, &selected, cursor, &mut std::io::stdout().lock())?;
        let mut escape_started = None;
        let key = loop {
            if let Some(key) = next_key(&mut pending) {
                break key;
            }
            let bytes = terminal.read_keys()?;
            if !bytes.is_empty() {
                pending.extend(bytes);
                if pending.front() == Some(&0x1b) {
                    escape_started.get_or_insert_with(std::time::Instant::now);
                }
                continue;
            }
            if pending.front() == Some(&0x1b)
                && escape_started.is_some_and(|start: std::time::Instant| {
                    start.elapsed() >= Duration::from_millis(120)
                })
            {
                pending.pop_front();
                break Key::Cancel;
            }
            thread::sleep(Duration::from_millis(20));
        };
        match key {
            Key::Up => cursor = cursor.saturating_sub(1),
            Key::Down => cursor = (cursor + 1).min(stages.len() - 1),
            Key::Toggle if stages[cursor].actionable_approvals > 0 => {
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
            Key::Cancel => return Ok(None),
            Key::Toggle | Key::Other(_) => {}
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
    print!("Type 'approve' to submit these approvals, or anything else to cancel: ");
    std::io::stdout().flush()?;
    let mut response = String::new();
    std::io::stdin().read_line(&mut response)?;
    Ok(is_approval_confirmation(&response))
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
