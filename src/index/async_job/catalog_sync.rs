//! Publication and terminal ownership for a daemon's catalog generation job.

use super::{
    Activity, BatchPacket, Config, IndexState, JobOwner, JobService, PayloadPermit, Project,
    SearchOperation, WorkerAdmission,
};
use crate::config::schema::Resolved;
use crate::daemon::{
    readiness::SyncAttribution,
    server::{CatalogConfigPublication, ProjectCell, ProjectState},
    tools,
};
use crate::error::{Error, Result};
use crate::hybrid::{
    HybridStages,
    async_jobs::{self, EmbeddingBatch},
};
use crate::index::{generation_job::GenerationJob, job::CaptureBudget};
use crate::store::{
    generation_build::{
        CatalogCommit, LexicalCommit, PreparedLexicalPublication, open_index_registry,
    },
    models::SyncState,
    registry_inspect::ObservedProjectIndex,
    repo,
    settings::DbSettings,
};
use crate::vector_cache::{EmbeddingRecipe, JobPinAuthority, VectorCacheSource, WriteCapacity};
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
    pub(crate) active_model: Option<String>,
    pub(crate) source: VectorCacheSource,
    pub(crate) pin_ttl: Duration,
    pub(crate) revision: Arc<ProjectCell>,
    pub(crate) owner: Owner,
    pub(crate) refresh: Option<RuntimeUpdate>,
}

pub(crate) struct RuntimeUpdate {
    pub(crate) active: Arc<Config>,
    pub(crate) global_config: Config,
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) watchers: Arc<crate::watch::Watchers>,
    pub(crate) watch_runtime: Arc<crate::watch::WatchRuntime>,
}

impl RuntimeUpdate {
    fn with_current_sources<T>(
        &self,
        context: &Context,
        operation: &SearchOperation,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let root = &context.project.canonical_root;
        crate::config::paths::with_index_config_write_locks(
            self.global_path.as_deref(),
            root,
            || {
                operation.check()?;
                if Config::for_project(&self.global_config, root)? != *context.config {
                    return Err(Error::Admission {
                        message:
                            "project index configuration changed before publication; retry index"
                                .into(),
                    }
                    .into());
                }
                Ok(publish()?)
            },
        )
        .map_err(|error| match error.downcast::<Error>() {
            Ok(error) => error,
            Err(error) => Error::Admission {
                message: format!("could not lock the index configuration: {error}"),
            },
        })
    }
}

impl Context {
    fn active_config(&self) -> &Arc<Config> {
        self.refresh
            .as_ref()
            .map_or(&self.config, |update| &update.active)
    }
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
    // Release candidate pins first, with the old pair still protected.
    protected_base: Option<JobPinAuthority>,
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
            protected_base: None,
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
) -> Result<ObservedProjectIndex> {
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
            if sync.stage(&call, account)? {
                Ok(None)
            } else {
                sync.publish(&call).map(Some)
            }
        })
        .await?;
    let already_published = **embed;
    drop(embed);
    if let Some(published) = already_published {
        return Ok(published);
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
    let observed = **published;
    drop(published);
    drop(owner);
    Ok(observed)
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
        drop(revision.preparation_lease(self.context.active_config())?);
        let prepared = job.stage(&mut capture)?;
        if let Some(embedder) = &self.context.stages.embedder {
            if embedder.model_id().len() as u64 > EmbeddingRecipe::MAX_MODEL_BYTES {
                return Err(Error::Admission {
                    message: "embedding model key exceeds the catalog identity bound".into(),
                });
            }
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
                // Pins cover provider waits through the operation deadline.
                // Round up for the durable lease's second-granularity clock.
                let ttl = self.context.pin_ttl.max(
                    operation
                        .remaining()?
                        .saturating_add(Duration::from_secs(1)),
                );
                self.protected_base =
                    self.protect_base(&prepared, embedder.model_id(), ttl, &mut capture)?;
                let pin = JobPinAuthority::allocate(
                    cache,
                    self.context.source.clone(),
                    embedder.model_id().to_owned(),
                    ttl,
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

    fn base_model(
        &self,
        local: Option<&crate::store::project_index::LocalMetadata>,
        requested: &str,
        capture: &mut CaptureBudget<'_>,
    ) -> Result<Option<String>> {
        let context = &self.context;
        let Some(local) = local else {
            return Ok(context.active_model.clone());
        };
        let Some(recipe) = local.embedding_recipe else {
            return Ok(None);
        };
        if let Some(model) = [context.active_model.as_deref(), Some(requested)]
            .into_iter()
            .flatten()
            .find(|model| EmbeddingRecipe::for_model(model) == recipe)
        {
            return Ok(Some(model.to_owned()));
        }
        let cache = context.source.connect(&context.cache)?;
        let operation = capture.operation;
        cache.model_for_recipe(recipe, || operation.check(), |bytes| capture.reserve(bytes))
    }

    fn protect_base(
        &self,
        prepared: &PreparedLexicalPublication,
        model: &str,
        ttl: Duration,
        capture: &mut CaptureBudget<'_>,
    ) -> Result<Option<JobPinAuthority>> {
        let context = &self.context;
        let pin = prepared
            .with_base_metadata(&context.project, |db, local| {
                let Some(model) = self.base_model(local, model, capture)? else {
                    return Ok(None);
                };
                let (count, longest) = super::embedding_shape(db, context.project.id)?;
                if count == 0 {
                    return Ok(None);
                }
                // Only the hash set and one SQL text row plus its Rust copy
                // live here. The base never enters the provider pipeline.
                let bytes = (longest as u64)
                    .checked_mul(2)
                    .and_then(|text| super::pin_tracking_bytes(count)?.checked_add(text))
                    .ok_or_else(|| Error::Admission {
                        message: "active vector pin workspace overflow".into(),
                    })?;
                capture.reserve(bytes)?;
                let mut pin = JobPinAuthority::allocate(
                    &context.cache,
                    context.source.clone(),
                    model,
                    ttl,
                    count,
                )?;
                // The pin set is unordered. Projecting text through a sorted
                // scan would let SQLite retain the whole corpus in a sorter.
                let mut select = db
                    .connection()
                    .prepare(
                        "SELECT c.text FROM chunks c JOIN docs d ON d.id = c.doc_id
                     WHERE d.project_id = ?1",
                    )
                    .map_err(repo::db_error)?;
                let mut rows = select.query([context.project.id]).map_err(repo::db_error)?;
                while let Some(row) = rows.next().map_err(repo::db_error)? {
                    capture.operation.check()?;
                    let text: String = row.get(0).map_err(repo::db_error)?;
                    pin.track(&crate::vector_cache::chunk_hash(&text))?;
                }
                Ok(Some(pin))
            })?
            .flatten();
        if let Some(pin) = &pin {
            capture.operation.check()?;
            pin.renew()?;
        }
        Ok(pin)
    }

    pub(super) fn batch_packet(
        &mut self,
        operation: &SearchOperation,
        account: Option<Arc<PayloadPermit>>,
    ) -> Result<BatchPacket> {
        let context = &self.context;
        let _revision = context
            .revision
            .preparation_lease(context.active_config())?;
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
        let _revision = context
            .revision
            .preparation_lease(context.active_config())?;
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
            WriteCapacity::RequireFit,
        )
    }

    fn publish(&mut self, operation: &SearchOperation) -> Result<ObservedProjectIndex> {
        operation.check()?;
        if self
            .embedding
            .as_ref()
            .is_some_and(|embedding| !embedding.complete)
        {
            return Err(Error::internal("catalog embedding scan did not finish"));
        }
        let prepared = self
            .staged
            .take()
            .ok_or_else(|| Error::internal("catalog stage absent"))?;
        let outcome = prepared.outcome().clone();
        // Keep retiring providers alive until the staged pair has dropped its
        // native preparation lease, including any reclaim work.
        let mut retired = None;
        // The producer must know both eligible scopes before the final
        // Markdown survey. Keep filtering until the handoff or rollback.
        let mut watch_transition = (crate::index::outcome::recipe_id(self.context.active_config())
            != crate::index::outcome::recipe_id(&self.context.config))
        .then(|| {
            self.context
                .revision
                .begin_watch_config_transition(self.context.config.clone())
        });
        let recipe = self
            .context
            .stages
            .embedder
            .as_ref()
            .map(|embedder| EmbeddingRecipe::for_model(embedder.model_id()));
        let (_, selected) = prepared.publish_with_embedding(recipe, |commit| {
            let finalize = || {
                self.commit_prepared(
                    commit,
                    &outcome,
                    operation,
                    &mut retired,
                    &mut watch_transition,
                )
            };
            if let Some(update) = &self.context.refresh {
                update.with_current_sources(&self.context, operation, finalize)
            } else {
                finalize()
            }
        })?;
        Ok(selected)
    }

    fn commit_prepared(
        &self,
        commit: LexicalCommit<'_>,
        outcome: &crate::index::outcome::IndexOutcome,
        operation: &SearchOperation,
        retired: &mut Option<ProjectState>,
        watch_transition: &mut Option<crate::watch::WatchConfigTransition>,
    ) -> Result<ObservedProjectIndex> {
        operation.check()?;
        let context = &self.context;
        let verification = self
            .embedding
            .as_ref()
            .map(|embedding| {
                embedding.pin.renew()?;
                operation.check()?;
                embedding.pin.prepare_verification(embedding.dimension)
            })
            .transpose()?;
        let active = context.active_config();
        let (publication, unchanged) = if Arc::ptr_eq(active, &context.config) {
            // A first publication must not race a re-open that saw no head.
            let reload = context.revision.try_catalog_reload()?;
            (
                None,
                Some((reload, context.revision.preparation_lease(active)?)),
            )
        } else {
            (Some(context.revision.catalog_publication(active)?), None)
        };
        let result = commit.commit_with(
            || {
                operation.check()?;
                verification
                    .map(|proof| proof.hold_verified(|| operation.check()))
                    .transpose()
            },
            |conn, published| {
                let id = published
                    .generation_id
                    .ok_or_else(|| Error::internal("published catalog sync generation missing"))?;
                let mut record = serde_json::to_value(&outcome.stats).map_err(|error| {
                    Error::internal_with_source("encode catalog sync counters", error)
                })?;
                record["index_outcome"] =
                    serde_json::to_value(SyncAttribution::for_generation(outcome, id)).map_err(
                        |error| Error::internal_with_source("encode catalog sync identity", error),
                    )?;
                repo::finish_sync_job(
                    conn,
                    self.id,
                    SyncState::Done,
                    Some(&encoded_record(&record)?),
                )
            },
            || operation.check(),
            |result| {
                if let Some(publication) = &publication {
                    match result {
                        CatalogCommit::Committed => {
                            *retired = Some(
                                publication.install(context.config.clone(), context.stages.clone()),
                            );
                        }
                        CatalogCommit::Unverified(head) => {
                            if let Some(update) = &context.refresh {
                                publication.retain_unverified(
                                    head,
                                    update.watch_runtime.clone(),
                                    watch_transition.take(),
                                );
                            }
                        }
                    }
                }
            },
        );
        // Watcher handoff must follow a committed runtime even if the
        // subsequent durability barrier returned an error.
        drop(unchanged);
        let _reload = publication.map(CatalogConfigPublication::release_preparation);
        if retired.is_some()
            && let Some(update) = &context.refresh
            && let Err(error) = update.watchers.revise_with_runtime(
                &context.cache,
                context.project.id,
                update.watch_runtime.clone(),
            )
        {
            eprintln!(
                "warning: cannot revise watcher for project {}: {error}",
                context.project.id
            );
        }
        result
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
