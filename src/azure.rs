use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};
use wait_timeout::ChildExt;

use crate::args::Target;
use crate::model::{Approval, Build, BuildChange, Record, Timeline};

pub(crate) trait Api {
    fn build(&self) -> Result<Build>;
    fn timeline(&self) -> Result<Vec<Record>>;
    fn approval(&self, id: &str) -> Result<Approval>;
    fn approve(&self, id: &str, comment: &str) -> Result<()>;
}

pub(crate) struct Azure {
    pub(crate) target: Target,
    pub(crate) timeout: Duration,
}

impl Azure {
    fn invoke(
        &self,
        area: &str,
        resource: &str,
        routes: &[String],
        query: &[String],
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut command = Command::new("az");
        command.args([
            "devops",
            "invoke",
            "--organization",
            &self.target.organization,
            "--area",
            area,
            "--resource",
            resource,
            "--api-version",
            "7.1",
            "--only-show-errors",
            "--output",
            "json",
            "--route-parameters",
        ]);
        command
            .arg(format!("project={}", self.target.project))
            .args(routes);
        if !query.is_empty() {
            command.arg("--query-parameters").args(query);
        }
        let mut input = None;
        if let Some(body) = body {
            let mut file = tempfile::NamedTempFile::new()?;
            serde_json::to_writer(&mut file, body)?;
            file.flush()?;
            command.args(["--http-method", "PATCH", "--encoding", "utf-8", "--in-file"]);
            command.arg(file.path());
            input = Some(file);
        }
        let response = self.execute(&mut command, &format!("{area}/{resource}"));
        drop(input);
        response
    }

    fn execute(&self, command: &mut Command, operation: &str) -> Result<Value> {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let mut child = command
            .spawn()
            .context("Cannot start az; install Azure CLI and azure-devops extension")?;
        let mut stdout = child.stdout.take().context("No stdout pipe")?;
        let mut stderr = child.stderr.take().context("No stderr pipe")?;
        // Drain both pipes while waiting: timelines can exceed OS pipe capacity.
        let out = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let err = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = child.wait_timeout(self.timeout);
        let timed_out = matches!(status, Ok(None));
        if timed_out || status.is_err() {
            let _ = child.kill();
            child.wait().context("Cannot reap az process")?;
        }
        let stdout = out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
        let stderr = err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
        ensure!(
            !timed_out,
            "az {operation} timed out; stopping without retry"
        );
        let status = status?.context("az returned no exit status")?;
        ensure!(
            status.success(),
            "az {operation} failed ({status}): {}",
            String::from_utf8_lossy(&stderr).trim()
        );
        if !stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&stderr));
        }
        serde_json::from_slice(&stdout).context("az returned invalid JSON")
    }
}

impl Api for Azure {
    fn build(&self) -> Result<Build> {
        let value = self.invoke(
            "build",
            "builds",
            &[format!("buildId={}", self.target.build_id)],
            &[],
            None,
        )?;
        let mut build: Build = serde_json::from_value(value)?;
        ensure!(build.id == self.target.build_id, "Build ID mismatch");
        if let Some(version) = &build.source_version {
            let mut url = url::Url::parse(&self.target.organization)?;
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid organization URL"))?
                .push(&self.target.project)
                .extend(["_apis", "build", "builds", &build.id.to_string(), "changes"]);
            url.query_pairs_mut()
                .append_pair("api-version", "7.1")
                .append_pair("$top", "1")
                .append_pair("includeSourceChange", "true");
            let mut command = Command::new("az");
            command.args([
                "rest",
                "--method",
                "get",
                "--url",
                url.as_str(),
                "--resource",
                "499b84ac-1321-427f-aa17-267ca6975798",
                "--only-show-errors",
                "--output",
                "json",
            ]);
            #[derive(Deserialize)]
            struct Changes {
                value: Vec<BuildChange>,
            }
            let changes: Changes =
                serde_json::from_value(self.execute(&mut command, "build/source-change")?)?;
            build.source_change = changes
                .value
                .into_iter()
                .find(|change| &change.id == version);
        }
        Ok(build)
    }

    fn timeline(&self) -> Result<Vec<Record>> {
        let value = self.invoke(
            "build",
            "timeline",
            &[format!("buildId={}", self.target.build_id)],
            &[],
            None,
        )?;
        Ok(serde_json::from_value::<Timeline>(value)?.records)
    }

    fn approval(&self, id: &str) -> Result<Approval> {
        let value = self.invoke(
            "approval",
            "approvals",
            &[],
            &[
                format!("approvalIds={id}"),
                "$expand=steps,permissions".into(),
            ],
            None,
        )?;
        #[derive(Deserialize)]
        struct Response {
            value: Vec<Approval>,
        }
        let mut response: Response = serde_json::from_value(value)?;
        ensure!(
            response.value.len() == 1 && response.value[0].id == id,
            "Approval query returned an unexpected ID or count"
        );
        Ok(response.value.remove(0))
    }

    fn approve(&self, id: &str, comment: &str) -> Result<()> {
        self.invoke(
            "approval",
            "approvals",
            &[],
            &[],
            Some(&json!([{"approvalId": id, "status": "approved", "comment": comment}])),
        )?;
        Ok(())
    }
}
