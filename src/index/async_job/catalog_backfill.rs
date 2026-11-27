//! Cache-only repair of an exact catalog selection on project open.

use crate::daemon::{job_service::JobReservation, server::ProjectCell};
use crate::error::{Error, Result};
use crate::hybrid::{
    HybridStages,
    async_jobs::{self, EmbeddingBatch},
};
use crate::index::job::CaptureBudget;
use crate::search::budget::PayloadBudget;
use crate::store::{
    Db,
    project_index::{ProjectIndex, require_catalog_embedding, with_catalog_pair},
    read_pool::ReadPool,
};
use crate::vector_cache::{JobPinAuthority, VectorCacheSource, WriteCapacity};
use std::{sync::Arc, time::Duration};
use tokio::sync::OwnedSemaphorePermit;

use super::{
    Activity, BatchPacket, Config, JobService, PayloadPermit, Project, SearchOperation,
    WorkerAdmission,
};

pub(crate) struct Context {
    pub(crate) project: Project,
    pub(crate) readers: Arc<ReadPool>,
    pub(crate) revision: Arc<ProjectCell>,
    pub(crate) config: Arc<Config>,
    pub(crate) stages: Arc<HybridStages>,
    pub(crate) source: VectorCacheSource,
    pub(crate) pin_ttl: Duration,
    pub(crate) _singleton: OwnedSemaphorePermit,
}

pub(super) struct CatalogBackfill {
    protected: Option<JobPinAuthority>,
    context: Context,
    index: ProjectIndex,
    dimension: usize,
    cursor: (i64, i64),
}

pub(crate) async fn run(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    budget: &PayloadBudget,
    context: Context,
    reservation: JobReservation,
) -> Result<()> {
    let operation = reservation.operation();
    let owner = service
        .admit_reserved(workers.clone(), activity.clone(), budget, reservation)
        .await?;
    let call = operation.clone();
    let (owner, ready) = service
        .phase(workers, activity.clone(), &operation, owner, move |state| {
            let account = state
                .account
                .as_deref()
                .ok_or_else(|| Error::internal("catalog backfill payload account missing"))?;
            state.catalog_backfill = CatalogBackfill::capture(context, &call, account)?;
            Ok(state.catalog_backfill.is_some())
        })
        .await?;
    let run = **ready;
    drop(ready);
    if run {
        drop(super::embed_batches(service, workers, &activity, &operation, owner, false).await?);
    }
    Ok(())
}

impl Context {
    fn read<T>(
        &self,
        operation: &SearchOperation,
        read: impl FnOnce(&Db, ProjectIndex) -> Result<T>,
    ) -> Result<T> {
        operation.check()?;
        let _runtime = self
            .revision
            .search_preparation_lease(&self.config, &self.stages)?;
        let reader = self.readers.try_acquire()?;
        with_catalog_pair(reader, &self.project, |registry, local, index| {
            require_catalog_embedding(local.as_ref(), self.model()?)?;
            read(local.as_ref().map_or(&*registry, |local| &local.db), index)
        })
    }

    fn model(&self) -> Result<&str> {
        self.stages
            .embedder
            .as_ref()
            .map(|stage| stage.model_id())
            .ok_or_else(|| Error::internal("catalog backfill embedder missing"))
    }
}

impl CatalogBackfill {
    fn capture(
        context: Context,
        operation: &SearchOperation,
        account: &PayloadPermit,
    ) -> Result<Option<Self>> {
        let Some(embedder) = context.stages.embedder.as_ref() else {
            return Ok(None);
        };
        let dimension = super::embedding_dimension(&context.config, embedder.as_ref())?;
        let (index, count, longest) = context.read(operation, |db, index| {
            let (count, longest) = super::embedding_shape(db, context.project.id)?;
            Ok((index, count, longest))
        })?;
        if count == 0 {
            return Ok(None);
        }
        CaptureBudget {
            account,
            operation,
            #[cfg(test)]
            observed: None,
        }
        .reserve(super::embedding_workspace(
            &context.config.remote,
            longest,
            0,
            0,
            dimension,
        )?)?;
        Ok(Some(Self {
            protected: None,
            context,
            index,
            dimension,
            cursor: (0, -1),
        }))
    }

    pub(super) fn batch_packet(
        &mut self,
        operation: &SearchOperation,
        account: Option<Arc<PayloadPermit>>,
    ) -> Result<BatchPacket> {
        let context = &self.context;
        let texts = context.read(operation, |db, index| {
            check_selected(self.index, index)?;
            super::embedding_texts(
                db,
                context.project.id,
                &mut self.cursor,
                &context.config.remote,
                operation,
            )
        })?;
        if texts.is_empty() {
            return Ok((None, None, account));
        }
        let batch = async_jobs::missing_without_touch(
            &context.source,
            context.readers.cache_root(),
            context.model()?,
            texts,
            self.dimension,
        )?;
        Ok((Some(batch), context.stages.embedder.clone(), account))
    }

    pub(super) fn store(
        &mut self,
        batch: &EmbeddingBatch,
        vectors: &[Vec<f32>],
        operation: &SearchOperation,
        account: &PayloadPermit,
    ) -> Result<()> {
        let context = &self.context;
        let root = context.readers.cache_root();
        let model = context.model()?;
        context.read(operation, |db, index| {
            check_selected(self.index, index)?;
            if self.protected.is_none() {
                if async_jobs::try_store_without_eviction(
                    &context.source,
                    root,
                    model,
                    batch,
                    vectors,
                    self.dimension,
                    operation,
                )? {
                    return Ok(());
                }
                let mut capture = CaptureBudget {
                    account,
                    operation,
                    #[cfg(test)]
                    observed: None,
                };
                let ttl = context.pin_ttl.max(
                    operation
                        .remaining()?
                        .saturating_add(Duration::from_secs(1)),
                );
                let protected = super::catalog_sync::capture_base_pin(
                    db,
                    context.project.id,
                    &context.source,
                    model.to_owned(),
                    ttl,
                    &mut capture,
                )?;
                if let Some(pin) = &protected {
                    pin.check_capacity(self.dimension)?;
                    // The lease covers the entire remaining absolute deadline.
                    pin.renew()?;
                }
                self.protected = protected;
            }
            operation.check()?;
            async_jobs::store(
                &context.source,
                root,
                model,
                batch,
                vectors,
                self.dimension,
                WriteCapacity::RequireFit,
            )
        })
    }
}

fn check_selected(expected: ProjectIndex, actual: ProjectIndex) -> Result<()> {
    if actual != expected {
        return Err(Error::Admission {
            message: "selected catalog changed during vector backfill".into(),
        });
    }
    Ok(())
}
