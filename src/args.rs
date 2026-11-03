use anyhow::{Context, Result, ensure};
use clap::Parser;
use url::Url;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Watch an Azure DevOps YAML pipeline using your existing az credentials.",
    after_help = "Run without a URL to enter interactive mode. Use arrow keys to navigate, Space to select,\nEnter to continue, and q or Esc to cancel. Before any approvals are sent, type 'approve'.\n\nBy default NO approvals are submitted. Only environment/check approvals are supported,\nnot ManualValidation tasks or classic releases. Other checks and permissions are never bypassed.\n\nExamples:\n  az-pipeline-watch\n  az-pipeline-watch 'https://dev.azure.com/ORG/PROJECT/_build/results?buildId=123'\n  az-pipeline-watch URL --approve-through Stage_uat --dry-run\n  az-pipeline-watch URL --approve-all --dry-run\n\n--approve-through approves stages in timeline order through the named stage, inclusive.\nThe command fails if the named stage does not exist in this build's timeline.\nStages before the boundary are selected by pipeline order, not inferred dependencies;\nparallel stages before it may also be selected. Later stages are not approved."
)]
pub(crate) struct Args {
    /// Build results URL (quote it if it contains '&'); omit for interactive mode
    pub(crate) url: Option<String>,
    /// Seconds between completed polling cycles
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) interval: u64,
    /// Approve stages in pipeline order through this exact stage ID
    #[arg(long, conflicts_with = "approve_all")]
    pub(crate) approve_through: Option<String>,
    /// Approve all available approvals belonging to this build, including production
    #[arg(long)]
    pub(crate) approve_all: bool,
    /// Print planned approvals, but never send PATCH requests
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Check once and exit (approval flags still apply unless --dry-run is set)
    #[arg(long)]
    pub(crate) once: bool,
    /// List stages and their existing approvals once, without submitting any votes
    #[arg(long, conflicts_with_all = ["approve_all", "approve_through"])]
    pub(crate) list_stages: bool,
    /// Timeout in seconds for each az invocation
    #[arg(long, default_value = "120", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) request_timeout: u64,
}

#[derive(Debug)]
pub(crate) struct Target {
    pub(crate) organization: String,
    pub(crate) project: String,
    pub(crate) build_id: u64,
}

impl Target {
    pub(crate) fn parse(input: &str) -> Result<Self> {
        let url = Url::parse(input).context("Invalid build URL")?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("dev.azure.com")
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "Expected https://dev.azure.com/ORG/PROJECT/_build/results?buildId=ID"
        );
        let parts: Vec<_> = url.path_segments().context("Missing URL path")?.collect();
        ensure!(
            parts.len() == 4 && parts[2] == "_build" && parts[3] == "results",
            "Expected /ORG/PROJECT/_build/results"
        );
        let decode = |s: &str| -> Result<String> {
            // form_urlencoded decodes percent escapes; preserve literal '+' in path segments.
            let encoded = format!("v={}", s.replace('+', "%2B"));
            let value = url::form_urlencoded::parse(encoded.as_bytes())
                .next()
                .context("Empty path segment")?
                .1
                .into_owned();
            ensure!(
                !value.is_empty() && !value.starts_with('-'),
                "Invalid path segment"
            );
            Ok(value)
        };
        let ids: Vec<_> = url.query_pairs().filter(|(k, _)| k == "buildId").collect();
        ensure!(ids.len() == 1, "URL must contain exactly one buildId");
        let build_id = ids[0]
            .1
            .parse()
            .context("buildId must be a positive integer")?;
        ensure!(build_id > 0, "buildId must be positive");
        Ok(Self {
            organization: format!("https://dev.azure.com/{}", parts[0]),
            project: decode(parts[1])?,
            build_id,
        })
    }
}
