//! Explicit CLI consent and admission for a stopped project replacement.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{
    Config,
    bootstrap::{Entrypoint, EntrypointFacts},
    change::ChangePlan,
    observe::{self, PlanningLimits},
    policy::{
        self, ConsentDecision, ConsentReview, GrantSet, InvalidationPolicy, PolicyContext,
        PolicyMode,
    },
    schema::Resolved,
};
use crate::daemon::{admission, registry, request::CancelOnDrop};
use crate::error::Error;
use crate::hybrid::work::Cancellation;
use crate::index::async_job::catalog_sync::{self, BuildContext, DirectPublication};
use crate::search::operation::SearchOperation;
use crate::store::{
    GENERATION_SCHEMA_VERSION, generation_build::open_index_registry, settings::DbSettings,
};
use crate::vector_cache::{VectorCacheSource, VectorStore};

use super::{CheckStatus, ProjectPaths};

pub(super) fn run(
    paths: ProjectPaths,
    plan_id: &str,
    allow_remote: bool,
    settings: &Resolved<DbSettings>,
) -> anyhow::Result<()> {
    println!("{}", execute(paths, plan_id, allow_remote, settings, None)?);
    Ok(())
}

pub(super) fn execute(
    paths: ProjectPaths,
    plan_id: &str,
    allow_remote: bool,
    settings: &Resolved<DbSettings>,
    profile: Option<&crate::daemon::settings::DaemonProfile>,
) -> anyhow::Result<serde_json::Value> {
    let started = Instant::now();
    let limits = PlanningLimits::standard();
    let ProjectPaths {
        config,
        cache,
        root,
    } = paths;
    let preview =
        observe::project_plan_for_profile(&config, &cache, &root, settings, limits, profile)?;
    authorize(preview.plan, plan_id, allow_remote)?;
    if preview.replacement.is_none() {
        return Ok(serde_json::json!({"plan_id": plan_id, "applied": false}));
    }

    // Keep the daemon out until all native work retires. Direct Index can
    // still race this command and is serialized by the catalog project leases.
    let _admission = acquire_stopped(&cache)?;
    let current =
        observe::project_plan_for_profile(&config, &cache, &root, settings, limits, profile)?;
    authorize(current.plan, plan_id, allow_remote)?;
    let replacement = current.replacement.ok_or(CheckStatus::Stale)?;
    let reviewed = replacement.reviewed;
    let epoch = reviewed.base.epoch.ok_or(CheckStatus::Unavailable)?;
    let id = reviewed.base.project_id;
    let global = replacement.global;
    let settings = crate::store::settings::db_from_config(&global)?;
    let project_config = Arc::new(replacement.config);
    let stats = crate::cli::command::run(global.into_settings(), move |context| async move {
        let operation = SearchOperation::with_timeout(
            started,
            Duration::from_millis(context.global.remote.background_timeout_ms),
            Cancellation::new(),
        )?;
        let mut cancelled = CancelOnDrop(Some(operation.cancellation().clone()));
        let activity = context.activity.track();
        let owner = context.clone();
        let (build, reviewed) = context
            .control(&operation, &activity, move || {
                reviewed.verify_sources()?;
                let db = open_index_registry(&cache, &settings, epoch)?;
                if !reviewed.head.is_selected(db.connection())? {
                    return Err(admission(
                        "reviewed catalog head changed before preparation",
                    ));
                }
                let project = registry::project_by_id(&db, id)?
                    .filter(|project| project.canonical_root == root)
                    .ok_or_else(|| admission("reviewed project registration changed"))?;
                let cache = db.cache_root().canonicalize().map_err(|error| {
                    Error::internal_with_source("resolve generation registry cache", error)
                })?;
                db.close()?;
                let policy = project_config.vector_store.as_ref().ok_or_else(|| {
                    admission("plan application requires version 2 configuration")
                })?;
                let store = VectorStore::reserve(&cache, policy)?;
                let mut embedding_config = Config::clone(&project_config);
                embedding_config.hybrid_cfg.reranker = None;
                let stages = Arc::new(owner.stages(&embedding_config)?);
                let pin_ttl = project_config.dense.ann_lifetimes.backfill_pin_ttl();
                Ok((
                    BuildContext {
                        cache,
                        epoch,
                        settings,
                        project,
                        config: project_config,
                        stages,
                        active_model: None,
                        source: VectorCacheSource::Owned(store),
                        pin_ttl,
                    },
                    reviewed,
                ))
            })
            .await?;
        let stats = catalog_sync::index_direct(
            &context.jobs,
            &context.workers,
            &activity,
            &operation,
            &context.budget,
            build,
            DirectPublication {
                config_dir: config,
                reviewed: Some(reviewed),
            },
        )
        .await?;
        cancelled.0 = None;
        Ok(stats)
    })?;
    Ok(serde_json::json!({"plan_id": plan_id, "applied": true, "project_id": id, "stats": stats}))
}

fn acquire_stopped(cache: &Path) -> anyhow::Result<admission::Lease> {
    match admission::Lease::acquire(
        cache,
        &crate::ipc::protocol::build_id(),
        GENERATION_SCHEMA_VERSION,
    ) {
        Err(error) if admission::is_lock_busy(&error) => {
            Err(anyhow::Error::from(error).context(CheckStatus::Unavailable))
        }
        result => result.map_err(anyhow::Error::from),
    }
}

fn authorize(plan: ChangePlan, plan_id: &str, allow_remote: bool) -> Result<(), CheckStatus> {
    if !plan.body.blockers.is_empty() {
        return Err(CheckStatus::Unavailable);
    }
    if plan.plan_id != plan_id {
        return Err(CheckStatus::Stale);
    }
    let PolicyContext::Managed(policy) = policy::resolve(
        PolicyMode::Managed,
        Some(InvalidationPolicy::Apply),
        EntrypointFacts {
            mode: Entrypoint::Cli,
            is_tty: false,
        },
    ) else {
        return Err(CheckStatus::Unavailable);
    };
    match ConsentReview::from_compiled(plan)
        .evaluate(policy, GrantSet::new(allow_remote, false, false))
    {
        ConsentDecision::ConsentSatisfied(_) => Ok(()),
        ConsentDecision::PromptRequired(_) | ConsentDecision::Refused(_) => {
            Err(CheckStatus::MissingGrants)
        }
    }
}

fn admission(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
