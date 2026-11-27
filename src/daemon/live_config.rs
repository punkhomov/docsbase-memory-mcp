//! Reviewed configuration plans and publication for running projects.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use crate::config::observe::{
    self, ActiveProjectConfig, AnnObservation, LiveProjectChange, PlanningLimits, ProjectConfigPlan,
};
use crate::config::policy::{
    self, ConsentDecision, ConsentReview, GrantSet, InvalidationPolicy, PolicyContext, PolicyMode,
};
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::ipc::protocol;
use crate::search::operation::SearchOperation;
use crate::store::models::Project;
use crate::store::project_index::{self, ProjectIndex};
use crate::store::registry_inspect::{ProjectRecipeInspection, inspect_live_project_recipes};
use crate::vector_cache::EmbeddingRecipe;

use super::{ProjectCell, Shared, capture_catalog_model, registry, request, scoped};

pub(super) async fn inspect(
    shared: &Arc<Shared>,
    scope: &scoped::Scope,
    version: u32,
    build_id: &str,
    path: PathBuf,
) -> Result<serde_json::Value> {
    protocol::ensure_protocol_version(version)?;
    if build_id != protocol::build_id() {
        return Err(Error::Admission {
            message: "restart the daemon with the same docsbase build before planning".into(),
        });
    }
    let operation = inspection_operation(shared)?;
    control(shared, scope, &operation, move |shared, operation| {
        report(shared, &path, operation)
    })
    .await
}

pub(super) async fn apply(
    shared: &Arc<Shared>,
    scope: &scoped::Scope,
    path: PathBuf,
    plan_id: String,
    allow_remote: bool,
    follow_job: bool,
) -> Result<serde_json::Value> {
    let started = Instant::now();
    let inspection = inspection_operation(shared)?;
    let reviewed_id = plan_id.clone();
    let work = control(shared, scope, &inspection, move |shared, operation| {
        prepare_apply(
            shared,
            &path,
            &reviewed_id,
            allow_remote,
            follow_job,
            operation,
        )
    })
    .await?;
    let context = match work {
        Apply::Reply(reply) => return Ok(reply),
        Apply::Index(context) => *context,
        Apply::Ann { warm, index } => {
            let scheduled = shared.jobs.get().is_some_and(|service| {
                shared
                    .watch_operation()
                    .is_ok_and(|operation| warm.schedule(shared, service, &operation, index))
            });
            return Ok(serde_json::json!({
                "apply_status": 0,
                "applied": true,
                "plan_id": plan_id,
                "ann_warm": if scheduled { "scheduled" } else { "deferred" },
            }));
        }
    };
    let service = shared
        .jobs
        .get()
        .ok_or_else(|| unavailable("catalog index executor is unavailable"))?;
    let timeout_ms = shared.global_config.remote.background_timeout_ms;
    let operation = SearchOperation::with_timeout(
        started,
        Duration::from_millis(timeout_ms),
        Cancellation::new(),
    )?;
    let mut cancel = request::CancelOnDrop(Some(operation.cancellation().clone()));
    let job = tokio::time::timeout_at(
        tokio::time::Instant::from_std(inspection.deadline()),
        super::run_catalog_sync(
            shared,
            service,
            context,
            Arc::clone(&scope.activity),
            operation,
            true,
        ),
    )
    .await
    .map_err(|_| unavailable("live config index admission exceeded the control deadline"))??;
    cancel.0 = None;
    Ok(serde_json::json!({
        "apply_status": 0,
        "applied": false,
        "plan_id": plan_id,
        "job": job,
        "timeout_ms": timeout_ms,
        "poll_ms": shared.daemon_settings().poll_interval_ms,
    }))
}

enum Apply {
    Reply(serde_json::Value),
    Index(Box<crate::index::async_job::catalog_sync::Context>),
    Ann {
        warm: super::CatalogAnnWarm,
        index: crate::store::registry_inspect::ObservedProjectIndex,
    },
}

async fn control<T: Send + 'static>(
    shared: &Arc<Shared>,
    scope: &scoped::Scope,
    operation: &SearchOperation,
    execute: impl FnOnce(&Shared, &SearchOperation) -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = tokio::time::timeout_at(
        tokio::time::Instant::from_std(operation.deadline()),
        shared.workers.control(),
    )
    .await
    .map_err(|_| unavailable("live config planning deadline expired in the control queue"))??;
    let shared = Arc::clone(shared);
    let work = operation.clone();
    let worker = request::spawn(Some(permit), Some(Arc::clone(&scope.activity)), move || {
        execute(&shared, &work)
    });
    request::run(worker, Some(operation)).await
}

pub(super) fn inspection_operation(shared: &Shared) -> Result<SearchOperation> {
    let timeout = Duration::from_millis(shared.daemon_settings().control_timeout_ms)
        .min(shared.global_config.client.ipc_io_timeout());
    SearchOperation::with_timeout(Instant::now(), timeout, Cancellation::new())
}

pub(super) fn report(
    shared: &Shared,
    path: &Path,
    operation: &SearchOperation,
) -> Result<serde_json::Value> {
    let (_, plan) = snapshot(shared, path, operation)?;
    let check = plan.live_check_code();
    Ok(serde_json::json!({"plan": plan, "check_status": check}))
}

pub(super) fn snapshot(
    shared: &Shared,
    path: &Path,
    operation: &SearchOperation,
) -> Result<(Project, ProjectConfigPlan)> {
    operation.check()?;
    let epoch = shared
        .catalog_epoch
        .ok_or_else(|| unavailable("live planning requires a v5 catalog"))?;
    let config_dir = shared
        .global_config_path
        .as_deref()
        .and_then(Path::parent)
        .ok_or_else(|| unavailable("daemon configuration source is unavailable"))?;
    let root = registry::project_root_for(path, &shared.cache)?;
    let limits = PlanningLimits::standard();
    let (project, inspection, config, ann) = {
        let reader = shared.readers.try_acquire()?;
        let project = registry::find_by_root(reader.connection(), &root)?
            .ok_or_else(|| unavailable("project is not registered with the daemon"))?;
        shared.admit_project(&project)?;
        let slot = existing_slot(shared, project.id);
        let _preparation = slot
            .as_ref()
            .map(|slot| {
                slot.preparation
                    .try_read()
                    .map_err(|_| Shared::catalog_config_busy())
            })
            .transpose()?;
        let config = runtime_config(slot.as_deref())?;
        let evidence = inspect_live_project_recipes(
            &reader,
            &project,
            limits.registry,
            |index, embedding, metadata| match slot.as_ref().zip(config.as_deref()) {
                Some((slot, config)) => {
                    observe_ann(shared, slot, config, index, embedding, metadata, operation)
                }
                None => AnnObservation::Unknown,
            },
        )?;
        // Initial lazy resolution also takes a read guard. Reject it
        // if it appeared while the paired artifact was being read.
        let current_slot = slot.clone().or_else(|| existing_slot(shared, project.id));
        let current = runtime_config(current_slot.as_deref())?;
        match (config.as_ref(), current.as_ref()) {
            (None, None) => {}
            (Some(old), Some(new)) if Arc::ptr_eq(old, new) => {}
            _ => return Err(Shared::catalog_config_busy()),
        }
        let (selected, ann) = evidence
            .map_or((None, AnnObservation::Unknown), |(selected, ann)| {
                (Some(selected), ann)
            });
        if selected
            .as_ref()
            .is_some_and(|selected| selected.index.epoch != Some(epoch))
        {
            return Err(unavailable("catalog installation changed during planning"));
        }
        let inspection = ProjectRecipeInspection {
            schema_version: Some(shared.registry_schema()),
            epoch: Some(epoch),
            project_id: Some(project.id),
            selected,
        };
        (project, inspection, config, ann)
    };
    operation.check()?;
    let plan = observe::live_project_plan(
        config_dir,
        &shared.cache,
        &root,
        limits,
        inspection,
        ActiveProjectConfig {
            config,
            global: shared.global_config_stamp.clone(),
            daemon: &shared.global_config,
            overrides: &shared.global_config_overrides,
            defaults: shared.global_config_defaults,
            ann,
        },
    )?;
    Ok((project, plan))
}

fn prepare_apply(
    shared: &Shared,
    path: &Path,
    plan_id: &str,
    allow_remote: bool,
    follow_job: bool,
    operation: &SearchOperation,
) -> Result<Apply> {
    use crate::config::bootstrap::{Entrypoint, EntrypointFacts};

    let (project, report) = snapshot(shared, path, operation)?;
    let reply = |status, applied| {
        Apply::Reply(
            serde_json::json!({"apply_status": status, "applied": applied, "plan_id": plan_id}),
        )
    };
    if report.plan.plan_id != plan_id {
        return Ok(reply(4, false));
    }
    if !report.plan.body.blockers.is_empty() {
        return Ok(reply(5, false));
    }
    if report.plan.body.actions.is_empty() {
        return Ok(reply(0, false));
    }
    let PolicyContext::Managed(policy) = policy::resolve(
        PolicyMode::Managed,
        Some(InvalidationPolicy::Apply),
        EntrypointFacts {
            mode: Entrypoint::Cli,
            is_tty: false,
        },
    ) else {
        return Err(unavailable(
            "live configuration requires an explicit apply policy",
        ));
    };
    let change = report
        .live_change
        .ok_or_else(|| unavailable("live project change is unavailable"))?;
    let replace_index = report
        .plan
        .body
        .actions
        .iter()
        .any(|action| change.replaces_index(action));
    let rebuild_ann = report
        .plan
        .body
        .actions
        .iter()
        .any(|action| change.rebuilds_ann(action));
    let review = ConsentReview::from_compiled(report.plan);
    if !matches!(
        review.evaluate(policy, GrantSet::new(allow_remote, false, false)),
        ConsentDecision::ConsentSatisfied(_)
    ) {
        return Ok(reply(5, false));
    }
    if replace_index {
        if !follow_job {
            return Ok(reply(5, false));
        }
        let context = super::capture_reviewed_catalog_sync(shared, project, change)?;
        return Ok(Apply::Index(Box::new(context)));
    }
    let index = change.reviewed.base;
    let config = Arc::clone(&change.config);
    let stages = publish_runtime(shared, &project, change, operation, |config| {
        shared.async_stages(config)
    })?;
    match stages {
        Some(stages) if rebuild_ann => Ok(Apply::Ann {
            warm: super::CatalogAnnWarm {
                project,
                config,
                stages,
            },
            index,
        }),
        Some(_) => Ok(reply(0, true)),
        None => Ok(reply(4, false)),
    }
}

pub(super) fn publish_runtime(
    shared: &Shared,
    project: &Project,
    change: LiveProjectChange,
    operation: &SearchOperation,
    build_stages: impl FnOnce(&crate::config::Config) -> Result<crate::hybrid::HybridStages>,
) -> Result<Option<Arc<crate::hybrid::HybridStages>>> {
    let Some(slot) = existing_slot(shared, project.id) else {
        return Ok(None);
    };
    let Some(expected) = change.active else {
        return Ok(None);
    };
    let config = change.config;
    operation.check()?;
    let stages =
        if config.hybrid_cfg == expected.hybrid_cfg && config.providers == expected.providers {
            let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state
                .config
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &expected))
            {
                return Ok(None);
            }
            state
                .search_stages
                .as_ref()
                .or(state.stages.as_ref())
                .cloned()
        } else {
            None
        };
    let stages = match stages {
        Some(stages) => stages,
        None => Arc::new(build_stages(&config)?),
    };
    let model = super::catalog_stage_model(&config, &stages)?;
    let recipe = model.map(EmbeddingRecipe::for_model);
    let watch_runtime = if shared.website.is_none() {
        let source = shared.vector_source_for(project.id, &config);
        source.validate_root(&shared.cache)?;
        Some(Arc::new(crate::watch::WatchRuntime::new(
            config.clone(),
            stages.clone(),
            source,
        )))
    } else {
        None
    };
    let base = change.reviewed.base;
    crate::config::paths::with_index_config_write_locks(
        shared.config_lock_dir()?,
        config.config_lock,
        shared.global_config_path.as_deref(),
        &project.canonical_root,
        || {
            operation.check()?;
            let reader = shared.readers.try_acquire()?;
            if !change.reviewed.head.is_selected(reader.connection())? {
                return Ok(None);
            }
            let installed =
                project_index::with_catalog_pair(reader, project, |registry, local, index| {
                    if index.catalog_generation() != base.epoch.zip(base.generation_id)
                        || index.content_revision() != base.content_revision
                        || !change.reviewed.head.is_selected(registry.connection())?
                    {
                        return Ok(None);
                    }
                    project_index::require_catalog_recipe(local.as_ref(), recipe)?;
                    let publication = slot.try_catalog_publication()?;
                    if !runtime_config(Some(&slot))?
                        .is_some_and(|active| Arc::ptr_eq(&active, &expected))
                    {
                        return Ok(None);
                    }
                    if change.reviewed.verify_sources().is_err() {
                        return Ok(None);
                    }
                    operation.check()?;
                    if config.ann_build != expected.ann_build {
                        shared.dense.invalidate_memory(project.id);
                    }
                    let old = publication.install(Arc::clone(&config), Arc::clone(&stages));
                    Ok(Some((publication.release_preparation(), old)))
                })?;
            let Some((_reload, _old)) = installed else {
                return Ok(None);
            };
            if let Some(runtime) = watch_runtime
                && let Err(error) =
                    shared
                        .watchers
                        .revise_with_runtime(&shared.cache, project.id, runtime)
            {
                eprintln!(
                    "warning: cannot revise watcher for project {}: {error}",
                    project.id
                );
            }
            Ok(Some(stages))
        },
    )
}

fn existing_slot(shared: &Shared, project_id: i64) -> Option<Arc<ProjectCell>> {
    shared
        .project_states
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&project_id)
        .cloned()
}

fn runtime_config(slot: Option<&ProjectCell>) -> Result<Option<Arc<crate::config::Config>>> {
    let Some(slot) = slot else { return Ok(None) };
    let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
    if state.unverified.is_some() {
        return Err(Shared::catalog_config_busy());
    }
    Ok(state.config.clone())
}

fn observe_ann(
    shared: &Shared,
    slot: &ProjectCell,
    config: &crate::config::Config,
    index: ProjectIndex,
    embedding: Option<EmbeddingRecipe>,
    metadata: Option<&rusqlite::Connection>,
    operation: &SearchOperation,
) -> AnnObservation {
    let model = match capture_catalog_model(slot, config, &shared.payload_budget) {
        Ok(None) => return AnnObservation::Disabled,
        Ok(Some(model)) if model.ranking != crate::hybrid::Ranking::Rrf => {
            return AnnObservation::Disabled;
        }
        Ok(Some(model)) if embedding == Some(EmbeddingRecipe::for_model(&model.key)) => model,
        _ => return AnnObservation::Unknown,
    };
    if let Some((vectors, estimated_bytes)) = shared
        .dense
        .ann_diagnostics_for(index, &model.key, config.ann_build)
        .resident
    {
        return AnnObservation::Resident {
            vectors,
            estimated_bytes,
        };
    }
    metadata
        .and_then(|metadata| {
            shared.dense.persisted_ann_for(
                index,
                &model.key,
                config.ann_build,
                metadata,
                shared.global_config.search.max_request_payload_bytes,
                || operation.check(),
            )
        })
        .map_or(AnnObservation::Unknown, |(vectors, dump_bytes)| {
            AnnObservation::Persisted {
                vectors,
                dump_bytes,
            }
        })
}

fn unavailable(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
