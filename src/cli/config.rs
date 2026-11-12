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
}
/// Formats supported by the versioned effective view.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ViewFormat {
    /// Machine-readable versioned JSON.
    Json,
}

pub(super) fn run(command: &ConfigCommand) -> anyhow::Result<()> {
    match command {
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
