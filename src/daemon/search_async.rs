//! Interactive orchestration; only admitted physical CPU phases enter blocking work.
use super::{Activity, Shared, bound_project, request, tools};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::search::metadata::MetadataSource;
use crate::search::operation::SearchOperation;
use serde_json::Value;
use std::sync::Arc;
#[cfg(feature = "hybrid-remote")]
use std::time::Instant;

/// Existing prepared owners shared by daemon and read-only direct search.
pub(crate) struct SearchOwners {
    pub(crate) cache: std::path::PathBuf,
    pub(crate) readers: Arc<crate::store::read_pool::ReadPool>,
    pub(crate) dense: Arc<crate::hybrid::dense::DenseCache>,
    pub(crate) workers: Arc<crate::daemon::worker_admission::WorkerAdmission>,
    pub(crate) payload_budget: crate::search::budget::PayloadBudget,
    pub(crate) policy: crate::search::settings::SearchSettings,
    pub(crate) revision: Option<Arc<super::ProjectCell>>,
    pub(crate) _daemon: Option<Arc<Shared>>,
}

pub(crate) struct SearchSession {
    owners: Arc<SearchOwners>,
    project: crate::store::models::Project,
    config: Arc<Config>,
    stages: Arc<HybridStages>,
    operation: SearchOperation,
}
#[cfg(all(test, feature = "hybrid-remote"))]
impl Drop for SearchSession {
    fn drop(&mut self) {
        test_drop::record();
    }
}
#[cfg(all(test, feature = "hybrid-remote"))]
pub(crate) mod test_drop {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static ASYNC_DROP: AtomicBool = AtomicBool::new(false);
    pub(super) fn record() {
        COUNT.fetch_add(1, Ordering::SeqCst);
        if tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread)
        {
            ASYNC_DROP.store(true, Ordering::SeqCst);
        }
    }
    pub(crate) fn snapshot() -> (usize, bool) {
        (
            COUNT.load(Ordering::SeqCst),
            ASYNC_DROP.load(Ordering::SeqCst),
        )
    }
}
impl SearchSession {
    pub(crate) fn direct(
        owners: SearchOwners,
        project: crate::store::models::Project,
        config: Arc<Config>,
        stages: Arc<HybridStages>,
        operation: SearchOperation,
    ) -> Arc<Self> {
        Arc::new(Self {
            owners: Arc::new(owners),
            project,
            config,
            stages,
            operation,
        })
    }
    fn context(&self) -> tools::SearchContext<'_> {
        tools::SearchContext {
            stages: &self.stages,
            dense: &self.owners.dense,
            cache: &self.owners.cache,
            project: &self.project,
            window: self
                .config
                .hybrid_cfg
                .reranker
                .as_ref()
                .map_or(crate::hybrid::config::DEFAULT_RERANK_WINDOW, |stage| {
                    stage.window
                }),
            ann_search: self.config.ann_search,
            payload_budget: &self.owners.payload_budget,
            operation: self.operation.clone(),
        }
    }
    fn preparation(&self) -> Result<Option<std::sync::RwLockReadGuard<'_, ()>>> {
        self.owners
            .revision
            .as_ref()
            .map(|slot| slot.search_preparation_lease(&self.config, &self.stages))
            .transpose()
    }
    fn start(&self, args: &Value) -> Result<tools::SearchStart> {
        tools::start_search(
            &self.context(),
            MetadataSource::Prepared(&self.owners.readers),
            args,
            || self.preparation(),
        )
    }
    async fn query_vector(
        self: &Arc<Self>,
        start: &tools::StartedSearch,
        activity: &Arc<Activity>,
    ) -> Result<Option<Arc<[f32]>>> {
        let captured = Arc::clone(self);
        let shared = &self.owners;
        let operation = self.operation.clone();
        let vector = if let Some(query) = start.query() {
            let embedder = captured
                .stages
                .embedder
                .clone()
                .ok_or_else(|| Error::internal("RRF requires an embedding stage"))?;
            #[cfg(feature = "hybrid-remote")]
            let _network_wait = embedder
                .async_remote()
                .map(|_| operation.shared_network_timer());
            let text: Arc<str> = Arc::from(query);
            let owner = captured.clone();
            let model = embedder.model_id().to_owned();
            let slot = shared.revision.clone();
            // Keep ingress capacity until the leader's actual physical work retires.
            let leader_activity = activity.clone();
            Some(
                shared
                    .dense
                    .query_embedding_async(
                        crate::hybrid::dense::QueryEmbeddingRequest {
                            model: &model,
                            text: query,
                            revision: crate::hybrid::dense::RuntimeRevision::Captured {
                                config: captured.config.clone(),
                                stages: captured.stages.clone(),
                            },
                            operation: &operation,
                            policy: &shared.policy,
                        },
                        |publication| {
                            let captured = captured.clone();
                            let operation = operation.clone();
                            let slot = slot.clone();
                            async move {
                                phase(&captured.owners.workers, &operation, activity, {
                                    let captured = captured.clone();
                                    let operation = operation.clone();
                                    let slot = slot.clone();
                                    move || {
                                        publication.complete(&operation, || {
                                            slot.as_ref()
                                                .map(|slot| {
                                                    slot.search_preparation_lease(
                                                        &captured.config,
                                                        &captured.stages,
                                                    )
                                                })
                                                .transpose()
                                        })
                                    }
                                })
                                .await
                            }
                        },
                        move |call| async move {
                            #[cfg(feature = "hybrid-remote")]
                            if let Some(remote) = embedder.async_remote() {
                                let call = call.with_activity(leader_activity);
                                return remote.embed_query(text, &call).await;
                            }
                            let operation = call.operation().clone();
                            phase(
                                &owner.owners.workers,
                                &operation,
                                &leader_activity,
                                move || {
                                    call.check()?;
                                    crate::hybrid::blocking::enter(|| embedder.embed_query(&text))
                                },
                            )
                            .await
                        },
                    )
                    .await?,
            )
        } else {
            None
        };
        Ok(vector)
    }

    async fn finish(
        self: &Arc<Self>,
        prepared: tools::PreparedSearch,
        activity: &Arc<Activity>,
    ) -> Result<Value> {
        let captured = Arc::clone(self);
        let shared = &self.owners;
        let operation = self.operation.clone();
        #[cfg(feature = "hybrid-remote")]
        if let Some(reranker) = captured.stages.reranker.as_ref()
            && let Some(remote) = reranker.async_remote()
        {
            let owner = captured.clone();
            let (prepared, input) = phase(&shared.workers, &operation, activity, move || {
                let input = prepared.remote_rerank_input(
                    &owner.owners.payload_budget,
                    owner
                        .config
                        .hybrid_cfg
                        .reranker
                        .as_ref()
                        .map_or(0, |stage| stage.model.len()),
                )?;
                Ok((prepared, input))
            })
            .await?;
            let started = Instant::now();
            let ranked = if let Some(input) = input {
                let call = crate::hybrid::work::RemoteCall::new(operation.clone())
                    .with_activity(activity.clone())
                    .with_payload_reservation(input.reservation);
                let top_n = input.documents.len();
                remote
                    .rerank(input.query, input.documents, top_n, &call)
                    .await?
            } else {
                Vec::new()
            };
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            return phase(&shared.workers, &operation, activity, move || {
                prepared.finish_with_rerank(ranked, elapsed)
            })
            .await;
        }
        phase(&shared.workers, &operation, activity, move || {
            prepared.finish(&captured.stages)
        })
        .await
    }
}

pub(super) async fn phase<T: Send + 'static>(
    workers: &crate::daemon::worker_admission::WorkerAdmission,
    operation: &SearchOperation,
    activity: &Arc<Activity>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = workers.search(operation).await?;
    operation.check()?;
    let started = SearchOperation::clock_now(operation);
    let worker_operation = operation.clone();
    let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
        worker_operation.record_queue(worker_operation.since(started));
        worker_operation.check()?;
        let _cpu = worker_operation.cpu_timer();
        work()
    });
    request::run(worker, Some(operation)).await
}

pub(super) async fn run(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: Value,
    operation: SearchOperation,
    activity: Option<Arc<Activity>>,
) -> Result<Value> {
    let activity = activity.unwrap_or_else(|| shared.activity.track());
    let owner = shared.clone();
    let call = operation.clone();
    let dense_activity = activity.clone();
    let (captured, start) = phase(&shared.workers, &operation, &activity, move || {
        let project = {
            let reader = MetadataSource::Prepared(&owner.readers).acquire(&call)?;
            bound_project(&owner, &reader, session)?
        };
        let (config, stages) = owner.project_runtime_for_search(&project)?;
        let owners = Arc::new(SearchOwners {
            cache: owner.cache.clone(),
            readers: owner.readers.clone(),
            dense: Arc::new(
                owner
                    .dense
                    .snapshot_with_activity(dense_activity, config.ann_build),
            ),
            workers: owner.workers.clone(),
            payload_budget: owner.payload_budget.clone(),
            policy: owner.global_config.search.clone(),
            revision: Some(owner.project_slot(project.id)),
            _daemon: Some(owner.clone()),
        });
        let captured = Arc::new(SearchSession {
            owners,
            project,
            config,
            stages,
            operation: call,
        });
        let start = captured.start(&args)?;
        Ok((captured, start))
    })
    .await?;
    continue_search(captured, start, activity).await
}

pub(crate) async fn run_direct(
    captured: Arc<SearchSession>,
    args: Value,
    activity: Arc<Activity>,
) -> Result<Value> {
    let owner = captured.clone();
    let start = phase(
        &captured.owners.workers,
        &captured.operation,
        &activity,
        move || owner.start(&args),
    )
    .await?;
    continue_search(captured, start, activity).await
}

async fn continue_search(
    captured: Arc<SearchSession>,
    start: tools::SearchStart,
    activity: Arc<Activity>,
) -> Result<Value> {
    let operation = captured.operation.clone();
    let start = match start {
        tools::SearchStart::Complete(value) => return execution_meta(value, &operation),
        tools::SearchStart::Prepared(start) => start,
    };
    let vector = captured.query_vector(&start, &activity).await?;
    let owner = captured.clone();
    let prepared = phase(&captured.owners.workers, &operation, &activity, move || {
        tools::prepare_with_query_vector(
            &owner.context(),
            MetadataSource::Prepared(&owner.owners.readers),
            &|| owner.preparation(),
            start,
            vector.as_deref(),
        )
    })
    .await?;
    let value = captured.finish(prepared, &activity).await?;
    execution_meta(value, &operation)
}
fn execution_meta(mut value: Value, operation: &SearchOperation) -> Result<Value> {
    if let Some(meta) = value.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert(
            "execution".into(),
            serde_json::to_value(operation.execution()).map_err(|error| {
                Error::internal_with_source("serialize search execution", error)
            })?,
        );
    }
    Ok(value)
}
