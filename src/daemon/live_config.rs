//! Read-only snapshot of a live project's configuration and selected artifacts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use crate::config::observe::{self, ActiveProjectConfig, AnnObservation, PlanningLimits};
use crate::error::{Error, Result};
use crate::ipc::protocol;
use crate::store::project_index::ProjectIndex;
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
    let permit = shared.workers.control().await?;
    let shared = Arc::clone(shared);
    let worker = request::spawn(Some(permit), Some(Arc::clone(&scope.activity)), move || {
        report(&shared, &path)
    });
    request::run(worker, None).await
}

pub(super) fn report(shared: &Shared, path: &Path) -> Result<serde_json::Value> {
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
    let (inspection, config, ann) = {
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
            |index, embedding| match slot.as_ref().zip(config.as_deref()) {
                Some((slot, config)) => observe_ann(shared, slot, config, index, embedding),
                None => AnnObservation::Unknown,
            },
        )?;
        // Initial lazy resolution does not take the preparation lock.
        // Reject it if it appeared while the paired artifact was being read.
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
        (inspection, config, ann)
    };
    let plan = observe::live_project_plan(
        config_dir,
        &shared.cache,
        &root,
        limits,
        inspection,
        ActiveProjectConfig {
            config,
            global: shared.global_config_stamp.clone(),
            ann,
        },
    )?;
    let check = plan.live_check_code();
    Ok(serde_json::json!({"plan": plan, "check_status": check}))
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
) -> AnnObservation {
    let model = match capture_catalog_model(slot, config, &shared.payload_budget) {
        Ok(None) => return AnnObservation::Disabled,
        Ok(Some(model)) if model.ranking != crate::hybrid::Ranking::Rrf => {
            return AnnObservation::Disabled;
        }
        Ok(Some(model)) if embedding == Some(EmbeddingRecipe::for_model(&model.key)) => model,
        _ => return AnnObservation::Unknown,
    };
    shared
        .dense
        .ann_diagnostics_for(index, &model.key, config.ann_build)
        .resident
        .map_or(AnnObservation::Unknown, |(vectors, estimated_bytes)| {
            AnnObservation::Resident {
                vectors,
                estimated_bytes,
            }
        })
}

fn unavailable(message: &str) -> Error {
    Error::Admission {
        message: message.to_owned(),
    }
}
