//! Publication and terminal ownership for a daemon's catalog generation job.

use super::{
    Activity, BatchPacket, Config, IndexState, JobOwner, JobService, PayloadPermit, Project,
    SearchOperation, WorkerAdmission,
};
use crate::config::schema::Resolved;
use crate::daemon::{readiness::SyncAttribution, server::ProjectCell, tools};
use crate::error::{Error, Result};
use crate::hybrid::{
    HybridStages,
    async_jobs::{self, EmbeddingBatch},
};
use crate::index::{generation_job::GenerationJob, job::CaptureBudget};
use crate::store::{
    generation_build::{PreparedLexicalPublication, open_index_registry},
    models::SyncState,
    repo,
    settings::DbSettings,
};
use crate::vector_cache::{JobPinAuthority, VectorCacheSource};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct Context {
    pub(crate) cache: PathBuf,
    pub(crate) epoch: [u8; 16],
    pub(crate) settings: Resolved<DbSettings>,
    pub(crate) project: Project,
    pub(crate) config: Arc<Config>,
    pub(crate) stages: Arc<HybridStages>,
    pub(crate) source: VectorCacheSource,
    pub(crate) pin_ttl: Duration,
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
    staged: Option<PreparedLexicalPublication>,
    embedding: Option<CatalogEmbeddings>,
    context: Context,
    id: i64,
}

struct CatalogEmbeddings {
    cursor: (i64, i64),
    pin: JobPinAuthority,
    dimension: usize,
    complete: bool,
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
        let sync = self.catalog_sync.insert(CatalogSync {
            staged: None,
            embedding: None,
            context,
            id,
        });
        repo::mark_sync_running(db.connection(), id)?;
        let status = tools::sync_status(&db, &json!({"job_id": id}))?;
        db.close()?;
        sync.context.owner.active_id.store(id, Ordering::Release);
        Ok(status)
    }
}

pub(crate) async fn execute(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: &Arc<Activity>,
    operation: &SearchOperation,
    owner: JobOwner,
) -> Result<()> {
    let call = operation.clone();
    let (owner, embed) = service
        .phase(workers, activity.clone(), operation, owner, move |state| {
            let account = state
                .account
                .as_deref()
                .ok_or_else(|| Error::internal("catalog sync payload account missing"))?;
            let sync = state
                .catalog_sync
                .as_mut()
                .ok_or_else(|| Error::internal("catalog sync owner missing"))?;
            let embed = sync.stage(&call, account)?;
            if !embed {
                sync.publish(&call)?;
            }
            Ok(embed)
        })
        .await?;
    let needs_embeddings = **embed;
    drop(embed);
    if !needs_embeddings {
        return Ok(());
    }
    let owner = super::embed_batches(service, workers, activity, operation, owner, false).await?;
    let call = operation.clone();
    let (owner, published) = service
        .phase(workers, activity.clone(), operation, owner, move |state| {
            state
                .catalog_sync
                .as_mut()
                .ok_or_else(|| Error::internal("catalog sync owner missing"))?
                .publish(&call)
        })
        .await?;
    drop(published);
    drop(owner);
    Ok(())
}

impl CatalogSync {
    fn stage(&mut self, operation: &SearchOperation, account: &PayloadPermit) -> Result<bool> {
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
        drop(revision.preparation_lease(config)?);
        let prepared = job.stage(&mut capture)?;
        if let Some(embedder) = &self.context.stages.embedder {
            let (count, longest) =
                prepared.with_metadata(|db| super::embedding_shape(db, project.id))?;
            if count > 0 {
                let dimension = super::embedding_dimension(config, embedder.as_ref())?;
                capture.reserve(super::embedding_workspace(
                    &config.remote,
                    longest,
                    0,
                    count,
                    dimension,
                )?)?;
                let pin = JobPinAuthority::allocate(
                    cache,
                    self.context.source.clone(),
                    embedder.model_id().to_owned(),
                    self.context.pin_ttl,
                    count,
                )?;
                self.embedding = Some(CatalogEmbeddings {
                    cursor: (0, -1),
                    pin,
                    dimension,
                    complete: false,
                });
            }
        }
        self.staged = Some(prepared);
        Ok(self.embedding.is_some())
    }

    pub(super) fn batch_packet(
        &mut self,
        operation: &SearchOperation,
        account: Option<Arc<PayloadPermit>>,
    ) -> Result<BatchPacket> {
        let context = &self.context;
        let _revision = context.revision.preparation_lease(&context.config)?;
        let embedder = context
            .stages
            .embedder
            .as_ref()
            .ok_or_else(|| Error::internal("catalog embedding stage absent"))?;
        let embedding = self
            .embedding
            .as_mut()
            .ok_or_else(|| Error::internal("catalog embedding work absent"))?;
        let staged = self
            .staged
            .as_ref()
            .ok_or_else(|| Error::internal("catalog stage absent"))?;
        let texts = staged.with_metadata(|db| {
            super::embedding_texts(
                db,
                context.project.id,
                &mut embedding.cursor,
                &context.config.remote,
                operation,
            )
        })?;
        if texts.is_empty() {
            embedding.complete = true;
            return Ok((None, None, account));
        }
        for text in &texts {
            embedding
                .pin
                .track(&crate::vector_cache::chunk_hash(text))?;
        }
        embedding.pin.check_capacity(embedding.dimension)?;
        embedding.pin.renew()?;
        let batch = async_jobs::missing(
            &context.source,
            &context.cache,
            embedder.model_id(),
            texts,
            Some(embedding.dimension),
        )?;
        Ok((Some(batch), Some(embedder.clone()), account))
    }

    pub(super) fn store(&self, batch: &EmbeddingBatch, vectors: &[Vec<f32>]) -> Result<()> {
        let context = &self.context;
        let _revision = context.revision.preparation_lease(&context.config)?;
        let embedder = context
            .stages
            .embedder
            .as_ref()
            .ok_or_else(|| Error::internal("catalog embedding stage absent"))?;
        let embedding = self
            .embedding
            .as_ref()
            .ok_or_else(|| Error::internal("catalog embedding work absent"))?;
        embedding.pin.renew()?;
        async_jobs::store(
            &context.source,
            &context.cache,
            embedder.model_id(),
            batch,
            vectors,
            embedding.dimension,
        )
    }

    fn publish(&mut self, operation: &SearchOperation) -> Result<()> {
        operation.check()?;
        let context = &self.context;
        let embedding = self.embedding.as_ref();
        if embedding.is_some_and(|embedding| !embedding.complete) {
            return Err(Error::internal("catalog embedding scan did not finish"));
        }
        let prepared = self
            .staged
            .take()
            .ok_or_else(|| Error::internal("catalog stage absent"))?;
        let outcome = prepared.outcome().clone();
        prepared.publish(|commit| {
            operation.check()?;
            let _revision = context.revision.preparation_lease(&context.config)?;
            let verification = embedding
                .map(|embedding| {
                    embedding.pin.renew()?;
                    operation.check()?;
                    embedding.pin.prepare_verification(embedding.dimension)
                })
                .transpose()?;
            commit.commit_with(
                || {
                    operation.check()?;
                    verification
                        .map(|proof| proof.hold_verified(|| operation.check()))
                        .transpose()
                },
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
                                Error::internal_with_source("encode catalog sync identity", error)
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
