//! CLI entry points (FR-30).

pub mod index;
pub mod install;
pub mod search;
pub mod status;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::config::{Config, paths};
use crate::error::Error;
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
    /// Synchronize (re-index) the current project.
    Sync {
        /// Project directory (direct mode only).
        path: Option<PathBuf>,
    },
    /// Print the effective configuration for the current directory.
    Config,
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
    /// Run the stdio MCP server for one agent.
    Mcp,
    /// Daemon control commands.
    Daemon {
        /// Daemon subcommand.
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Install (or update) the binary and register owned artifacts.
    Install,
    /// Remove owned artifacts and, with `--yes`, the indexes.
    Uninstall {
        /// Delete without an interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
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
        Command::Sync { path } => sync(path.as_deref()),
        Command::Config => show_config(),
        Command::Status => status::status(),
        Command::Serve { detached, grace_ms } => serve(detached, grace_ms),
        Command::Mcp => crate::mcp::frontend::run_blocking(),
        Command::Daemon { command } => match command {
            DaemonCommand::Stop => daemon_stop(),
        },
        Command::Install => install::install(),
        Command::Uninstall { yes } => install::uninstall(yes),
    }
}

fn serve(_detached: bool, grace_ms: u64) -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    crate::daemon::lifecycle::run_daemon(&cache, std::time::Duration::from_millis(grace_ms))?;
    Ok(())
}

/// Runs `docsbase sync`: a daemon sync job when one is reachable (FR-17),
/// otherwise a direct full index (FR-30).
///
/// The sync job is started and polled over one bound connection: an idle
/// daemon arms its grace deadline when no session is live, so a long job
/// would otherwise outlive the accept loop mid-poll (final review N1).
fn sync(path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    if let Some(mut client) = Client::connect(&cache) {
        let cwd = std::env::current_dir().context("resolve current directory")?;
        match client.handshake(&cwd) {
            Ok(()) => {
                let job = client.call_tool("sync_start", serde_json::json!({}))?;
                let id = job["job_id"].as_i64().context("sync_start: job_id")?;
                let deadline = Instant::now() + Duration::from_secs(610);
                let mut job = job;
                while matches!(job["state"].as_str(), Some("queued" | "running")) {
                    if Instant::now() >= deadline {
                        anyhow::bail!("sync job {id} did not finish within the budget");
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    job = client.call_tool("sync_status", serde_json::json!({ "job_id": id }))?;
                }
                println!("{}", serde_json::to_string_pretty(&job)?);
                if job["state"] == "error" {
                    anyhow::bail!("sync job {id} failed: {}", job["stats"]);
                }
                return Ok(());
            }
            // Not indexed yet: the direct path registers the project first.
            Err(Error::Project { .. }) => {}
            Err(err) => return Err(err.into()),
        }
    }
    index::run(path)
}

/// Runs `docsbase config`: effective values for the current directory
/// (CLI > project > global > defaults, FR-28/FR-29).
fn show_config() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let config = Config::load(Some(&cwd))?;
    let payload = serde_json::json!({
        "ignores": config.ignores,
        "max_file_size": config.max_file_size,
        "max_docs_per_project": config.max_docs_per_project,
        "auto_index": config.auto_index,
        "hybrid": config.hybrid,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
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
