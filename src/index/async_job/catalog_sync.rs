//! Publication and terminal ownership for a daemon's lexical generation job.

use super::{Config, IndexState, PayloadPermit, Project, SearchOperation};
use crate::config::schema::Resolved;
use crate::daemon::{readiness::SyncAttribution, server::ProjectCell, tools};
use crate::error::{Error, Result};
use crate::index::{generation_job::GenerationJob, job::CaptureBudget};
use crate::store::{
    generation_build::open_index_registry, models::SyncState, repo, settings::DbSettings,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Context {
    pub(crate) cache: PathBuf,
    pub(crate) epoch: [u8; 16],
    pub(crate) settings: Resolved<DbSettings>,
    pub(crate) project: Project,
    pub(crate) config: Arc<Config>,
    pub(crate) revision: Arc<ProjectCell>,
    pub(crate) owner: Owner,
}

pub(crate) struct Owner {
    active_id: Arc<AtomicI64>,
    _singleton: OwnedSemaphorePermit,
}

pub(crate) struct Slot {
    permit: Arc<Semaphore>,
    active_id: Arc<AtomicI64>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            permit: Arc::new(Semaphore::new(1)),
            active_id: Arc::new(AtomicI64::new(0)),
        }
    }
}

impl Slot {
    pub(crate) fn try_own(&self) -> Option<Owner> {
        Some(Owner {
            active_id: self.active_id.clone(),
            _singleton: self.permit.clone().try_acquire_owned().ok()?,
        })
    }

    pub(crate) fn active_id(&self) -> i64 {
        self.active_id.load(Ordering::Acquire)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Clear the joinable identity before releasing admission for a retry.
        self.active_id.store(0, Ordering::Release);
    }
}

pub(super) struct CatalogSync {
    context: Context,
    id: i64,
}

impl IndexState {
    pub(crate) fn prepare_catalog_sync(
        &mut self,
        context: Context,
        operation: &SearchOperation,
    ) -> Result<Value> {
        operation.check()?;
        let db = open_index_registry(&context.cache, &context.settings, context.epoch)?;
        // Context holds the sole native owner slot through retirement. Any
        // leftover active row belongs to an owner whose terminal write failed.
        if let Some(orphan) = repo::active_sync_job(db.connection(), context.project.id)? {
            repo::finish_sync_job(
                db.connection(),
                orphan.id,
                SyncState::Error,
                Some(r#"{"orphaned":true}"#),
            )?;
        }
        let (id, created) = tools::enqueue_sync(&db, &context.project)?;
        if !created {
            return Err(super::active_sync_conflict());
        }
        let sync = self.catalog_sync.insert(CatalogSync { context, id });
        repo::mark_sync_running(db.connection(), id)?;
        let status = tools::sync_status(&db, &json!({"job_id": id}))?;
        db.close()?;
        sync.context.owner.active_id.store(id, Ordering::Release);
        Ok(status)
    }

    pub(crate) fn run_catalog_sync(&mut self, operation: &SearchOperation) -> Result<()> {
        let sync = self
            .catalog_sync
            .as_ref()
            .ok_or_else(|| Error::internal("catalog sync owner missing"))?;
        let account = self
            .account
            .as_ref()
            .ok_or_else(|| Error::internal("catalog sync payload account missing"))?;
        let result = sync.run(operation, account);
        if let Err(error) = &result {
            self.fail(error.to_string());
        }
        result
    }
}

impl CatalogSync {
    fn run(&self, operation: &SearchOperation, account: &PayloadPermit) -> Result<()> {
        let Context {
            cache,
            epoch,
            settings,
            project,
            config,
            revision,
            ..
        } = &self.context;
        let mut capture = CaptureBudget {
            account,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let job = GenerationJob {
            cache,
            project,
            epoch: *epoch,
            settings,
            config,
        };
        job.prepare(&mut capture, |prepared| {
            let outcome = prepared.outcome().clone();
            prepared.publish(|commit| {
                let _revision = revision.preparation_lease(config)?;
                commit.commit_with(
                    |conn, published| {
                        let id = published.generation_id.ok_or_else(|| {
                            Error::internal("published catalog sync generation missing")
                        })?;
                        let mut record = serde_json::to_value(&outcome.stats).map_err(|error| {
                            Error::internal_with_source("encode catalog sync counters", error)
                        })?;
                        record["index_outcome"] =
                            serde_json::to_value(SyncAttribution::for_generation(&outcome, id))
                                .map_err(|error| {
                                    Error::internal_with_source(
                                        "encode catalog sync identity",
                                        error,
                                    )
                                })?;
                        repo::finish_sync_job(
                            conn,
                            self.id,
                            SyncState::Done,
                            Some(&encoded_record(&record)?),
                        )
                    },
                    || operation.check(),
                )
            })
        })?;
        Ok(())
    }

    pub(super) fn terminal(&self, error: Option<&str>) -> Result<Value> {
        let context = &self.context;
        let db = open_index_registry(&context.cache, &context.settings, context.epoch)?;
        let job = repo::sync_job(db.connection(), self.id)?
            .ok_or_else(|| Error::internal("owned catalog sync job missing"))?;
        if matches!(job.state, SyncState::Queued | SyncState::Running) {
            // Success is already recorded in the same transaction as the head.
            // Cancellation, failed work and indeterminate returns must not erase it.
            let record = encoded_record(&json!({
                "error": error.unwrap_or("background catalog job cancelled or abandoned")
            }))?;
            repo::finish_sync_job(db.connection(), self.id, SyncState::Error, Some(&record))?;
        }
        let status = tools::sync_status(&db, &json!({"job_id": self.id}))?;
        db.close()?;
        Ok(status)
    }
}

fn encoded_record(record: &Value) -> Result<String> {
    let encoded = serde_json::to_string(record)
        .map_err(|error| Error::internal_with_source("encode catalog sync record", error))?;
    if encoded.len() as u64 > crate::limits::MAX_STATE_BYTES {
        return Err(Error::internal(
            "catalog sync record exceeds the state bound",
        ));
    }
    Ok(encoded)
}
