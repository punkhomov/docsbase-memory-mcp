//! `docsbase search` — read-only snapshot search (FR-20, FR-30).

use std::path::Path;

use serde_json::Value;

use crate::cli::status::read_project_for_search;
use crate::config::Config;
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
    run_with_mode(query, options, QueryMode::Syntax)
}

/// Runs search with explicit query interpretation using the original request budget.
///
/// # Errors
/// As [`run`], including refusal when a connected daemon lacks literal support.
pub fn run_with_mode(query: &str, options: &Options, mode: QueryMode) -> anyhow::Result<()> {
    run_started(query, options, None, mode, std::time::Instant::now())
}

#[cfg(test)]
pub(crate) fn run_with_settings(
    query: &str,
    options: &Options,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    run_started(
        query,
        options,
        Some(settings),
        QueryMode::Syntax,
        std::time::Instant::now(),
    )
}

pub(crate) fn run_started(
    query: &str,
    options: &Options,
    supplied_db: Option<&crate::config::schema::Resolved<crate::store::settings::DbSettings>>,
    mode: QueryMode,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    use crate::store::settings;
    let config = Config::global_resolved()?;
    let (settings, pool) = match supplied_db {
        Some(db) => (db.clone(), settings::standalone_read_pool()?),
        None => (
            settings::db_from_config(&config)?,
            settings::pool_from_config(&config)?,
        ),
    };
    let global = config.into_settings();
    let args = options.args(query, mode);
    let value = super::command::run(global, move |context| {
        run_in(context, args, settings, pool, started)
    })?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

async fn run_in(
    context: super::command::Context,
    args: Value,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pool: crate::config::schema::Resolved<crate::store::settings::ReadPoolSettings>,
    started: std::time::Instant,
) -> Result<Value> {
    let operation = SearchOperation::new(&context.global.search, started, Cancellation::new())?;
    let mut cancelled =
        crate::daemon::request::CancelOnDrop(Some(operation.cancellation().clone()));
    let activity = context.activity.track();
    let cwd = context
        .control(&operation, &activity, || {
            std::env::current_dir()
                .map_err(|e| Error::internal_with_source("resolve current directory", e))
        })
        .await?;
    // Only failure to make the initial connection admits direct fallback.
    if let Some(value) = daemon_in(
        &context,
        &cwd,
        args.clone(),
        &operation,
        &settings,
        &activity,
    )
    .await?
    {
        cancelled.0 = None;
        return Ok(value);
    }
    if matches!(
        context.global.daemon.profile,
        crate::daemon::settings::DaemonProfile::Website { .. }
    ) {
        return Err(crate::daemon::launch::website_unavailable());
    }
    let owner = context.clone();
    let original = operation.clone();
    let captured = context
        .control(&operation, &activity, move || {
            let (db, project) = read_project_for_search(&settings)?;
            let config =
                std::sync::Arc::new(Config::for_project(&owner.global, &project.canonical_root)?);
            if db.has_generation_catalog()
                && config.hybrid_cfg.ranking == crate::hybrid::config::Ranking::Rrf
                && let Some(model) = config.hybrid_cfg.embedding_model_key()
            {
                use crate::store::project_index::{require_catalog_embedding, with_catalog_pair};
                with_catalog_pair(&db, &project, |_, local, _| {
                    require_catalog_embedding(local.as_ref(), &model)
                })?;
            }
            let stages = std::sync::Arc::new(owner.stages(&config)?);
            let readers =
                std::sync::Arc::new(crate::store::read_pool::ReadPool::prepare_for_search(
                    &owner.cache,
                    &settings,
                    &pool,
                )?);
            let dense = std::sync::Arc::new(crate::hybrid::dense::DenseCache::for_config(
                crate::hybrid::dense::AnnPolicy::LoadOnly,
                &owner.cache,
                &owner.global.dense,
                &config,
            )?);
            drop(db); // Native readonly connection never crosses an await.
            let owners = crate::daemon::server::search_async::SearchOwners {
                cache: owner.cache,
                readers,
                dense,
                workers: owner.workers,
                payload_budget: owner.budget,
                policy: owner.global.search.clone(),
                revision: None,
                _daemon: None,
            };
            let captured = crate::daemon::server::search_async::SearchSession::direct(
                owners, project, config, stages, original,
            );
            let mut retained = owner
                .search
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if retained.is_some() {
                return Err(Error::internal("direct command search already prepared"));
            }
            *retained = Some(captured.clone());
            Ok(captured)
        })
        .await?;
    let result = crate::daemon::server::search_async::run_direct(captured, args, activity).await;
    if result.is_ok() {
        cancelled.0 = None;
    }
    result
}

async fn daemon_in(
    context: &super::command::Context,
    cwd: &Path,
    args: Value,
    operation: &SearchOperation,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    activity: &std::sync::Arc<crate::daemon::request::Activity>,
) -> Result<Option<Value>> {
    within_operation(operation, async {
        let Ok(mut client) = AsyncClient::connect(&context.transport, &context.cache).await else {
            operation.check()?;
            return Ok(None);
        };
        client.handshake_registry().await?;
        context.admit_daemon(&client)?;
        if client
            .capabilities()
            .iter()
            .any(|c| c == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY)
        {
            let cache = context.cache.clone();
            let cwd = cwd.to_path_buf();
            let settings = settings.clone();
            let project = context
                .control(operation, activity, move || {
                    crate::cli::existing_website_project(&cache, &cwd, &settings)
                })
                .await?;
            operation.check()?;
            client.bind_read_only(Some(project.id)).await?;
        } else {
            client.register_session(cwd).await?;
        }
        client.search(args, operation).await.map(Some)
    })
    .await
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
