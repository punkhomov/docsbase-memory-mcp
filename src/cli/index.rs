//! `docsbase index` — direct indexing with the per-project writer lease
//! (FR-11, FR-30; I5).

use std::path::Path;

use crate::config::Config;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::ipc::async_client::{AsyncClient, within_operation};
use crate::store::Db;
use crate::vector_cache::{VectorCacheSource, VectorStore};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Runs `docsbase index [path]`, printing `{"project_id":…,"stats":…}`.
///
/// # Errors
/// Returns an error when the path/config/database cannot be opened, another
/// writer holds the project lease (I5), or indexing fails.
pub fn run(path: Option<&Path>) -> anyhow::Result<()> {
    run_with_settings(path, &crate::store::settings::standalone()?)
}

pub(crate) fn run_with_settings(
    path: Option<&Path>,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let global = Config::global()?;
    let path = path.map(Path::to_path_buf);
    let settings = settings.clone();
    let payload = super::command::run(global, move |context| {
        run_in(context, path, settings, started)
    })?;
    println!("{payload}");
    Ok(())
}

pub(super) async fn run_in(
    context: super::command::Context,
    path: Option<std::path::PathBuf>,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    started: Instant,
) -> Result<serde_json::Value> {
    let operation = crate::search::operation::SearchOperation::with_timeout(
        started,
        Duration::from_millis(context.global.remote.background_timeout_ms),
        crate::hybrid::work::Cancellation::new(),
    )?;
    let mut cancelled =
        crate::daemon::request::CancelOnDrop(Some(operation.cancellation().clone()));
    within_operation(&operation, async {
        if let Ok(mut client) = AsyncClient::connect(&context.transport, &context.cache).await {
            if let Err(error) = client.handshake_registry().await {
                // A legacy direct index can proceed when there is no negotiated
                // daemon. A verified version mismatch never enters that path.
                return if context.global.vector_store.is_none() { Ok(()) } else { Err(error) };
            }
            context.admit_daemon(&client)?;
            if context.global.vector_store.is_some() {
                return Err(Error::Admission {
                    message: "the daemon owns the shared vector store; use `docsbase sync` or stop the daemon".into(),
                });
            }
        }
        Ok(())
    }).await?;
    let activity = context.activity.track();
    let result = run_with_operation(context, path, settings, operation, activity).await;
    if result.is_ok() {
        cancelled.0 = None;
    }
    result
}

pub(super) async fn run_with_operation(
    context: super::command::Context,
    path: Option<std::path::PathBuf>,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    operation: crate::search::operation::SearchOperation,
    activity: Arc<crate::daemon::request::Activity>,
) -> Result<serde_json::Value> {
    let owner = context.clone();
    let captured = context
        .control(&operation, &activity, move || {
            let cwd = std::env::current_dir()
                .map_err(|e| Error::internal_with_source("resolve current directory", e))?;
            let target = path.unwrap_or(cwd);
            let target = target.canonicalize().map_err(|e| Error::Project {
                message: format!(
                    "{} does not exist or cannot be resolved: {e}",
                    target.display()
                ),
                instruction: Some(
                    "re-create the worktree/directory or run from a valid project root".into(),
                ),
            })?;
            let (project, config, source) = if let Some(policy) = &owner.global.vector_store {
                let root = registry::project_root_for(&target, &owner.cache)?;
                let config = Config::for_project(&owner.global, &root)?;
                let store = VectorStore::reserve(&owner.cache, policy)?;
                let mut db = Db::open_with_settings(&owner.cache, &settings)?;
                (
                    registry::ensure_project(&mut db, &root)?,
                    config,
                    VectorCacheSource::Owned(store),
                )
            } else {
                let mut db = Db::open_with_settings(&owner.cache, &settings)?;
                let project = registry::ensure_project(&mut db, &target)?;
                let config = Config::for_project(&owner.global, &project.canonical_root)?;
                let source = VectorCacheSource::Legacy(config.hybrid_cfg.cache.limits());
                (project, config, source)
            };
            let config = Arc::new(config);
            let stages = Arc::new(owner.stages(&config)?);
            Ok(crate::index::async_job::JobContext {
                input: crate::index::async_job::JobInput::Full,
                singleton: None,
                cache: owner.cache,
                db_settings: settings,
                project,
                config,
                stages,
                source,
                revision: None,
                dense: None,
            })
        })
        .await?;
    let id = captured.project.id;
    let result = crate::index::async_job::run(
        &context.jobs,
        &context.workers,
        activity,
        operation,
        &context.budget,
        captured,
    )
    .await;
    Ok(serde_json::json!({"project_id":id,"stats":result?}))
}
