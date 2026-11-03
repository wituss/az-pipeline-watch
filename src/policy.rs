use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;

use crate::{args::Args, model::Record};

#[derive(Debug)]
pub(crate) enum Policy {
    None,
    All,
    Through(String),
}

impl Policy {
    pub(crate) fn from_args(args: &Args) -> Self {
        if args.approve_all {
            return Self::All;
        }
        args.approve_through
            .as_ref()
            .map_or(Self::None, |stage| Self::Through(stage.clone()))
    }

    pub(crate) fn selected_stages(&self, records: &[Record]) -> Result<BTreeSet<String>> {
        let mut stages = records
            .iter()
            .filter(|record| record.kind == "Stage")
            .map(|record| Ok((record.stage_id()?.to_owned(), record.order)))
            .collect::<Result<Vec<_>>>()?;
        let all: BTreeSet<_> = stages.iter().map(|(stage, _)| stage.clone()).collect();
        ensure!(
            all.len() == stages.len(),
            "Duplicate stage IDs in this build's timeline"
        );
        if let Self::Through(target) = self {
            ensure!(
                stages.iter().any(|(stage, _)| stage == target),
                "Stage {target} does not exist in this build's timeline"
            );
            ensure!(
                stages.iter().all(|(_, order)| order.is_some()),
                "Stage order is missing from this build's timeline"
            );
            stages.sort_by_key(|(_, order)| *order);
            let target_position = stages
                .iter()
                .position(|(stage, _)| stage == target)
                .context("Target stage disappeared from timeline")?;
            return Ok(stages
                .into_iter()
                .take(target_position + 1)
                .map(|(stage, _)| stage)
                .collect());
        }
        if matches!(self, Self::All) {
            return Ok(all);
        }
        Ok(BTreeSet::new())
    }
}
