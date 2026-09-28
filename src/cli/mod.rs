//! CLI entry points (FR-30).

pub mod index;

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
}

/// Parses argv and runs the selected command.
///
/// # Errors
/// Propagates command failures to the `anyhow` CLI boundary.
pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Index { path } => index::run(path.as_deref()),
    }
}
