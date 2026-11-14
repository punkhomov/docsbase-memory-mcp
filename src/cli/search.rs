//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use std::future::Future;
use std::path::Path;

use serde_json::Value;

use crate::cli::status::read_project_with_settings;
use crate::config::Config;
use crate::config::paths;
use crate::daemon::tools;
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::index::QueryMode;
use crate::ipc::async_client::{AsyncClient, within_operation};
use crate::search::operation::SearchOperation;

/// Runs `docsbase search <query> [--limit N] [--explain] [--snippet] [--dedupe …]`,
/// printing a JSON array
/// of `{path, heading_path, lines, score}` (FR-20) or, with `--explain`, the
/// `{hits, meta}` envelope (spec `search-telemetry`, FR-1). Response stages use
/// one bounded owned SQL payload; incomplete index updates require rebuilding.
///
/// # Errors
/// Returns an error when the project is unregistered/not indexed, the snapshot
/// cannot be opened, or the query is invalid.
pub struct Options {
    /// Maximum number of hits.
    pub limit: usize,
    /// Return `{hits, meta}` instead of the bare array.
    pub explain: bool,
    /// Attach a text fragment to each hit.
    pub snippet: bool,
    /// `none` or `path`.
    pub dedupe: String,
    /// Chunks kept per document (dedupe only).
    pub n_per_doc: Option<u64>,
    /// Project-relative path prefix.
    pub path_prefix: Option<String>,
    /// Required frontmatter tags (AND).
    pub tags: Vec<String>,
}

impl Options {
    /// The MCP argument object, so both seams send exactly the same request.
    fn args(&self, query: &str, mode: QueryMode) -> serde_json::Value {
        let mut args = serde_json::json!({
            "query": query,
            "query_mode": mode,
            "limit": self.limit,
            "explain": self.explain,
            "snippet": self.snippet,
            "dedupe": self.dedupe,
        });
        if let Some(n) = self.n_per_doc {
            args["n_per_doc"] = serde_json::json!(n);
        }
        if let Some(prefix) = &self.path_prefix {
            args["path_prefix"] = serde_json::json!(prefix);
        }
        if !self.tags.is_empty() {
            args["tags"] = serde_json::json!(self.tags);
        }
        args
    }
}

/// Runs `docsbase search <query> [--limit N] [--explain] [--snippet] [--dedupe …]`,
/// printing a JSON array of `{path, heading_path, lines, score}` (FR-20) or, with
/// `--explain`, the `{hits, meta}` envelope (spec `search-telemetry`, FR-1).
///
/// # Errors
/// Returns an error for invalid configuration/query, admission or connected
/// transport failure, an unregistered/unindexed project, an unavailable snapshot,
/// or synchronous invocation from a current-thread Tokio runtime.
pub fn run(query: &str, options: &Options) -> anyhow::Result<()> {
    run_with_settings(query, options, &crate::store::settings::standalone()?)
}

/// Runs search with explicit query interpretation using the original request budget.
///
/// # Errors
/// As [`run`], including refusal when a connected daemon lacks literal support.
pub fn run_with_mode(query: &str, options: &Options, mode: QueryMode) -> anyhow::Result<()> {
    run_with_settings_and_mode(query, options, &crate::store::settings::standalone()?, mode)
}

pub(crate) fn run_with_settings(
    query: &str,
    options: &Options,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    run_with_settings_and_mode(query, options, settings, QueryMode::Syntax)
}

pub(crate) fn run_with_settings_and_mode(
    query: &str,
    options: &Options,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    mode: QueryMode,
) -> anyhow::Result<()> {
    run_with_policies(
        query,
        options,
        settings,
        mode,
        crate::hybrid::dense_settings::standalone()?.settings(),
    )
}

pub(crate) fn run_with_policies(
    query: &str,
    options: &Options,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    mode: QueryMode,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
) -> anyhow::Result<()> {
    run_with_policies_started(
        query,
        options,
        settings,
        mode,
        dense_settings,
        std::time::Instant::now(),
    )
}

pub(crate) fn run_with_policies_started(
    query: &str,
    options: &Options,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    mode: QueryMode,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    dense_settings.query_cache_settings().validate()?;
    let global = Config::global()?;
    let operation = SearchOperation::new(&global.search, started, Cancellation::new())?;
    let cache = paths::cache_dir()?;
    let cwd = std::env::current_dir()
        .map_err(|error| Error::internal_with_source("resolve current directory", error))?;
    let args = options.args(query, mode);
    if let Some(value) = try_daemon_search(&cache, &cwd, args.clone(), &operation, settings)? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if matches!(
        global.daemon.profile,
        crate::daemon::settings::DaemonProfile::Website { .. }
    ) {
        return Err(crate::daemon::launch::website_unavailable().into());
    }
    operation.check()?;
    let (db, project) = read_project_with_settings(settings)?;
    // Direct mode honors the project's hybrid configuration exactly like the
    // daemon route: stages resolved up front (explicit admission on failure,
    // FR-14) and the same explicit search context (FR-11/12).
    let config = Config::for_project(&global, &project.canonical_root)?;
    operation.check()?;
    let stages = crate::hybrid::from_config(&config.hybrid_cfg, &config.providers)?;
    operation.check()?;
    let window = config
        .hybrid_cfg
        .reranker
        .as_ref()
        .map_or(crate::hybrid::config::DEFAULT_RERANK_WINDOW, |reranker| {
            reranker.window
        });
    // Direct mode loads a persisted ANN index if one exists and never builds
    // one (FR-8, FR-5): a one-shot process must not pay for HNSW build
    // latency.
    let dense = direct_dense_cache(&cache, config.hybrid_cfg.cache.max_bytes, dense_settings)?;
    let budget = crate::search::budget::PayloadBudget::new(config.search.clone())?;
    let value = tools::search_docs_in(
        &tools::SearchContext {
            stages: &stages,
            dense: &dense,
            cache: &cache,
            project: &project,
            window,
            payload_budget: &budget,
            operation,
        },
        &db,
        // Every search argument, including explain, reaches the same pipeline.
        &args,
    )?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Only an initial failed connect permits direct mode; connected failures propagate.
fn try_daemon_search(
    cache: &Path,
    cwd: &Path,
    args: Value,
    operation: &SearchOperation,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> Result<Option<Value>> {
    block_on_io(within_operation(operation, async {
        let Ok(mut client) = AsyncClient::connect(cache).await else {
            operation.check()?;
            return Ok(None);
        };
        client.handshake_registry().await?;
        if client
            .capabilities()
            .iter()
            .any(|capability| capability == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY)
        {
            let project = crate::cli::existing_website_project(cache, cwd, settings)?;
            operation.check()?;
            client.bind_read_only(Some(project.id)).await?;
        } else {
            client.register_session(cwd).await?;
        }
        client.search(args, operation).await.map(Some)
    }))
}

fn block_on_io<T>(work: impl Future<Output = Result<T>>) -> Result<T> {
    if crate::hybrid::blocking::in_context()
        && let Some(bridge) = crate::hybrid::blocking::bridge()
    {
        return bridge.block_on(work);
    }
    let run = || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| Error::internal_with_source("create CLI IPC runtime", error))?;
        runtime.block_on(work)
    };
    match tokio::runtime::Handle::try_current() {
        Err(_) => run(),
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
            tokio::task::block_in_place(run),
        Ok(_) => Err(Error::Admission {
            message: "synchronous CLI search cannot run inside a current-thread Tokio runtime; use AsyncClient".to_owned(),
        }),
    }
}

fn direct_dense_cache(
    cache: &Path,
    vector_cache_max_bytes: u64,
    dense_settings: &crate::hybrid::dense_settings::DenseSettings,
) -> Result<crate::hybrid::dense::DenseCache> {
    // v1 project/global vector-store policy retains precedence over this fallback.
    let mut effective = dense_settings.clone();
    effective.vector_cache_max_bytes = vector_cache_max_bytes;
    crate::hybrid::dense::DenseCache::with_settings(
        crate::hybrid::dense::AnnPolicy::LoadOnly,
        cache,
        &effective,
    )
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
