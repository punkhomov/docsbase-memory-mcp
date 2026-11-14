//! Interactive orchestration; only admitted physical CPU phases enter blocking work.
use super::{Activity, Shared, bound_project, request, tools};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::search::metadata::MetadataSource;
use crate::search::operation::SearchOperation;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

struct SearchSession {
    shared: Arc<Shared>,
    project: crate::store::models::Project,
    config: Arc<Config>,
    stages: Arc<HybridStages>,
    operation: SearchOperation,
}
impl SearchSession {
    fn context(&self) -> tools::SearchContext<'_> {
        tools::SearchContext {
            stages: &self.stages,
            dense: &self.shared.dense,
            cache: &self.shared.cache,
            project: &self.project,
            window: self
                .config
                .hybrid_cfg
                .reranker
                .as_ref()
                .map_or(crate::hybrid::config::DEFAULT_RERANK_WINDOW, |stage| {
                    stage.window
                }),
            payload_budget: &self.shared.payload_budget,
            operation: self.operation.clone(),
        }
    }
    async fn query_vector(
        self: &Arc<Self>,
        start: &tools::StartedSearch,
        activity: &Arc<Activity>,
    ) -> Result<Option<Arc<[f32]>>> {
        let captured = Arc::clone(self);
        let shared = &self.shared;
        let operation = self.operation.clone();
        let vector = if let Some(query) = start.query() {
            let embedder = captured
                .stages
                .embedder
                .clone()
                .ok_or_else(|| Error::internal("RRF requires an embedding stage"))?;
            let text: Arc<str> = Arc::from(query);
            let owner = captured.clone();
            let model = embedder.model_id().to_owned();
            let slot = shared.project_slot(captured.project.id);
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
                            policy: &shared.global_config.search,
                        },
                        || slot.search_preparation_lease(&captured.config, &captured.stages),
                        move |call| async move {
                            #[cfg(feature = "hybrid-remote")]
                            if let Some(remote) = embedder.async_remote() {
                                let call = call.with_activity(leader_activity);
                                return remote.embed_query(text, &call).await;
                            }
                            let operation = call.operation().clone();
                            phase(&owner.shared, &operation, &leader_activity, move || {
                                call.check()?;
                                crate::hybrid::blocking::enter(|| embedder.embed_query(&text))
                            })
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
        let shared = &self.shared;
        let operation = self.operation.clone();
        #[cfg(feature = "hybrid-remote")]
        if let Some(reranker) = captured.stages.reranker.as_ref()
            && let Some(remote) = reranker.async_remote()
        {
            let owner = captured.clone();
            let (prepared, input) = phase(shared, &operation, activity, move || {
                let input = prepared.remote_rerank_input(
                    &owner.shared.payload_budget,
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
            return phase(shared, &operation, activity, move || {
                prepared.finish_with_rerank(ranked, elapsed)
            })
            .await;
        }
        phase(shared, &operation, activity, move || {
            prepared.finish(&captured.stages)
        })
        .await
    }
}

async fn phase<T: Send + 'static>(
    shared: &Arc<Shared>,
    operation: &SearchOperation,
    activity: &Arc<Activity>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = shared.workers.search(operation).await?;
    operation.check()?;
    let started = Instant::now();
    let worker_operation = operation.clone();
    let worker = request::spawn(Some(permit), Some(activity.clone()), move || {
        worker_operation.record_queue(started.elapsed());
        worker_operation.check()?;
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
    let (captured, start) = phase(shared, &operation, &activity, move || {
        let project = {
            let reader = MetadataSource::Prepared(&owner.readers).acquire(&call)?;
            bound_project(&owner, &reader, session)?
        };
        let (config, stages) = owner.project_runtime_for_search(&project)?;
        let captured = Arc::new(SearchSession {
            shared: owner,
            project,
            config,
            stages,
            operation: call,
        });
        let slot = captured.shared.project_slot(captured.project.id);
        let start = tools::start_search(
            &captured.context(),
            MetadataSource::Prepared(&captured.shared.readers),
            &args,
            || slot.search_preparation_lease(&captured.config, &captured.stages),
        )?;
        Ok((captured, start))
    })
    .await?;
    let start = match start {
        tools::SearchStart::Complete(value) => return Ok(value),
        tools::SearchStart::Prepared(start) => start,
    };
    let vector = captured.query_vector(&start, &activity).await?;
    let owner = captured.clone();
    let prepared = phase(shared, &operation, &activity, move || {
        let slot = owner.shared.project_slot(owner.project.id);
        tools::prepare_with_query_vector(
            &owner.context(),
            MetadataSource::Prepared(&owner.shared.readers),
            &|| slot.search_preparation_lease(&owner.config, &owner.stages),
            start,
            vector.as_deref(),
        )
    })
    .await?;
    captured.finish(prepared, &activity).await
}
