//! CLI entry points (FR-30).

pub mod index;
pub mod search;
pub mod status;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::config::paths;
use crate::ipc::client::Client;

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
    /// Run the shared daemon in the foreground (hidden `--detached` for autostart).
    Serve {
        /// Marker flag used by autostart; detaching is done by the parent.
        #[arg(long, hide = true)]
        detached: bool,
        /// Grace period before the last session's exit stops the daemon.
        #[arg(long, hide = true, default_value_t = crate::daemon::lifecycle::DEFAULT_GRACE_MS)]
        grace_ms: u64,
    },
    /// Daemon control commands.
    Daemon {
        /// Daemon subcommand.
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

/// Daemon control subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Stop the running daemon.
    Stop,
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
        Command::Serve { detached, grace_ms } => serve(detached, grace_ms),
        Command::Daemon { command } => match command {
            DaemonCommand::Stop => daemon_stop(),
        },
    }
}

fn serve(_detached: bool, grace_ms: u64) -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    crate::daemon::lifecycle::run_daemon(&cache, std::time::Duration::from_millis(grace_ms))?;
    Ok(())
}

fn daemon_stop() -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    crate::daemon::lifecycle::stop_daemon(&cache)?;
    println!("daemon stopped");
    Ok(())
}

/// Routes `tool` through the live daemon; `Ok(None)` means no daemon is
/// reachable and the caller should fall back to direct mode (FR-30).
///
/// `bind_session` registers the cwd-bound session first (I6); registry-wide
/// tools such as `status` skip it so they work from any directory.
///
/// # Errors
/// Surfaces handshake/tool errors from a daemon that did answer.
pub(crate) fn try_daemon(
    tool: &str,
    args: serde_json::Value,
    bind_session: bool,
) -> anyhow::Result<Option<serde_json::Value>> {
    let cache = paths::cache_dir()?;
    let Some(mut client) = Client::connect(&cache) else {
        return Ok(None);
    };
    if bind_session {
        let cwd = std::env::current_dir().context("resolve current directory")?;
        client.handshake(&cwd)?;
    } else {
        client.handshake_registry()?;
    }
    Ok(Some(client.call_tool(tool, args)?))
}
