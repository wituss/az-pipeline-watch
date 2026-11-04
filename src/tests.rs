use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::json;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    args::{Args, Target},
    azure::Api,
    controls::adjust_interval,
    display::{
        format_duration, list_stages, pipeline_info, render_frame, section_with_width, timer_line,
    },
    model::{Approval, Build, Record, Step, approval_stages},
    policy::Policy,
    watch::cycle,
};

fn through(name: &str) -> Policy {
    Policy::Through(name.to_owned())
}

#[test]
fn approve_through_selects_pipeline_order_and_validates_target() {
    let selected = through("UAT").selected_stages(&records()).unwrap();
    assert_eq!(
        selected,
        BTreeSet::from(["Build".into(), "Dev".into(), "QA".into(), "UAT".into()])
    );
    assert!(through("Missing").selected_stages(&records()).is_err());
}

#[test]
fn selected_policy_only_approves_selected_stages_and_rejects_stale_selection() {
    let selected = BTreeSet::from(["QA".to_owned()]);
    assert_eq!(
        Policy::Selected(selected.clone())
            .selected_stages(&records())
            .unwrap(),
        selected
    );
    assert!(
        Policy::Selected(BTreeSet::from(["Removed".to_owned()]))
            .selected_stages(&records())
            .is_err()
    );
}

#[test]
fn cli_defaults_to_no_approvals_and_rejects_conflicting_flags() {
    let url = "https://dev.azure.com/org/project/_build/results?buildId=1";
    assert!(Args::try_parse_from(["watch"]).unwrap().url.is_none());
    let args = Args::try_parse_from(["watch", url]).unwrap();
    assert!(matches!(Policy::from_args(&args), Policy::None));
    assert!(!args.autoapprove);
    assert!(
        Args::try_parse_from(["watch", "--autoapprove"])
            .unwrap()
            .autoapprove
    );
    for conflicting in [
        vec!["watch", url, "--autoapprove"],
        vec!["watch", "--autoapprove", "--approve-all"],
        vec!["watch", "--autoapprove", "--approve-through", "UAT"],
        vec!["watch", "--autoapprove", "--list-stages"],
    ] {
        assert!(Args::try_parse_from(conflicting).is_err());
    }
    let through_args = Args::try_parse_from(["watch", url, "--approve-through", "UAT"]).unwrap();
    assert!(matches!(Policy::from_args(&through_args), Policy::Through(stage) if stage == "UAT"));
    assert!(
        Args::try_parse_from(["watch", url, "--approve-through", "UAT", "--approve-all"]).is_err()
    );
    assert!(Args::try_parse_from(["watch", url, "--pipeline-yaml", "file"]).is_err());
    assert!(Args::try_parse_from(["watch", url, "--interval", "0"]).is_err());
    assert!(
        Args::try_parse_from(["watch", url, "--list-stages"])
            .unwrap()
            .list_stages
    );
    assert!(Args::try_parse_from(["watch", url, "--list-stages", "--approve-all"]).is_err());
    assert!(
        Args::try_parse_from(["watch", url, "--list-stages", "--approve-through", "UAT",]).is_err()
    );
}

#[test]
fn pipeline_metadata_uses_azure_fields_and_handles_missing_metadata() {
    let build: Build = serde_json::from_value(json!({
        "id": 42, "status": "inProgress", "buildNumber": "20261104.1",
        "definition": {"name": "Deploy service"},
        "repository": {"name": "service"},
        "requestedFor": {"displayName": "Test Author"}
    }))
    .unwrap();
    assert_eq!(
        pipeline_info(&build),
        "Pipeline: Deploy service | Run: 20261104.1\nTitle: -\nRepository: service | Commit author: -\n"
    );
    let build = Fake::default().build().unwrap();
    assert_eq!(
        pipeline_info(&build),
        "Pipeline: - | Run: -\nTitle: -\nRepository: - | Commit author: -\n"
    );
}

#[test]
fn github_metadata_uses_trigger_title_repository_id_and_source_commit_author() {
    let mut build: Build = serde_json::from_value(json!({
        "id": 42, "status": "inProgress",
        "repository": {"id": "org/service", "type": "GitHub"},
        "requestedFor": {"displayName": "Build Service"},
        "triggerInfo": {"ci.message": "Merge feature\nExtra details"}
    }))
    .unwrap();
    build.source_change = Some(
        serde_json::from_value(json!({
            "id": "source-sha", "message": "Commit title",
            "author": {"displayName": "Commit Author"}
        }))
        .unwrap(),
    );
    let info = pipeline_info(&build);
    assert!(info.contains("Title: Merge feature\n"));
    assert!(info.contains("Repository: org/service | Commit author: Commit Author\n"));
    assert!(!info.contains("Requested for"));
    build.trigger_info.clear();
    assert!(pipeline_info(&build).contains("Title: Commit title\n"));
    build
        .trigger_info
        .insert("ci.triggerRepository".into(), "org/trigger".into());
    assert!(pipeline_info(&build).contains("Repository: org/trigger |"));
}

#[test]
fn terminal_refresh_clears_screen_but_logs_remain_plain_text() {
    let mut terminal = Vec::new();
    render_frame(&mut terminal, "pipeline status\n", true).unwrap();
    assert_eq!(&terminal, b"\x1b[2J\x1b[1;1Hpipeline status\n");

    let mut log = Vec::new();
    render_frame(&mut log, "pipeline status\n", false).unwrap();
    assert_eq!(&log, b"pipeline status\n");
}

#[test]
fn section_frames_wrap_long_lines_and_preserve_unicode_display_width() {
    let frame = section_with_width("Timers", "1234567890123456789012345\n🟣 test", 24);
    for line in frame.lines() {
        assert_eq!(unicode_width::UnicodeWidthStr::width(line), 24);
    }
    assert!(frame.starts_with("+-- Timers "));
    assert!(frame.contains("| 12345678901234567890 |\n| 12345"));
    assert!(frame.contains("| 🟣 test"));
}

#[test]
fn timers_use_build_timestamps_and_keep_completed_duration_fixed() {
    let start: SystemTime = "2026-11-04T08:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap()
        .into();
    let mut build: Build = serde_json::from_value(json!({
        "id": 42, "status": "inProgress",
        "startTime": "2026-11-04T09:00:00.5000000+01:00"
    }))
    .unwrap();
    let now = start + Duration::from_secs(3662);
    assert_eq!(
        timer_line(
            &build,
            Duration::from_secs(62),
            Some(Duration::from_millis(2100)),
            now
        )
        .unwrap(),
        "Watcher: 00:01:02 | Next refresh: 3s | Pipeline: 01:01:01"
    );
    build.status = "completed".into();
    build.finish_time = Some("2026-11-04T08:01:00.500Z".parse().unwrap());
    for now in [now, now + Duration::from_secs(600)] {
        assert_eq!(
            timer_line(
                &build,
                Duration::from_secs(90),
                Some(Duration::from_secs(30)),
                now
            )
            .unwrap(),
            "Watcher: 00:01:30 | Next refresh: - | Pipeline: 00:01:00"
        );
    }
    assert_eq!(format_duration(Duration::from_secs(90061)), "25:01:01");
}

#[test]
fn timers_handle_missing_timestamps_and_reject_invalid_ones() {
    let mut build: Build =
        serde_json::from_value(json!({"id": 42, "status": "notStarted"})).unwrap();
    assert_eq!(
        timer_line(&build, Duration::ZERO, None, SystemTime::now()).unwrap(),
        "Watcher: 00:00:00 | Next refresh: - | Pipeline: -"
    );
    build.status = "completed".into();
    build.start_time = Some("2026-11-04T08:00:00Z".parse().unwrap());
    assert_eq!(build.duration_at(SystemTime::now()).unwrap(), None);
    build.finish_time = Some("2026-11-04T07:59:59Z".parse().unwrap());
    assert!(build.duration_at(SystemTime::now()).is_err());
    assert!(
        serde_json::from_value::<Build>(json!({
            "id": 42, "status": "inProgress", "startTime": "invalid"
        }))
        .is_err()
    );
}

#[test]
fn keyboard_changes_refresh_interval_in_five_second_steps() {
    assert_eq!(adjust_interval(30, b'+'), Some(25));
    assert_eq!(adjust_interval(30, b'='), Some(25));
    assert_eq!(adjust_interval(30, b'-'), Some(35));
    assert_eq!(adjust_interval(3, b'+'), Some(1));
    assert_eq!(adjust_interval(3600, b'-'), Some(3600));
    assert_eq!(adjust_interval(30, b'r'), None);
}

#[test]
fn target_requires_one_build_and_decodes_project() {
    let target = Target::parse(
        "https://dev.azure.com/org/Project%20One/_build/results?buildId=42&view=results",
    )
    .unwrap();
    assert_eq!(target.project, "Project One");
    assert_eq!(target.build_id, 42);
    for url in [
        "http://dev.azure.com/org/p/_build/results?buildId=1",
        "https://evil.test/org/p/_build/results?buildId=1",
        "https://dev.azure.com/org/p/_build/results?buildId=1&buildId=2",
        "https://dev.azure.com/org/p/_build/results?buildId=0",
    ] {
        assert!(Target::parse(url).is_err());
    }
}

fn records() -> Vec<Record> {
    let mut records = Vec::new();
    for (order, name) in ["Build", "Dev", "QA", "UAT", "PRD", "Scan"]
        .iter()
        .enumerate()
    {
        records.push(json!({
            "id": name, "identifier": name, "name": name, "type": "Stage",
            "state": "pending", "order": order
        }));
        records.push(json!({
            "id": format!("{name}-checkpoint"), "parentId": name, "name": "Checkpoint",
            "type": "Checkpoint", "state": "inProgress"
        }));
        records.push(json!({
            "id": format!("{name}-approval"), "parentId": format!("{name}-checkpoint"),
            "name": "Checkpoint.Approval", "type": "Checkpoint.Approval", "state": "inProgress"
        }));
    }
    serde_json::from_value(json!(records)).unwrap()
}

struct Fake {
    writes: RefCell<BTreeMap<String, String>>,
    permission: &'static str,
    completed: bool,
    verify: bool,
    timeline: Vec<Record>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            writes: RefCell::new(BTreeMap::new()),
            permission: "view, update",
            completed: false,
            verify: true,
            timeline: records(),
        }
    }
}

impl Api for Fake {
    fn build(&self) -> Result<Build> {
        Ok(Build {
            id: 42,
            status: if self.completed {
                "completed"
            } else {
                "inProgress"
            }
            .into(),
            result: self.completed.then(|| "succeeded".into()),
            start_time: None,
            finish_time: None,
            build_number: None,
            definition: None,
            repository: None,
            source_version: None,
            trigger_info: BTreeMap::new(),
            source_change: None,
        })
    }

    fn timeline(&self) -> Result<Vec<Record>> {
        // Use the same deserializer as the real API without making production types Clone.
        let value = json!(
            self.timeline
                .iter()
                .map(|r| json!({
                    "id": r.id, "parentId": r.parent_id, "identifier": r.identifier,
                    "name": r.name, "type": r.kind, "state": r.state, "order": r.order
                }))
                .collect::<Vec<_>>()
        );
        Ok(serde_json::from_value(value)?)
    }

    fn approval(&self, id: &str) -> Result<Approval> {
        let comment = self
            .writes
            .borrow()
            .get(id)
            .cloned()
            .filter(|_| self.verify);
        Ok(Approval {
            id: id.into(),
            status: "pending".into(),
            permissions: Some(self.permission.into()),
            steps: comment
                .into_iter()
                .map(|comment| Step {
                    status: "approved".into(),
                    comment: Some(comment),
                    permissions: None,
                })
                .collect(),
        })
    }

    fn approve(&self, id: &str, comment: &str) -> Result<()> {
        ensure!(
            !self.writes.borrow().contains_key(id),
            "Duplicate approval submission"
        );
        self.writes.borrow_mut().insert(id.into(), comment.into());
        Ok(())
    }
}

fn tick(
    api: &Fake,
    policy: &Policy,
    dry_run: bool,
    submitted: &mut HashSet<String>,
) -> Result<Build> {
    cycle(
        api,
        policy,
        dry_run,
        false,
        Some(30),
        false,
        Instant::now(),
        submitted,
        "test",
        &AtomicBool::new(false),
    )
    .map(|(build, _)| build)
}

#[test]
fn default_dry_run_no_permission_and_completed_build_never_write() {
    for (policy, dry_run, permission, completed) in [
        (Policy::None, false, "view, update", false),
        (Policy::All, true, "view, update", false),
        (Policy::All, false, "view", false),
        (Policy::All, false, "view, update", true),
    ] {
        let api = Fake {
            permission,
            completed,
            ..Fake::default()
        };
        tick(&api, &policy, dry_run, &mut HashSet::new()).unwrap();
        assert!(api.writes.borrow().is_empty());
    }
}

#[test]
fn through_writes_stages_in_order_and_verifies_vote_without_repeating() {
    let api = Fake::default();
    let mut submitted = HashSet::new();
    tick(&api, &through("UAT"), false, &mut submitted).unwrap();
    tick(&api, &through("UAT"), false, &mut submitted).unwrap();
    let keys: Vec<_> = api.writes.borrow().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "Build-approval",
            "Dev-approval",
            "QA-approval",
            "UAT-approval"
        ]
    );
    assert_eq!(submitted.len(), 4);
}

#[test]
fn selected_policy_writes_only_selected_stage_approvals() {
    let api = Fake::default();
    let mut submitted = HashSet::new();
    tick(
        &api,
        &Policy::Selected(BTreeSet::from(["QA".to_owned()])),
        false,
        &mut submitted,
    )
    .unwrap();
    assert_eq!(
        api.writes.borrow().keys().cloned().collect::<Vec<_>>(),
        ["QA-approval"]
    );
}

#[test]
fn all_can_include_production_but_stopping_prevents_writes() {
    let api = Fake::default();
    tick(&api, &Policy::All, false, &mut HashSet::new()).unwrap();
    assert_eq!(api.writes.borrow().len(), 6);
    let api = Fake::default();
    cycle(
        &api,
        &Policy::All,
        false,
        false,
        Some(30),
        false,
        Instant::now(),
        &mut HashSet::new(),
        "test",
        &AtomicBool::new(true),
    )
    .unwrap();
    assert!(api.writes.borrow().is_empty());
}

#[test]
fn missing_target_and_broken_parent_chain_fail_before_writing() {
    let mut api = Fake::default();
    assert!(tick(&api, &through("Missing"), false, &mut HashSet::new()).is_err());
    assert!(api.writes.borrow().is_empty());
    api.timeline.retain(|r| r.id != "Scan");
    assert!(tick(&api, &through("UAT"), false, &mut HashSet::new()).is_err());
    assert!(api.writes.borrow().is_empty());
    let mut api = Fake::default();
    api.timeline
        .iter_mut()
        .find(|r| r.id == "QA-checkpoint")
        .unwrap()
        .parent_id = Some("missing".into());
    assert!(tick(&api, &Policy::All, false, &mut HashSet::new()).is_err());
    assert!(api.writes.borrow().is_empty());
}

#[test]
fn unconfirmed_write_is_an_error_not_a_retry() {
    let api = Fake {
        verify: false,
        ..Fake::default()
    };
    let mut submitted = HashSet::new();
    let error = tick(&api, &Policy::All, false, &mut submitted).unwrap_err();
    assert!(error.to_string().contains("not confirmed"));
    assert_eq!(api.writes.borrow().len(), 1);
    assert!(submitted.is_empty());
}

#[test]
fn listing_includes_completed_approvals_and_never_writes() {
    let mut api = Fake::default();
    for record in &mut api.timeline {
        if record.id.starts_with("QA") {
            record.state = "completed".into();
        }
    }
    api.timeline.retain(|r| r.id != "PRD-approval");
    let mut output = Vec::new();
    list_stages(&api, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches("+-- Stages & approvals ").count(), 1);
    assert_eq!(output.matches("Approval QA-approval:").count(), 1);
    assert!(output.contains("Approval QA-approval: pending; can approve now: no"));
    assert!(output.contains("Approval UAT-approval: pending; can approve now: yes"));
    assert!(output.contains("No approval records yet"));
    assert!(api.writes.borrow().is_empty());
    assert!(
        !approval_stages(&api.timeline, false)
            .unwrap()
            .contains_key("QA-approval")
    );
}

#[test]
fn listing_reports_no_action_on_finished_build() {
    let api = Fake {
        completed: true,
        ..Fake::default()
    };
    let mut output = Vec::new();
    list_stages(&api, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Approval PRD-approval: pending; can approve now: no"));
    assert!(!output.contains("can approve now: yes"));
    assert!(api.writes.borrow().is_empty());
}
