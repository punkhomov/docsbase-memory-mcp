//! Configuration inspection and explicit application of a reviewed plan.
use clap::{Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

mod apply;

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
    /// Apply a reviewed configuration plan to an existing stopped v5 project.
    Apply {
        /// Project directory. Defaults to the current directory.
        path: Option<PathBuf>,
        /// Exact ID printed by `config plan`.
        #[arg(long)]
        plan_id: String,
        /// Permit remote embedding requests that may incur costs.
        #[arg(long)]
        allow_remote: bool,
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
}

impl CheckStatus {
    pub(super) const fn exit_code(&self) -> u8 {
        match self {
            Self::Required => 3,
            Self::Stale => 4,
            Self::Unavailable | Self::MissingGrants | Self::DaemonUnavailable => 5,
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
        } => apply::run(
            ProjectPaths::resolve(path.as_deref())?,
            plan_id,
            *allow_remote,
            db,
        ),
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
            let directory = crate::config::paths::config_dir()?;
            let view = crate::config::view::load_from(&directory, Some(&cwd))?;
            println!("{}", serde_json::to_string_pretty(&view)?);
            Ok(())
        }
    }
}

fn live_plan(cache: PathBuf, root: PathBuf, check: bool) -> anyhow::Result<()> {
    use crate::ipc::{
        client::Client,
        protocol::{self, Request, Response},
    };
    super::with_transport(move |transport| {
        let mut client = Client::try_connect(&transport, &cache)
            .map_err(|error| anyhow::Error::from(error).context(CheckStatus::Unavailable))?
            .ok_or(CheckStatus::DaemonUnavailable)?;
        let response = client
            .call(Request::InspectProjectConfig {
                protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(),
                path: root,
            })
            .map_err(|error| anyhow::Error::from(error).context(CheckStatus::Unavailable))?;
        let result = match response {
            Response::ToolResult { value } => value,
            Response::Error {
                code,
                message,
                instruction,
            } => {
                return Err(anyhow::Error::from(crate::error::Error::from_mcp_code(
                    code,
                    message,
                    instruction,
                ))
                .context(CheckStatus::Unavailable));
            }
            Response::Hello { .. } => anyhow::bail!("unexpected live config plan reply"),
        };
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
    })
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
