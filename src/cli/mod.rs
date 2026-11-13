//! CLI entry points (FR-30).

/// Shared test seam for CLI unit-test modules: environment mutation
/// (`DOCSBASE_CONFIG_DIR`, `OPENROUTER_API_KEY`, …) must be serialized
/// across all modules that read it concurrently, or parallel `cargo test`
/// runs race each other. Keep this one lock for every env-touching test.
#[cfg(test)]
pub(crate) mod test_env {
    /// Single env lock for the whole test binary: the `hybrid-local` build
    /// shares the runtime guard (which also neutralizes a host
    /// `ORT_DYLIB_PATH`); the default build has no ORT env surface, so a
    /// plain mutex is enough (round-7 C2).
    #[cfg(feature = "hybrid-local")]
    pub(crate) use crate::hybrid::runtime::test_env::{EnvGuard, env_guard};

    #[cfg(not(feature = "hybrid-local"))]
    pub(crate) type EnvGuard = std::sync::MutexGuard<'static, ()>;

    #[cfg(not(feature = "hybrid-local"))]
    pub(crate) fn env_guard() -> EnvGuard {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;

pub mod config;
pub mod connect;
pub mod index;
pub mod install;
pub mod models;
pub mod search;
pub mod status;
pub mod update;

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
    /// Open the local documentation panel (loopback only).
    #[cfg(feature = "web")]
    Web {
        /// Open the launch URL in your default browser.
        #[arg(long)]
        open: bool,
        /// Local port (0 selects an available port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Register and index a project (default: current directory).
    Index {
        /// Project directory.
        path: Option<PathBuf>,
    },
    /// Search indexed chunks of the current project.
    Search {
        /// Query string.
        query: String,
        /// Interpret query syntax or literal natural-language text (syntax|literal).
        #[arg(long, default_value_t = crate::index::QueryMode::default())]
        query_mode: crate::index::QueryMode,
        /// Maximum number of hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Attach telemetry: `{hits, meta}` instead of a bare array.
        #[arg(long)]
        explain: bool,
        /// Attach a text fragment to each hit.
        #[arg(long)]
        snippet: bool,
        /// Post-filter: `none` (default) or `path` (cap chunks per document).
        #[arg(long, default_value = "none")]
        dedupe: String,
        /// Chunks kept per document when `--dedupe path` (default 2, max 10).
        #[arg(long)]
        n_per_doc: Option<u64>,
        /// Only documents whose project-relative path starts with this prefix.
        #[arg(long)]
        path_prefix: Option<String>,
        /// Require this frontmatter tag (repeatable, AND).
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// List indexed documents of the current project.
    List,
    /// Synchronize (re-index) the current project.
    Sync {
        /// Project directory (defaults to the current directory).
        path: Option<PathBuf>,
    },
    /// Print the effective configuration for the current directory.
    Config {
        /// Optional versioned view; omission preserves the legacy JSON.
        #[command(subcommand)]
        command: Option<config::ConfigCommand>,
    },
    /// Show registry-wide status.
    Status,
    /// Run the shared daemon in the foreground (hidden `--detached` for autostart).
    Serve {
        /// Complete daemon profile selector (shared|website).
        #[arg(long, value_enum)]
        profile: Option<ServeProfile>,
        /// Existing indexed absolute root; selects a complete Website profile.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Marker flag used by autostart; detaching is done by the parent.
        #[arg(long, hide = true)]
        detached: bool,
        /// Grace period before the last session's exit stops the daemon.
        #[arg(long, hide = true)]
        grace_ms: Option<u64>,
    },
    /// Run the stdio MCP server for one agent.
    Mcp,
    /// Daemon control commands.
    Daemon {
        /// Daemon subcommand.
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Model management commands.
    Models {
        /// Model subcommand.
        #[command(subcommand)]
        command: models::ModelsCommand,
    },
    /// Prepare providers on this machine (status, or `connect <provider>`).
    Connect {
        /// Provider to prepare; omit for the status table.
        provider: Option<String>,
        /// Report the state without writing anything.
        #[arg(long)]
        check: bool,
        /// Re-probe / reinstall even when already prepared.
        #[arg(long)]
        force: bool,
    },
    /// Install (or update) the binary and register owned artifacts.
    Install,
    /// Update the installed binary from a GitHub release (network).
    Update {
        /// Only check whether a newer release exists.
        #[arg(long, conflicts_with_all = ["from", "version"])]
        check: bool,
        /// Reinstall even when the running version already matches the target.
        #[arg(long)]
        force: bool,
        /// Release tag to install instead of the latest (e.g. v0.1.0-alpha.5).
        #[arg(long, value_name = "TAG", conflicts_with = "from")]
        version: Option<String>,
        /// Install a local binary, skipping download and verification.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,
    },
    /// Remove owned artifacts and, with `--yes`, the indexes.
    Uninstall {
        /// Delete without an interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Print everything that would be removed, delete nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Complete CLI profile selection; a Website selector always supplies its own root.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ServeProfile {
    Shared,
    Website,
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
    let db_settings = crate::store::settings::standalone()?;
    let pool_settings = crate::store::settings::standalone_read_pool()?;
    let dense_settings = crate::hybrid::dense_settings::standalone()?;
    match cli.command {
        #[cfg(feature = "web")]
        Command::Web { open, port } => crate::web::run(port, open),
        Command::Index { path } => index::run_with_settings(path.as_deref(), &db_settings),
        Command::Search {
            query,
            query_mode,
            limit,
            explain,
            snippet,
            dedupe,
            n_per_doc,
            path_prefix,
            tags,
        } => search::run_with_policies(
            &query,
            &search::Options {
                limit,
                explain,
                snippet,
                dedupe,
                n_per_doc,
                path_prefix,
                tags,
            },
            &db_settings,
            query_mode,
            dense_settings.settings(),
        ),
        Command::List => status::list_with_settings(&db_settings),
        Command::Sync { path } => sync(path.as_deref(), &db_settings),
        Command::Config { command: None } => show_config(),
        Command::Config {
            command: Some(command),
        } => config::run(&command, &db_settings),
        Command::Status => status::status_with_settings(&db_settings),
        Command::Serve {
            detached,
            grace_ms,
            profile,
            project,
        } => serve(
            detached,
            grace_ms,
            profile,
            project,
            &db_settings,
            &pool_settings,
            dense_settings.settings(),
        ),
        Command::Mcp => crate::mcp::frontend::run_blocking(),
        Command::Daemon { command } => match command {
            DaemonCommand::Stop => daemon_stop(),
        },
        Command::Models { command } => match command {
            models::ModelsCommand::Use(args) => models::run(&args),
            #[cfg(feature = "hybrid-local")]
            models::ModelsCommand::Fetch(args) => models::fetch_cmd(&args),
            #[cfg(feature = "hybrid-local")]
            models::ModelsCommand::List => models::list_cmd(),
            #[cfg(feature = "hybrid-local")]
            models::ModelsCommand::Remove(args) => models::remove_cmd(&args),
        },
        Command::Connect {
            provider,
            check,
            force,
        } => connect::run(&connect::ConnectArgs {
            provider,
            check,
            force,
        }),
        Command::Install => install::install(),
        Command::Update {
            check,
            force,
            version,
            from,
        } => update::run(&update::Options {
            check,
            force,
            version,
            from,
        }),
        Command::Uninstall { yes, dry_run } => {
            install::uninstall_with_settings(yes, dry_run, &db_settings)
        }
    }
}

fn serve(
    detached: bool,
    grace_ms: Option<u64>,
    profile: Option<ServeProfile>,
    project: Option<PathBuf>,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool_settings: &crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
) -> anyhow::Result<()> {
    let selected_profile = match (profile, project) {
        (None, None) => None,
        (Some(ServeProfile::Shared), None) => Some(crate::daemon::settings::DaemonProfile::Shared),
        (None | Some(ServeProfile::Website), Some(project_root)) => Some(crate::daemon::settings::DaemonProfile::Website { project_root }),
        _ => return Err(Error::Admission { message: "serve --profile website requires its own --project ABS; --profile shared cannot have --project".to_owned() }.into()),
    };
    let cache = paths::cache_dir()?;
    crate::daemon::lifecycle::run_daemon_with_legacy_launch(
        &cache,
        settings,
        pool_settings,
        dense_settings,
        &crate::daemon::launch::LaunchSnapshot::capture(),
        crate::daemon::settings::DaemonPatch {
            profile: selected_profile,
            idle_grace_ms: grace_ms,
            ..crate::daemon::settings::DaemonPatch::default()
        },
        detached,
    )?;
    Ok(())
}

/// Runs `docsbase sync`: a daemon sync job when one is reachable (FR-17),
/// otherwise a direct full index (FR-30).
///
/// The sync job is started and polled over one bound connection: an idle
/// daemon arms its grace deadline when no session is live, so a long job
/// would otherwise outlive the accept loop mid-poll (final review N1).
fn sync(
    path: Option<&std::path::Path>,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let target = cwd.join(path.unwrap_or(std::path::Path::new(".")));
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve sync target {}", target.display()))?;
    let cache = paths::cache_dir()?;
    sync_with_connection(
        &cache,
        &target,
        settings,
        Client::try_connect(&cache),
        || index::run_with_settings(Some(&target), settings),
    )
}

// One captured connection result controls the existing sync route and fallback.
fn sync_with_connection(
    cache: &std::path::Path,
    target: &std::path::Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    connection: crate::error::Result<Option<Client>>,
    direct: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if let Some(mut client) = connection? {
        client.handshake_registry()?;
        let binding = bind_cli_project(&mut client, cache, target, settings);
        match binding {
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
            Err(Error::Project { .. }) if !client.active_website() => {
                return direct();
            }
            Err(err) => return Err(err.into()),
        }
    }
    crate::daemon::launch::require_shared_without_daemon()?;
    direct()
}

/// Runs `docsbase config`: effective values for the current directory
/// (CLI > project > global > defaults, FR-28/FR-29).
fn show_config() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let config = Config::load(Some(&cwd))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&config_payload(&config))?
    );
    Ok(())
}

/// Effective configuration as JSON, including the resolved hybrid stages
/// (T3 acceptance; `docsbase config` and tests share this shape).
#[must_use]
pub fn config_payload(config: &Config) -> serde_json::Value {
    let hybrid_cfg = &config.hybrid_cfg;
    serde_json::json!({
        "ignores": config.ignores,
        "max_file_size": config.max_file_size,
        "max_docs_per_project": config.max_docs_per_project,
        "auto_index": config.auto_index,
        "hybrid": config.hybrid,
        "hybrid_config": {
            "enabled": hybrid_cfg.enabled,
            "ranking": match hybrid_cfg.ranking {
                crate::hybrid::Ranking::Lexical => "lexical",
                crate::hybrid::Ranking::Rrf => "rrf",
            },
            "embedding": hybrid_cfg.embedding.as_ref().map(|embedding| serde_json::json!({
                "provider": embedding.provider,
                "model": embedding.model,
                "dimensions": embedding.dimensions,
                "normalize": embedding.normalize,
            })),
            "reranker": hybrid_cfg.reranker.as_ref().map(|reranker| serde_json::json!({
                "provider": reranker.provider,
                "model": reranker.model,
                "window": reranker.window,
            })),
        },
    })
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
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<Option<serde_json::Value>> {
    let cache = paths::cache_dir()?;
    let Some(mut client) = Client::try_connect(&cache)? else {
        crate::daemon::launch::require_shared_without_daemon()?;
        return Ok(None);
    };
    client.handshake_registry()?;
    if bind_session {
        let cwd = std::env::current_dir().context("resolve current directory")?;
        bind_cli_project(&mut client, &cache, &cwd, settings)?;
    }
    Ok(Some(client.call_tool(tool, args)?))
}

// Live Hello selects routing. The file cannot override an active daemon profile.
fn bind_cli_project(
    client: &mut Client,
    cache: &std::path::Path,
    target: &std::path::Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> crate::error::Result<()> {
    if !client.active_website() {
        return client.register_session(target);
    }
    let project = existing_website_project(cache, target, settings)?;
    match client.call(crate::ipc::protocol::Request::BindReadOnly {
        pid: std::process::id(),
        project_id: Some(project.id),
    })? {
        crate::ipc::protocol::Response::ToolResult { .. } => Ok(()),
        crate::ipc::protocol::Response::Error {
            code,
            message,
            instruction,
        } => Err(Error::from_mcp_code(code, message, instruction)),
        other @ crate::ipc::protocol::Response::Hello { .. } => Err(Error::Protocol {
            message: format!("unexpected binding reply: {other:?}"),
        }),
    }
}

pub(crate) fn existing_website_project(
    cache: &std::path::Path,
    target: &std::path::Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> crate::error::Result<crate::store::models::Project> {
    let result = crate::store::Db::open_readonly_for_admission(cache, settings)
        .and_then(|db| crate::daemon::registry::resolve_by_cwd(&db, target));
    result.map_err(|error| Error::Admission {
        message: format!("website requires an existing fixed project: {error}"),
    })
}
