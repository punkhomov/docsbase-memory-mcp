//! Read-only configuration views; no daemon or provider construction.
use clap::{Subcommand, ValueEnum};

/// Optional commands under the legacy `config` command.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show current source values with units, scopes, origins and build capabilities.
    Effective {
        /// Versioned output format.
        #[arg(long, value_enum, default_value = "json")]
        format: ViewFormat,
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

pub(super) fn run(
    command: &ConfigCommand,
    db: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    match command {
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
