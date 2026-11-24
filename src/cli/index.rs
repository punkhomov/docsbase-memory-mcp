//! `docsbase index` through the per-project writer or the running v2 daemon.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::daemon::registry;
use crate::error::{Error, Result};
use crate::ipc::async_client::{AsyncClient, within_operation};
use crate::search::operation::SearchOperation;
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
    path: Option<PathBuf>,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    started: Instant,
) -> Result<serde_json::Value> {
    let operation = SearchOperation::with_timeout(
        started,
        Duration::from_millis(context.global.remote.background_timeout_ms),
        crate::hybrid::work::Cancellation::new(),
    )?;
    let mut cancelled =
        crate::daemon::request::CancelOnDrop(Some(operation.cancellation().clone()));
    let activity = context.activity.track();
    let connected = within_operation(&operation, async {
        if let Ok(mut client) = AsyncClient::connect(&context.transport, &context.cache).await {
            if let Err(error) = client.handshake_registry().await {
                // A legacy direct index may proceed without a negotiated peer.
                return if context.global.vector_store.is_none() {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            context.admit_daemon(&client)?;
            if context.global.vector_store.is_some() {
                return Ok(Some(client));
            }
        }
        Ok(None)
    })
    .await?;
    let result = if let Some(client) = connected {
        within_operation(
            &operation,
            index_on_daemon(&context, &operation, &activity, client, path),
        )
        .await
    } else {
        run_with_operation(context, path, settings, operation, activity).await
    };
    if result.is_ok() {
        cancelled.0 = None;
    }
    result
}

async fn index_on_daemon(
    context: &super::command::Context,
    operation: &SearchOperation,
    activity: &Arc<crate::daemon::request::Activity>,
    mut client: AsyncClient,
    path: Option<PathBuf>,
) -> Result<serde_json::Value> {
    if client
        .capabilities()
        .iter()
        .any(|cap| cap == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY)
    {
        return Err(Error::Admission {
            message: "website indexing uses `docsbase sync` for its fixed project".into(),
        });
    }
    let target = context
        .control(operation, activity, move || canonical_target(path))
        .await?;
    let mut args = project_path_args(&target)?;
    args["refresh_config"] = serde_json::Value::Bool(true);
    // A registry-only session keeps the daemon alive between job-status polls
    // and avoids auto-indexing as a side effect of registration.
    client.bind_read_only(None).await?;
    let job = client.call_tool("index_project", args).await?;
    let poll = Duration::from_millis(context.global.daemon.poll_interval_ms);
    let finished = super::sync_command::await_job(&mut client, operation, poll, job).await?;
    match finished["state"].as_str() {
        Some("done") => {}
        Some("error") => {
            return Err(Error::Admission {
                message: finished["stats"]["error"]
                    .as_str()
                    .unwrap_or("daemon indexing failed")
                    .to_owned(),
            });
        }
        _ => {
            return Err(Error::Protocol {
                message: "unexpected daemon index job state".into(),
            });
        }
    }
    let project_id = finished["project_id"]
        .as_i64()
        .ok_or_else(|| Error::Protocol {
            message: "daemon index job missing project_id".into(),
        })?;
    let stats: crate::index::job::JobStats = serde_json::from_value(finished["stats"].clone())
        .map_err(|error| Error::Protocol {
            message: format!("invalid daemon index job statistics: {error}"),
        })?;
    Ok(serde_json::json!({ "project_id": project_id, "stats": stats }))
}

pub(super) fn project_path_args(path: &Path) -> Result<serde_json::Value> {
    let path = path.to_str().ok_or_else(|| Error::Project {
        message: "daemon indexing requires a UTF-8 project path".into(),
        instruction: None,
    })?;
    Ok(serde_json::json!({ "path": path }))
}

fn canonical_target(path: Option<PathBuf>) -> Result<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::internal_with_source("resolve current directory", e))?;
    let target = path.unwrap_or(cwd);
    target.canonicalize().map_err(|e| Error::Project {
        message: format!(
            "{} does not exist or cannot be resolved: {e}",
            target.display()
        ),
        instruction: Some(
            "re-create the worktree/directory or run from a valid project root".into(),
        ),
    })
}

pub(super) async fn run_with_operation(
    context: super::command::Context,
    path: Option<PathBuf>,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    operation: SearchOperation,
    activity: Arc<crate::daemon::request::Activity>,
) -> Result<serde_json::Value> {
    let owner = context.clone();
    let captured = context
        .control(&operation, &activity, move || {
            let target = canonical_target(path)?;
            let (project, config, source) = if let Some(policy) = &owner.global.vector_store {
                let root = registry::project_root_for(&target, &owner.cache)?;
                let config = Config::for_project(&owner.global, &root)?;
                let store = VectorStore::acquire(&owner.cache, policy)?;
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
