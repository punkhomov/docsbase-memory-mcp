//! Root-enabled registration: CPU metadata → optional admitted full job → binding.
use super::{
    Activity, Arc, Db, Error, Path, Result, Shared, Value, capture_catalog_first_index,
    finish_registration, index_async_with_operation, json, oneshot, registry, request,
    run_catalog_sync,
};
use crate::config::policy::PolicyContext;
use crate::index::async_job::catalog_sync;
use crate::search::operation::SearchOperation;
use crate::store::models::Project;

enum Plan {
    Bind(Project),
    Index(Project),
    FirstGeneration(Box<catalog_sync::Context>),
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
    let service = shared
        .jobs
        .get()
        .ok_or_else(|| Error::internal("registration job root absent"))?;
    let (project, indexed) = match plan {
        Plan::Bind(project) => return Ok(project),
        Plan::Index(project) => {
            let indexed = index_async_with_operation(
                shared,
                service,
                None,
                json!({"path": project.canonical_root}),
                Some(activity.clone()),
                false,
                operation.clone(),
            )
            .await;
            (project, indexed)
        }
        Plan::FirstGeneration(context) => {
            let project = context.build.project.clone();
            let indexed = run_catalog_sync(
                shared,
                service,
                *context,
                activity.clone(),
                operation.clone(),
                false,
            )
            .await;
            (project, indexed)
        }
    };
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
        let db = owner.open_work_registry()?;
        if let Err(error) = indexed {
            // Preserve the existing concurrent-registration winner rule.
            if let Ok(project) =
                registry::resolve_by_cwd(&db, &cwd).map(|project| registered_view(&db, project))
                && project.status == crate::store::models::ProjectStatus::Indexed
            {
                return Ok(project);
            }
            return Err(error);
        }
        let project = registry::project_by_id(&db, project.id)?
            .ok_or_else(|| Error::internal("project vanished after automatic index"))?;
        let project = registered_view(&db, project);
        owner.open_project_config(&project)?;
        Ok(project)
    });
    request::run(work, Some(operation)).await
}

fn prepare(shared: &Arc<Shared>, cwd: &Path, operation: &SearchOperation) -> Result<Plan> {
    #[cfg(test)]
    shared.observe_registration_work();
    operation.check()?;
    let mut db = shared.open_work_registry()?;
    let error = match registry::resolve_by_cwd(&db, cwd) {
        Ok(project) => return prepare_registered(shared, &db, project, operation),
        Err(error) => error,
    };
    let Ok(root) = registry::project_root_for(cwd, &shared.cache) else {
        return Err(error);
    };
    if !shared.config_for_root(&root)?.auto_index {
        return Err(error);
    }
    operation.check()?;
    let (project, inserted) = registry::ensure_project_with_insert(&mut db, &root)?;
    if shared.catalog_epoch.is_some() && matches!(shared.invalidation, PolicyContext::Managed(_)) {
        if !inserted {
            let existing = registry::resolve_by_cwd(&db, cwd)?;
            return prepare_registered(shared, &db, existing, operation);
        }
        drop(db);
        let context = capture_catalog_first_index(shared, project)?;
        if !context.build.config.auto_index {
            return Err(error);
        }
        return Ok(Plan::FirstGeneration(Box::new(context)));
    }
    if !shared.project_runtime_for_search(&project)?.0.auto_index {
        return Err(error);
    }
    Ok(Plan::Index(project))
}

fn prepare_registered(
    shared: &Arc<Shared>,
    db: &Db,
    project: Project,
    operation: &SearchOperation,
) -> Result<Plan> {
    let project = registered_view(db, project);
    shared.open_project_config(&project)?;
    shared.schedule_backfill_in(&project, Some(operation.clone()))?;
    Ok(Plan::Bind(project))
}

fn registered_view(db: &Db, project: Project) -> Project {
    if db.has_generation_catalog()
        && let Ok(Some(local)) = crate::store::project_index::local_watcher_project(db, &project)
    {
        return local;
    }
    // Registration binds the root even when its selected index is busy or
    // needs repair. Explicit Sync can repair it through that session.
    project
}
