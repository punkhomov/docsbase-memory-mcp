//! Configuration inspection and explicit application of a reviewed plan.
use anyhow::Context as _;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::ipc::{
    async_client::{AsyncClient, within_operation},
    client::Client,
    protocol::{self, Request, Response},
};

mod apply;
pub(super) mod startup;
pub use startup::StartupOptions;

/// Optional commands under the legacy `config` command.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show current source values with units, scopes, origins and build capabilities.
    Effective {
        /// Versioned output format.
        #[arg(long, value_enum, default_value = "json")]
        format: ViewFormat,
    },
    /// Show which configured changes require replacing the selected project index.
    Plan {
        /// Project directory. Defaults to the current directory.
        path: Option<std::path::PathBuf>,
        /// Inspect the running v5 daemon without starting or reloading it.
        #[arg(long)]
        live: bool,
        /// Return 3 for a required change and 5 when observation was blocked.
        #[arg(long)]
        check: bool,
        /// Versioned output format.
        #[arg(long, value_enum, default_value = "json")]
        format: ViewFormat,
    },
    /// Apply a reviewed configuration plan to an existing v5 project.
    Apply {
        /// Project directory. Defaults to the current directory.
        path: Option<PathBuf>,
        /// Exact ID printed by `config plan`.
        #[arg(long)]
        plan_id: String,
        /// Permit remote embedding requests that may incur costs.
        #[arg(long)]
        allow_remote: bool,
        /// Apply a plan generated from the running daemon.
        #[arg(long)]
        live: bool,
    },
    /// Preview migration from actual registered sources; never writes or applies.
    Migrate {
        /// Target configuration version (currently 2 only).
        #[arg(long, value_parser = clap::value_parser!(u32).range(2..=2))]
        to: u32,
        /// Versioned output format.
        #[arg(long, value_enum, default_value = "json")]
        format: ViewFormat,
        /// Explicit global vector capacity in bytes; does not authorize applying it.
        #[arg(long, conflicts_with = "accept_suggested_vector_budget")]
        vector_max_bytes: Option<u64>,
        /// Select the maximum observed legacy capacity, without applying it.
        #[arg(long)]
        accept_suggested_vector_budget: bool,
    },
}
/// Formats supported by the versioned effective view.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ViewFormat {
    /// Machine-readable versioned JSON.
    Json,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CheckStatus {
    #[error("configuration changes require action")]
    Required,
    #[error("configuration plan could not confirm the selected project state")]
    Unavailable,
    #[error("configuration plan changed; review a new config plan")]
    Stale,
    #[error("configuration plan requires --allow-remote to authorize possible embedding costs")]
    MissingGrants,
    #[error("live configuration plan requires a running v5 daemon")]
    DaemonUnavailable,
    #[error("configuration changes require consent from an interactive terminal")]
    PromptUnavailable,
    #[error("startup configuration plan was declined")]
    Declined,
}

impl CheckStatus {
    pub(super) const fn exit_code(&self) -> u8 {
        match self {
            Self::Required => 3,
            Self::Stale => 4,
            Self::Unavailable
            | Self::MissingGrants
            | Self::DaemonUnavailable
            | Self::PromptUnavailable
            | Self::Declined => 5,
        }
    }
}

pub(super) fn run(
    command: &ConfigCommand,
    db: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    match command {
        ConfigCommand::Plan {
            path,
            check,
            live,
            format: ViewFormat::Json,
        } => {
            let ProjectPaths {
                config,
                cache,
                root,
            } = ProjectPaths::resolve(path.as_deref())?;
            if *live {
                return live_plan(cache, root, *check);
            }
            let limits = crate::config::observe::PlanningLimits::standard();
            let report = crate::config::observe::project_plan(&config, &cache, &root, db, limits)?;
            let encoded = serde_json::to_string(&report)?;
            anyhow::ensure!(
                encoded.len() <= limits.max_output_bytes,
                "configuration plan exceeds the output limit"
            );
            println!("{encoded}");
            if *check && !report.plan.body.blockers.is_empty() {
                return Err(CheckStatus::Unavailable.into());
            }
            if *check && !report.plan.body.actions.is_empty() {
                return Err(CheckStatus::Required.into());
            }
            Ok(())
        }
        ConfigCommand::Apply {
            path,
            plan_id,
            allow_remote,
            live,
        } => {
            let started = Instant::now();
            let paths = ProjectPaths::resolve(path.as_deref())?;
            if *live {
                live_apply(paths.cache, paths.root, plan_id, *allow_remote, started)
            } else {
                apply::run(paths, plan_id, *allow_remote, db)
            }
        }
        ConfigCommand::Migrate {
            to: _,
            format: ViewFormat::Json,
            vector_max_bytes,
            accept_suggested_vector_budget,
        } => {
            use crate::config::{
                migrate::VectorBudgetDecision,
                observe::{self, PlanningLimits},
            };
            let cwd = std::env::current_dir()?;
            let absolute = |path: std::path::PathBuf| {
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            };
            let config = absolute(crate::config::paths::config_dir()?);
            let cache = absolute(crate::config::paths::cache_dir()?);
            let decision = if let Some(bytes) = vector_max_bytes {
                Some(VectorBudgetDecision::UseBytes(*bytes))
            } else if *accept_suggested_vector_budget {
                Some(VectorBudgetDecision::AcceptSuggestedMaximum)
            } else {
                None
            };
            let limits = PlanningLimits::standard();
            let plan = observe::migration_plan(&config, &cache, db, limits, decision)?;
            println!("{}", plan.to_json(limits.max_output_bytes)?);
            Ok(())
        }
        ConfigCommand::Effective {
            format: ViewFormat::Json,
        } => {
            let cwd = std::env::current_dir()?;
            let bootstrap = super::capture_bootstrap(false)?;
            let view = crate::config::view::load_with_bootstrap(&bootstrap, Some(&cwd))?;
            println!("{}", serde_json::to_string_pretty(&view)?);
            Ok(())
        }
    }
}

fn live_plan(cache: PathBuf, root: PathBuf, check: bool) -> anyhow::Result<()> {
    let result = super::with_transport(move |transport, settings| {
        let mut client = Client::try_connect_with_settings(&transport, &cache, settings)
            .context(CheckStatus::Unavailable)?
            .ok_or(CheckStatus::DaemonUnavailable)?;
        let response = client
            .call(Request::InspectProjectConfig {
                protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(),
                path: root,
            })
            .context(CheckStatus::Unavailable)?;
        live_reply(response)
    })?;
    let plan = result
        .get("plan")
        .filter(|value| value["basis"] == "live")
        .ok_or_else(|| anyhow::anyhow!("missing live config plan"))?;
    let status = match result["check_status"].as_u64() {
        Some(0) => Ok(()),
        Some(3) => Err(CheckStatus::Required),
        Some(5) => Err(CheckStatus::Unavailable),
        _ => anyhow::bail!("invalid live config plan status"),
    };
    println!("{}", serde_json::to_string(plan)?);
    if check {
        status?;
    }
    Ok(())
}

fn live_apply(
    cache: PathBuf,
    root: PathBuf,
    plan_id: &str,
    allow_remote: bool,
    started: Instant,
) -> anyhow::Result<()> {
    let result = super::with_transport(move |transport, settings| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build live configuration apply runtime")?;
        runtime.block_on(async {
            let timeout = settings.ipc_io_timeout();
            let mut client =
                tokio::time::timeout(timeout, AsyncClient::connect(&transport, &cache))
                    .await
                    .map_err(|_| CheckStatus::DaemonUnavailable)?
                    .map_err(|_| CheckStatus::DaemonUnavailable)?;
            tokio::time::timeout(timeout, client.handshake_registry())
                .await
                .context(CheckStatus::Unavailable)?
                .context(CheckStatus::Unavailable)?;
            let response = tokio::time::timeout(
                timeout,
                client.call(Request::ApplyProjectConfig {
                    path: root,
                    plan_id: plan_id.to_owned(),
                    allow_remote,
                    follow_job: true,
                }),
            )
            .await
            .context(CheckStatus::Unavailable)?
            .context(CheckStatus::Unavailable)?;
            let result = live_reply(response)?;
            match result["apply_status"].as_u64() {
                Some(0) => complete_live_apply(&mut client, plan_id, started, result).await,
                Some(4) => Err(CheckStatus::Stale.into()),
                Some(5) => Err(CheckStatus::Unavailable.into()),
                _ => anyhow::bail!("invalid live config apply status"),
            }
        })
    })?;
    println!("{result}");
    Ok(())
}

async fn complete_live_apply(
    client: &mut AsyncClient,
    plan_id: &str,
    started: Instant,
    mut result: Value,
) -> anyhow::Result<Value> {
    use crate::{hybrid::work::Cancellation, search::operation::SearchOperation};

    anyhow::ensure!(
        result["plan_id"] == plan_id && result["applied"].is_boolean(),
        "invalid live config apply reply"
    );
    let Some(job) = result.get_mut("job").map(Value::take) else {
        anyhow::ensure!(
            result.get("timeout_ms").is_none() && result.get("poll_ms").is_none(),
            "live config apply reply missing job"
        );
        return Ok(result);
    };
    anyhow::ensure!(
        result["applied"] == false,
        "invalid live config apply job acknowledgement"
    );
    let job_id = job["job_id"]
        .as_i64()
        .filter(|&id| id > 0)
        .context("invalid live config apply job ID")?;
    let project_id = job["project_id"]
        .as_i64()
        .filter(|&id| id > 0)
        .context("invalid live config apply project ID")?;
    let timeout = Duration::from_millis(
        result["timeout_ms"]
            .as_u64()
            .filter(|&ms| ms > 0)
            .context("invalid live config apply timeout")?,
    );
    let poll = Duration::from_millis(
        result["poll_ms"]
            .as_u64()
            .filter(|&ms| ms > 0)
            .context("invalid live config apply poll interval")?,
    );
    let operation = SearchOperation::with_timeout(started, timeout, Cancellation::new())
        .with_context(|| format!("wait for live configuration apply job {job_id}"))?;
    let finished = within_operation(
        &operation,
        super::sync_command::await_job(client, &operation, poll, job),
    )
    .await
    .with_context(|| format!("wait for live configuration apply job {job_id}"))?;
    anyhow::ensure!(
        finished["job_id"] == job_id && finished["project_id"] == project_id,
        "unexpected identity for live configuration apply job {job_id}"
    );
    match finished["state"].as_str() {
        Some("done") => {}
        Some("error") => {
            let error = finished["stats"]["error"]
                .as_str()
                .unwrap_or("unknown error");
            anyhow::bail!("live configuration apply job {job_id} failed ({error})");
        }
        _ => anyhow::bail!("unexpected state for live configuration apply job {job_id}"),
    }
    let stats: crate::index::job::JobStats = serde_json::from_value(finished["stats"].clone())
        .context("invalid live config apply job statistics")?;
    Ok(
        json!({"apply_status": 0, "applied": true, "plan_id": plan_id, "project_id": project_id, "stats": stats}),
    )
}

fn live_reply(response: Response) -> anyhow::Result<Value> {
    match response {
        Response::ToolResult { value } => Ok(value),
        Response::Error {
            code,
            message,
            instruction,
        } => Err(anyhow::Error::from(crate::error::Error::from_mcp_code(
            code,
            message,
            instruction,
        ))
        .context(CheckStatus::Unavailable)),
        Response::Hello { .. } => anyhow::bail!("unexpected live configuration reply"),
    }
}

struct ProjectPaths {
    config: PathBuf,
    cache: PathBuf,
    root: PathBuf,
}

impl ProjectPaths {
    fn resolve(path: Option<&Path>) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        let config = cwd.join(crate::config::paths::config_dir()?);
        let cache = cwd.join(crate::config::paths::cache_dir()?);
        let path = path.map_or_else(|| cwd.clone(), |path| cwd.join(path));
        let root = crate::daemon::registry::project_root_for(&path, &cache)?;
        Ok(Self {
            config,
            cache,
            root,
        })
    }
}
