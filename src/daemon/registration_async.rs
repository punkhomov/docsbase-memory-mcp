//! Root-enabled registration: CPU metadata → optional admitted full job → binding.
use super::{
    Activity, Arc, Db, Error, Path, Result, Shared, Value, finish_registration,
    index_async_with_operation, json, oneshot, registry, request,
};
use crate::search::operation::SearchOperation;
use crate::store::models::Project;

struct Plan {
    project: Project,
    index: bool,
}

pub(super) async fn run(
    shared: &Arc<Shared>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
    activity: Option<Arc<Activity>>,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    let operation = shared.watch_operation()?;
    let mut cancellation = request::CancelOnDrop(Some(operation.cancellation().clone()));
    let activity = activity.unwrap_or_else(|| shared.activity.track());
    let outcome = prepare_and_index(shared, cwd, &operation, &activity).await;
    let result = finish_registration(shared, pid, cwd, allow_unbound, outcome);
    cancellation.0 = None;
    result
}

async fn prepare_and_index(
    shared: &Arc<Shared>,
    cwd: &Path,
    operation: &SearchOperation,
    activity: &Arc<Activity>,
) -> Result<Project> {
    let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
        .wait(
            &crate::hybrid::work::Cancellation::new(),
            shared.workers.control(),
        )
        .await?;
    let owner = shared.clone();
    let root = cwd.to_path_buf();
    let call = operation.clone();
    let work = request::spawn(Some(permit), Some(activity.clone()), move || {
        prepare(&owner, &root, &call)
    });
    let plan = request::run(work, Some(operation)).await?;
    if !plan.index {
        return Ok(plan.project);
    }
    let service = shared
        .jobs
        .get()
        .ok_or_else(|| Error::internal("registration job root absent"))?;
    let indexed = index_async_with_operation(
        shared,
        service,
        None,
        json!({"path": plan.project.canonical_root}),
        Some(activity.clone()),
        false,
        operation.clone(),
    )
    .await;
    let permit = crate::hybrid::work::RemoteCall::new(operation.clone())
        .wait(
            &crate::hybrid::work::Cancellation::new(),
            shared.workers.control(),
        )
        .await?;
    let owner = shared.clone();
    let cwd = cwd.to_path_buf();
    let call = operation.clone();
    let work = request::spawn(Some(permit), Some(activity.clone()), move || {
        call.check()?;
        let db = Db::open_with_settings(&owner.cache, &owner.db_settings)?;
        if let Err(error) = indexed {
            // Preserve the existing concurrent-registration winner rule.
            if let Ok(project) = registry::resolve_by_cwd(&db, &cwd)
                && project.status == crate::store::models::ProjectStatus::Indexed
            {
                return Ok(project);
            }
            return Err(error);
        }
        let project = registry::project_by_id(&db, plan.project.id)?
            .ok_or_else(|| Error::internal("project vanished after automatic index"))?;
        owner.open_project_config(&project)?;
        Ok(project)
    });
    request::run(work, Some(operation)).await
}

fn prepare(shared: &Arc<Shared>, cwd: &Path, operation: &SearchOperation) -> Result<Plan> {
    #[cfg(test)]
    shared.observe_registration_work();
    operation.check()?;
    let mut db = Db::open_with_settings(&shared.cache, &shared.db_settings)?;
    let error = match registry::resolve_by_cwd(&db, cwd) {
        Ok(project) => {
            shared.open_project_config(&project)?;
            shared.schedule_backfill_in(&project, Some(operation.clone()))?;
            return Ok(Plan {
                project,
                index: false,
            });
        }
        Err(error) => error,
    };
    let Ok(root) = registry::project_root_for(cwd, &shared.cache) else {
        return Err(error);
    };
    if !shared.config_for_root(&root)?.auto_index {
        return Err(error);
    }
    operation.check()?;
    let project = registry::ensure_project(&mut db, &root)?;
    if !shared.project_runtime_for_search(&project)?.0.auto_index {
        return Err(error);
    }
    Ok(Plan {
        project,
        index: true,
    })
}
