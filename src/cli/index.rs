//! `docsbase index` — direct indexing with the per-project writer lease
//! (FR-11, FR-30; I5).

use std::path::Path;

use crate::config::Config;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::store::Db;
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
            let mut db = Db::open_with_settings(&owner.cache, &settings)?;
            let project = registry::ensure_project(&mut db, &target)?;
            let config = Arc::new(Config::for_project(&owner.global, &project.canonical_root)?);
            let stages = Arc::new(owner.stages(&config)?);
            let source =
                crate::vector_cache::VectorCacheSource::Legacy(config.hybrid_cfg.cache.limits());
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
