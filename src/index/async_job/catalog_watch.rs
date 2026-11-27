//! Prepare embeddings out of band before mutating a selected catalog revision.

use std::path::PathBuf;
use std::sync::{Arc, RwLockReadGuard};
use std::time::Duration;

use super::{
    Activity, BatchPacket, JobService, PayloadBudget, PayloadPermit, SearchOperation,
    WorkerAdmission,
};
use crate::config::schema::Resolved;
use crate::daemon::server::ProjectCell;
use crate::error::{Error, Result};
use crate::hybrid::async_jobs::{self, EmbeddingBatch};
use crate::index::job::{CaptureBudget, JobInput, JobStats};
use crate::store::{
    generation_build::open_index_registry, models::Project, project_index::PreparedWatch,
    settings::DbSettings,
};
use crate::vector_cache::{EmbeddingRecipe, JobPinAuthority, WriteCapacity, chunk_hash};
use crate::watch::{WatchPayload, WatchRuntime};

pub(crate) struct Context {
    pub(crate) cache: PathBuf,
    pub(crate) epoch: [u8; 16],
    pub(crate) settings: Resolved<DbSettings>,
    pub(crate) project: Project,
    pub(crate) revision: Arc<ProjectCell>,
    pub(crate) runtime: Arc<WatchRuntime>,
    pub(crate) paths: Arc<WatchPayload>,
    pub(crate) pin_ttl: Duration,
}

impl Context {
    fn admit(&self) -> Result<RwLockReadGuard<'_, ()>> {
        self.revision
            .search_preparation_lease(&self.runtime.config, &self.runtime.stages)
            .map_err(|error| {
                Error::internal_with_source("retry watcher with the current catalog runtime", error)
            })
    }

    fn model(&self) -> Option<&str> {
        self.runtime
            .stages
            .embedder
            .as_ref()
            .map(|embedder| embedder.model_id())
    }
}

pub(super) struct CatalogWatch {
    // Drop candidate and active pins before releasing project preparation.
    embedding: Option<WatchEmbedding>,
    protected_base: Option<JobPinAuthority>,
    prepared: PreparedWatch,
    context: Context,
}

struct WatchEmbedding {
    cursor: usize,
    pin: JobPinAuthority,
    dimension: usize,
    ttl: Duration,
    complete: bool,
}

pub(crate) async fn run(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    operation: &SearchOperation,
    budget: &PayloadBudget,
    context: Context,
) -> Result<JobStats> {
    let account = context.paths.account.clone();
    let owner = service
        .admit_with_account(
            workers.clone(),
            activity.clone(),
            operation,
            budget,
            Some(account),
        )
        .await?;
    let call = operation.clone();
    let (owner, captured) = service
        .phase(workers, activity.clone(), operation, owner, move |state| {
            let account = state
                .account
                .as_deref()
                .ok_or_else(|| Error::internal("watcher payload account missing"))?;
            let job = CatalogWatch::capture(context, &call, account)?;
            let embeds = job.embedding.is_some();
            state.catalog_watch = Some(job);
            Ok(embeds)
        })
        .await?;
    let embeds = **captured;
    drop(captured);
    let owner = if embeds {
        super::embed_batches(service, workers, &activity, operation, owner, false).await?
    } else {
        owner
    };
    let call = operation.clone();
    let (_owner, published) = service
        .phase(workers, activity, operation, owner, move |state| {
            state
                .catalog_watch
                .take()
                .ok_or_else(|| Error::internal("catalog watcher job missing"))?
                .publish(&call)
        })
        .await?;
    Ok((**published).clone())
}

impl CatalogWatch {
    fn capture(
        context: Context,
        operation: &SearchOperation,
        account: &PayloadPermit,
    ) -> Result<Self> {
        operation.check()?;
        let registry = open_index_registry(&context.cache, &context.settings, context.epoch)?;
        let mut capture = CaptureBudget {
            account,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let input = JobInput::Watch(context.paths.clone());
        let prepared = PreparedWatch::capture(
            &registry,
            &context.project,
            &context.runtime.config,
            &input,
            context.model(),
            &mut capture,
            || context.admit(),
        )?;
        registry.close()?;
        let embedding = Self::prepare_embedding(&context, &prepared, &mut capture)?;
        Ok(Self {
            embedding,
            protected_base: None,
            prepared,
            context,
        })
    }

    fn prepare_embedding(
        context: &Context,
        prepared: &PreparedWatch,
        capture: &mut CaptureBudget<'_>,
    ) -> Result<Option<WatchEmbedding>> {
        let Some(embedder) = &context.runtime.stages.embedder else {
            return Ok(None);
        };
        let count = prepared.plan().chunk_count();
        if count == 0 {
            return Ok(None);
        }
        let model = embedder.model_id();
        if model.len() as u64 > EmbeddingRecipe::MAX_MODEL_BYTES {
            return Err(Error::Admission {
                message: "embedding model key exceeds the catalog identity bound".into(),
            });
        }
        let config = &context.runtime.config;
        let dimension = super::embedding_dimension(config, embedder.as_ref())?;
        let peak = prepared.plan().embedding_batch_peak(
            config.remote.embed_batch_texts,
            config.remote.embed_batch_bytes,
        )?;
        capture.reserve(super::embedding_workspace(
            &config.remote,
            0,
            peak,
            count,
            dimension,
        )?)?;
        let ttl = context.pin_ttl.max(
            capture
                .operation
                .remaining()?
                .saturating_add(Duration::from_secs(1)),
        );
        let pin = JobPinAuthority::allocate(
            &context.cache,
            context.runtime.source.clone(),
            model.to_owned(),
            ttl,
            count,
        )?
        .without_release_eviction();
        Ok(Some(WatchEmbedding {
            cursor: 0,
            pin,
            dimension,
            ttl,
            complete: false,
        }))
    }

    pub(super) fn batch_packet(
        &mut self,
        operation: &SearchOperation,
        account: Option<Arc<PayloadPermit>>,
    ) -> Result<BatchPacket> {
        let context = &self.context;
        let _revision = context.admit()?;
        operation.check()?;
        let embedder = context
            .runtime
            .stages
            .embedder
            .as_ref()
            .ok_or_else(|| Error::internal("catalog watcher embedder missing"))?;
        let embedding = self
            .embedding
            .as_mut()
            .ok_or_else(|| Error::internal("catalog watcher embedding state missing"))?;
        let policy = &context.runtime.config.remote;
        let texts = self.prepared.plan().next_texts(
            &mut embedding.cursor,
            policy.embed_batch_texts,
            policy.embed_batch_bytes,
        );
        if texts.is_empty() {
            embedding.complete = true;
            return Ok((None, None, account));
        }
        for text in &texts {
            embedding.pin.track(&chunk_hash(text))?;
        }
        embedding.pin.check_capacity(embedding.dimension)?;
        embedding.pin.renew()?;
        let batch = async_jobs::missing(
            &context.runtime.source,
            &context.cache,
            embedder.model_id(),
            texts,
            Some(embedding.dimension),
        )?;
        Ok((Some(batch), Some(embedder.clone()), account))
    }

    pub(super) fn store(
        &mut self,
        batch: &EmbeddingBatch,
        vectors: &[Vec<f32>],
        operation: &SearchOperation,
        account: &PayloadPermit,
    ) -> Result<()> {
        let context = &self.context;
        let _revision = context.admit()?;
        let model = context
            .model()
            .ok_or_else(|| Error::internal("catalog watcher embedder missing"))?;
        let embedding = self
            .embedding
            .as_ref()
            .ok_or_else(|| Error::internal("catalog watcher embedding state missing"))?;
        embedding.pin.renew()?;
        let source = &context.runtime.source;
        if self.protected_base.is_none() {
            if async_jobs::try_store_without_eviction(
                source,
                &context.cache,
                model,
                batch,
                vectors,
                embedding.dimension,
                operation,
            )? {
                return Ok(());
            }
            self.protected_base = self.protect_base(model, embedding.ttl, operation, account)?;
        }
        operation.check()?;
        async_jobs::store(
            source,
            &context.cache,
            model,
            batch,
            vectors,
            embedding.dimension,
            WriteCapacity::RequireFit,
        )
    }

    fn protect_base(
        &self,
        model: &str,
        ttl: Duration,
        operation: &SearchOperation,
        account: &PayloadPermit,
    ) -> Result<Option<JobPinAuthority>> {
        let context = &self.context;
        let registry = open_index_registry(&context.cache, &context.settings, context.epoch)?;
        let mut capture = CaptureBudget {
            account,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let pin = self
            .prepared
            .with_base_metadata(&registry, &context.project, |db| {
                super::catalog_sync::capture_base_pin(
                    db,
                    context.project.id,
                    &context.runtime.source,
                    model.to_owned(),
                    ttl,
                    &mut capture,
                )
            })?;
        registry.close()?;
        if let Some(pin) = &pin {
            operation.check()?;
            pin.renew()?;
        }
        Ok(pin)
    }

    fn publish(&mut self, operation: &SearchOperation) -> Result<JobStats> {
        operation.check()?;
        let context = &self.context;
        let verification = self
            .embedding
            .as_ref()
            .map(|embedding| {
                if !embedding.complete {
                    return Err(Error::internal(
                        "catalog watcher embedding scan did not finish",
                    ));
                }
                embedding.pin.renew()?;
                embedding.pin.prepare_verification(embedding.dimension)
            })
            .transpose()?;
        let registry = open_index_registry(&context.cache, &context.settings, context.epoch)?;
        self.prepared.publish(
            &registry,
            &context.project,
            &context.runtime.config,
            context.model(),
            || {
                let guard = context.admit()?;
                operation.check()?;
                Ok(guard)
            },
            || {
                verification
                    .map(|proof| proof.hold_verified(|| operation.check()))
                    .transpose()
            },
        )
    }
}
