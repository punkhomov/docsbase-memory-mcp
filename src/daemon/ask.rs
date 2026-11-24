//! Composition of the documentation harness with existing daemon owners.
use super::{Activity, Shared, bound_project, search_async, tools};
use crate::ask::{
    AskLimits, AskRequest,
    runner::{self, DocumentTools},
};
use crate::error::{Error, Result};
use crate::hybrid::remote::{RemoteCall, StageFuture};
use crate::search::{metadata::MetadataSource, operation::SearchOperation};
use serde_json::{Value, json};
use std::sync::Arc;

struct Reads {
    shared: Arc<Shared>,
    session: Option<u64>,
    project: crate::store::models::Project,
    config: crate::config::Config,
    operation: SearchOperation,
    activity: Arc<Activity>,
    // Holds the same reservation through physical read closures on cancellation.
    reservation: Arc<crate::search::budget::PayloadPermit>,
}
impl DocumentTools for Reads {
    fn call<'a>(&'a self, name: &'a str, mut args: Value) -> StageFuture<'a, Value> {
        Box::pin(async move {
            let object = args
                .as_object_mut()
                .ok_or_else(|| Error::internal("read arguments must be an object"))?;
            if name == "search_docs" {
                object.insert("query_mode".into(), json!("literal"));
                object.insert("limit".into(), json!(AskLimits::SEARCH_HITS));
                object.insert("snippet".into(), json!(true));
                object.insert("dedupe".into(), json!("path"));
                object.insert("n_per_doc".into(), json!(1));
                object.insert("explain".into(), json!(false));
                return search_async::run(
                    &self.shared,
                    self.session,
                    args,
                    self.operation.clone(),
                    Some(self.activity.clone()),
                )
                .await;
            }
            if name == "read_neighbors" {
                for key in ["before", "after"] {
                    let count = match object.get(key) {
                        None => AskLimits::NEIGHBORS,
                        Some(value) => value
                            .as_u64()
                            .filter(|v| *v <= AskLimits::NEIGHBORS)
                            .ok_or_else(|| Error::Admission {
                                message: "ask_docs reads at most one neighbor per side".into(),
                            })?,
                    };
                    object.insert(key.into(), json!(count));
                }
            }
            let project = self.project.clone();
            let config = self.config.clone();
            let shared = self.shared.clone();
            let operation = self.operation.clone();
            let name = name.to_owned();
            let reservation = self.reservation.clone();
            search_async::phase(
                &self.shared.workers,
                &self.operation,
                &self.activity,
                move || {
                    let _reservation = reservation;
                    if name == "get_doc" {
                        tools::get_doc(&project, &config, &args)
                    } else if name == "read_neighbors" {
                        let reader =
                            MetadataSource::Prepared(&shared.readers).acquire(&operation)?;
                        tools::read_neighbors_bounded(
                            &reader,
                            &project,
                            &args,
                            Some(AskLimits::TOOL_BYTES),
                        )
                    } else {
                        Err(Error::Admission {
                            message: "unsupported harness read".into(),
                        })
                    }
                },
            )
            .await
        })
    }
}

pub(super) async fn run(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: Value,
    operation: SearchOperation,
    activity: Option<Arc<Activity>>,
) -> Result<Value> {
    run_with_factory(
        shared,
        session,
        args,
        operation,
        activity,
        |model, config, execution| {
            let address = crate::hybrid::ModelAddress::parse(model)?;
            crate::hybrid::provider(address.provider())?.build_generator(
                address.model(),
                &config.providers,
                execution,
            )
        },
    )
    .await
}

async fn run_with_factory(
    shared: &Arc<Shared>,
    session: Option<u64>,
    args: Value,
    operation: SearchOperation,
    activity: Option<Arc<Activity>>,
    factory: impl FnOnce(
        &str,
        &crate::config::Config,
        Arc<crate::hybrid::remote::RemoteExecution>,
    ) -> Result<Arc<dyn crate::hybrid::generation::Generator>>
    + Send
    + 'static,
) -> Result<Value> {
    if shared.website.is_some() {
        return Err(Shared::profile_denial());
    }
    let request = AskRequest::parse(args)?;
    let activity = activity.unwrap_or_else(|| shared.activity.track());
    let reservation = Arc::new(shared.payload_budget.reserve(AskLimits::RETAINED_BYTES)?);
    let owner = shared.clone();
    let captured = operation.clone();
    let model = request.model.clone();
    let (project, mut config, generator) =
        search_async::phase(&shared.workers, &operation, &activity, move || {
            let project = {
                let reader = MetadataSource::Prepared(&owner.readers).acquire(&captured)?;
                bound_project(&owner, &reader, session)?
            };
            let config = owner.project_config(&project)?;
            let generator = factory(&model, &config, owner.remote.clone())?;
            Ok((project, (*config).clone(), generator))
        })
        .await?;
    // Refuse oversized whole-file reads; the model can use existing chunk reads.
    config.max_file_size = config.max_file_size.min(AskLimits::TOOL_BYTES as u64);
    let reads = Reads {
        shared: shared.clone(),
        session,
        project,
        config,
        operation: operation.clone(),
        activity: activity.clone(),
        reservation: reservation.clone(),
    };
    let call = RemoteCall::new(operation)
        .with_activity(activity)
        .with_payload_reservation(reservation);
    runner::run(request, generator.as_ref(), &reads, &call).await
}

#[cfg(test)]
#[path = "ask_tests.rs"]
mod tests;
