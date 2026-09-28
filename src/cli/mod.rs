//! CLI entry points (FR-30).

pub mod index;
pub mod search;
pub mod status;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// `docsbase` command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "docsbase",
    version,
    about = "Local-first docs memory MCP server"
)]
pub struct Cli {
    /// Command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Supported top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register and index a project (default: current directory).
    Index {
        /// Project directory.
        path: Option<PathBuf>,
    },
    /// Search indexed chunks of the current project.
    Search {
        /// Query string.
        query: String,
        /// Maximum number of hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List indexed documents of the current project.
    List,
    /// Show registry-wide status.
    Status,
}

/// Parses argv and runs the selected command.
///
/// # Errors
/// Propagates command failures to the `anyhow` CLI boundary.
pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Index { path } => index::run(path.as_deref()),
        Command::Search { query, limit } => search::run(&query, limit),
        Command::List => status::list(),
        Command::Status => status::status(),
    }
}
