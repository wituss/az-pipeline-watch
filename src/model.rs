use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, SystemTime},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Build {
    pub(crate) id: u64,
    pub(crate) status: String,
    pub(crate) result: Option<String>,
    pub(crate) start_time: Option<DateTime<Utc>>,
    pub(crate) finish_time: Option<DateTime<Utc>>,
    pub(crate) build_number: Option<String>,
    pub(crate) definition: Option<BuildDefinition>,
    pub(crate) repository: Option<BuildRepository>,
    pub(crate) source_version: Option<String>,
    #[serde(default)]
    pub(crate) trigger_info: BTreeMap<String, String>,
    #[serde(skip)]
    pub(crate) source_change: Option<BuildChange>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BuildChange {
    pub(crate) id: String,
    pub(crate) message: Option<String>,
    pub(crate) author: Option<BuildIdentity>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BuildDefinition {
    pub(crate) name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BuildRepository {
    pub(crate) name: Option<String>,
    pub(crate) id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildIdentity {
    pub(crate) display_name: Option<String>,
}

impl Build {
    pub(crate) fn duration_at(&self, now: SystemTime) -> Result<Option<Duration>> {
        let Some(start) = self.start_time else {
            return Ok(None);
        };
        let end = match self.finish_time {
            Some(finish) => finish,
            None if self.status == "completed" => return Ok(None),
            None => now.into(),
        };
        Ok(Some((end - start).to_std().context(
            "Pipeline end time precedes its start time; check timestamps and local clock",
        )?))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) identifier: Option<String>,
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) state: String,
    pub(crate) result: Option<String>,
    pub(crate) order: Option<u64>,
    pub(crate) task: Option<Task>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Task {
    pub(crate) name: String,
}

impl Record {
    pub(crate) fn stage_id(&self) -> Result<&str> {
        self.identifier
            .as_deref()
            .context("Stage record has no identifier")
    }
}

#[derive(Deserialize)]
pub(crate) struct Timeline {
    pub(crate) records: Vec<Record>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Approval {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) permissions: Option<String>,
    #[serde(default)]
    pub(crate) steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Step {
    pub(crate) status: String,
    pub(crate) comment: Option<String>,
    pub(crate) permissions: Option<String>,
}

fn can_update(permissions: Option<&str>) -> bool {
    permissions.is_some_and(|s| {
        s.split(',')
            .any(|p| p.trim().eq_ignore_ascii_case("update"))
    })
}

impl Approval {
    pub(crate) fn actionable(&self) -> bool {
        self.status == "pending"
            && (can_update(self.permissions.as_deref())
                || self
                    .steps
                    .iter()
                    .any(|s| s.status == "pending" && can_update(s.permissions.as_deref())))
    }
}

pub(crate) fn approval_stages(
    records: &[Record],
    include_completed: bool,
) -> Result<BTreeMap<String, String>> {
    let mut by_id = BTreeMap::new();
    for record in records {
        ensure!(
            by_id.insert(record.id.as_str(), record).is_none(),
            "Duplicate timeline ID {}; refusing ambiguous approval mapping",
            record.id
        );
    }
    let mut approvals = BTreeMap::new();
    for record in records.iter().filter(|r| {
        r.kind == "Checkpoint.Approval" && (include_completed || r.state != "completed")
    }) {
        let mut current = record;
        let mut visited = HashSet::new();
        while current.kind != "Stage" {
            ensure!(visited.insert(&current.id), "Cycle in timeline parents");
            current = by_id
                .get(
                    current
                        .parent_id
                        .as_deref()
                        .context("Approval has no stage ancestor")?,
                )
                .context("Approval references missing timeline parent")?;
        }
        if include_completed || current.state != "completed" {
            approvals.insert(record.id.clone(), current.stage_id()?.to_owned());
        }
    }
    Ok(approvals)
}
