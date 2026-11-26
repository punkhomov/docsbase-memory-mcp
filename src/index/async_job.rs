//! Resumable lexical jobs. Native state is unique and only accessed by admitted phases.
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;

use super::{
    job::{self, PreparedPlan},
    outcome::{self, SurveyScope},
    tantivy_index::IndexHandle,
};
use crate::config::Config;
use crate::daemon::{
    job_service::{JobOwner, JobService},
    lifecycle, registry,
    request::Activity,
    worker_admission::WorkerAdmission,
};
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::search::{
    budget::{PayloadBudget, PayloadPermit},
    operation::SearchOperation,
};
use crate::store::{
    Db,
    models::{Project, ProjectStatus, SyncState},
    repo,
};
use crate::vector_cache::VectorCacheSource;

use crate::platform::file_lease::ExclusiveFileLease;

fn acquire_writer_lease(cache: &std::path::Path, project: i64) -> Result<ExclusiveFileLease> {
    let path = lifecycle::lease_path(cache, project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::internal_with_source("create writer lease directory", e))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| Error::internal_with_source("open writer lease", e))?;
    ExclusiveFileLease::try_acquire(file).map_err(|e| Error::Project {
        message: format!("another writer holds {}: {e}", path.display()),
        instruction: Some("wait for the running index to finish".into()),
    })
}

pub(crate) mod catalog_sync;

pub(crate) use super::job::JobInput;

pub(crate) fn active_sync_conflict() -> Error {
    Error::Admission {
        message: "index requires a new job to use current project settings; wait for the active sync to finish and retry `docsbase index`".to_owned(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mutation {
    Untouched,
    Started,
    LexicalCommitted,
}

pub(crate) struct JobContext {
    pub(crate) input: JobInput<'static>,
    pub(crate) singleton: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(crate) cache: PathBuf,
    pub(crate) db_settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    pub(crate) project: Project,
    pub(crate) config: Arc<Config>,
    pub(crate) stages: Arc<HybridStages>,
    pub(crate) source: VectorCacheSource,
    pub(crate) revision: Option<Arc<crate::daemon::server::ProjectCell>>,
    pub(crate) dense: Option<Arc<crate::hybrid::dense::DenseCache>>,
}

// Explicit native/data before guards: also preserves ordering if terminal publication unwinds.
pub(crate) struct IndexState {
    catalog_sync: Option<catalog_sync::CatalogSync>,
    index: Option<IndexHandle>,
    #[cfg(test)]
    retirement_observer: Option<crate::daemon::job_service::RetirementObserver>,
    db: Option<Db>,
    plan: Option<PreparedPlan>,
    pin: Option<crate::vector_cache::JobPinAuthority>,
    context: Option<JobContext>,
    record: serde_json::Value,
    operation: Option<SearchOperation>,
    scope: SurveyScope,
    error: Option<String>,
    reply: Option<oneshot::Sender<Result<serde_json::Value>>>,
    sync_id: Option<i64>,
    owns_sync: bool,
    mutation: Mutation,
    complete: bool,
    attempt: Option<i64>,
    plan_cursor: usize,
    backfill_cursor: (i64, i64),
    generation: Option<i64>,
    dimension: usize,
    account: Option<Arc<PayloadPermit>>,
    retained: Option<Arc<PayloadPermit>>,
    lease: Option<ExclusiveFileLease>,
    singleton: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl IndexState {
    pub(crate) fn empty(account: Arc<PayloadPermit>) -> Self {
        Self {
            catalog_sync: None,
            index: None,
            #[cfg(test)]
            retirement_observer: None,
            db: None,
            plan: None,
            pin: None,
            context: None,
            account: Some(account),
            retained: None,
            lease: None,
            singleton: None,
            sync_id: None,
            owns_sync: false,
            mutation: Mutation::Untouched,
            complete: false,
            attempt: None,
            plan_cursor: 0,
            backfill_cursor: (0, -1),
            generation: None,
            dimension: 0,
            record: serde_json::json!({}),
            operation: None,
            scope: SurveyScope::FullSurvey,
            error: None,
            reply: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn set_retirement_observer(
        &mut self,
        observer: Option<crate::daemon::job_service::RetirementObserver>,
    ) {
        self.retirement_observer = observer;
    }
    pub(crate) fn fail(&mut self, error: String) {
        self.error = Some(error);
    }
    fn prepare(
        &mut self,
        context: JobContext,
        reply: oneshot::Sender<Result<serde_json::Value>>,
        operation: &SearchOperation,
    ) -> Result<bool> {
        self.reply = Some(reply);
        self.operation = Some(operation.clone());
        let mut context = context;
        self.singleton = context.singleton.take();
        self.context = Some(context);
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| Error::internal("job context absent"))?;
        context.source.validate_root(&context.cache)?;
        let revision = context.revision.clone();
        let config = context.config.clone();
        let explicit_index = matches!(context.input, JobInput::Sync { join_active: false });
        let _revision = revision
            .as_ref()
            .map(|slot| {
                slot.preparation_lease(&config).map_err(|error| {
                    if explicit_index {
                        Error::Admission {
                            message: "project settings changed during index preparation; wait for any active sync to finish and retry `docsbase index`".to_owned(),
                        }
                    } else {
                        error
                    }
                })
            })
            .transpose()?;
        self.db = Some(Db::open_with_settings(
            &context.cache,
            &context.db_settings,
        )?);
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| Error::internal("job database absent"))?;
        // Only explicit Sync owns a durable job row. Direct/full/incremental
        // lexical callers preserve their existing no-Sync-row contract.
        if let JobInput::Sync { join_active } = context.input {
            let tx = db.begin_write()?;
            if let Some(existing) = repo::active_sync_job(&tx, context.project.id)? {
                if !join_active {
                    return Err(active_sync_conflict());
                }
                self.sync_id = Some(existing.id);
                drop(tx);
                self.complete = true;
                return Ok(false);
            }
            operation.check()?;
            let id = repo::create_sync_job(&tx, context.project.id)?
                .ok_or_else(|| Error::internal("sync dedup changed within write transaction"))?;
            self.sync_id = Some(id);
            if let Err(error) = tx.commit() {
                self.sync_id = None;
                return Err(crate::store::repo::db_error(error));
            }
            self.owns_sync = true;
            if let Err(error) = repo::prune_sync_jobs(db.connection(), context.project.id) {
                eprintln!("warning: cannot prune sync jobs: {error}");
            }
        }
        operation.check()?;
        if !self.capture_plan(operation)? {
            self.complete = true;
            return Ok(false);
        }
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| Error::internal("job context absent"))?;
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| Error::internal("job database absent"))?;
        operation.check()?;
        if let Some(id) = self.sync_id {
            repo::mark_sync_running(db.connection(), id)?;
        }
        self.mutation = Mutation::Started;
        // Watch preserves legacy incremental status during begin/error/cancel;
        // Other jobs keep their existing explicit index status transitions.
        if !matches!(context.input, JobInput::Watch(_)) {
            registry::set_status(db, context.project.id, ProjectStatus::Indexing)?;
        }
        self.attempt = Some(outcome::begin(db, &context.project)?);
        self.index = Some(IndexHandle::open_or_create_with_settings(
            &lifecycle::index_dir(&context.cache, context.project.id),
            context.config.index_writer,
        )?);
        self.plan
            .as_mut()
            .ok_or_else(|| Error::internal("job plan absent"))?
            .begin(
                db,
                self.index
                    .as_mut()
                    .ok_or_else(|| Error::internal("job index absent"))?,
                context.project.id,
            )?;
        Ok(true)
    }
    fn capture_plan(&mut self, operation: &SearchOperation) -> Result<bool> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| Error::internal("job context absent"))?;
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| Error::internal("job database absent"))?;
        self.lease = Some(acquire_writer_lease(&context.cache, context.project.id)?);
        // Re-read after writer admission, before capture/index/marker effects.
        if matches!(context.input, JobInput::Watch(_)) {
            crate::watch::admit_incremental(db, &context.project)?;
        }
        let recreated =
            IndexHandle::needs_rebuild(&lifecycle::index_dir(&context.cache, context.project.id));
        let mut capture = job::CaptureBudget {
            account: self
                .account
                .as_deref()
                .ok_or_else(|| Error::internal("job capture account absent"))?,
            operation,
            #[cfg(test)]
            observed: None,
        };
        let (plan, scope) = job::capture_input(
            db,
            &context.project,
            &context.config,
            recreated,
            &context.input,
            Some(&mut capture),
        )?;
        self.plan = Some(plan);
        let Some(scope) = scope else {
            return Ok(false);
        };
        self.scope = scope;
        if let Some(embedder) = &context.stages.embedder {
            self.dimension = embedding_dimension(&context.config, embedder.as_ref())?;
            let (rows, longest) = embedding_shape(db, context.project.id)?;
            let count = self
                .plan
                .as_ref()
                .and_then(|plan| rows.checked_add(plan.chunk_count()))
                .ok_or_else(|| Error::Admission {
                    message: "job hash tracking count overflow".into(),
                })?;
            let policy = &context.config.remote;
            let captured_peak = self
                .plan
                .as_ref()
                .ok_or_else(|| Error::internal("job plan absent"))?
                .embedding_batch_peak(policy.embed_batch_texts, policy.embed_batch_bytes)?;
            let workspace =
                embedding_workspace(policy, longest, captured_peak, count, self.dimension)?;
            capture.reserve(workspace)?;
            if context.stages.ranking == crate::hybrid::config::Ranking::Rrf
                && context.dense.as_ref().is_some_and(|dense| {
                    dense
                        .limits()
                        .ann_build_eligible_for_model(count, embedder.model_id())
                })
            {
                self.pin = Some(crate::vector_cache::JobPinAuthority::allocate(
                    &context.cache,
                    context.source.clone(),
                    embedder.model_id().to_owned(),
                    context
                        .dense
                        .as_ref()
                        .ok_or_else(|| Error::internal("dense owner absent"))?
                        .limits()
                        .ann_lifetimes
                        .backfill_pin_ttl(),
                    count,
                )?);
            }
        }
        self.retained = self.account.take();
        Ok(true)
    }
    fn commit(&mut self, operation: &SearchOperation) -> Result<bool> {
        operation.check()?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| Error::internal("job context absent"))?;
        let _revision = context
            .revision
            .as_ref()
            .map(|slot| slot.preparation_lease(&context.config))
            .transpose()?;
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| Error::internal("job database absent"))?;
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| Error::internal("job plan absent"))?;
        plan.commit(
            db,
            self.index
                .as_mut()
                .ok_or_else(|| Error::internal("job index absent"))?,
            context.project.id,
        )?;
        operation.check()?;
        let stats = plan.stats.clone();
        outcome::publish(
            db,
            &context.project,
            &context.config,
            self.scope,
            self.attempt
                .ok_or_else(|| Error::internal("job attempt absent"))?,
            &stats,
        )?;
        if let Some(id) = self.sync_id {
            crate::daemon::tools::associate_sync_index(db, id, context.project.id, &stats)?;
            self.record = crate::daemon::tools::retained_sync_record(db, id)?;
        }
        self.generation = Some(repo::index_generation(db.connection(), context.project.id)?);
        self.mutation = Mutation::LexicalCommitted;
        Ok(matches!(context.input, JobInput::Sync { .. }))
    }
    fn batch(
        &mut self,
        backfill: bool,
        operation: &SearchOperation,
    ) -> Result<Option<crate::hybrid::async_jobs::EmbeddingBatch>> {
        operation.check()?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| Error::internal("job context absent"))?;
        let Some(embedder) = &context.stages.embedder else {
            return Ok(None);
        };
        let _revision = context
            .revision
            .as_ref()
            .map(|slot| slot.preparation_lease(&context.config))
            .transpose()?;
        let policy = &context.config.remote;
        let texts = if backfill {
            let db = self
                .db
                .as_ref()
                .ok_or_else(|| Error::internal("job database absent"))?;
            if Some(repo::index_generation(db.connection(), context.project.id)?) != self.generation
            {
                return Err(Error::Admission {
                    message: "project generation changed before backfill".into(),
                });
            }
            embedding_texts(
                db,
                context.project.id,
                &mut self.backfill_cursor,
                policy,
                operation,
            )?
        } else {
            self.plan
                .as_ref()
                .ok_or_else(|| Error::internal("job plan absent"))?
                .next_texts(
                    &mut self.plan_cursor,
                    policy.embed_batch_texts,
                    policy.embed_batch_bytes,
                )
        };
        if texts.is_empty() {
            return Ok(None);
        }
        if let Some(pin) = &mut self.pin {
            for text in &texts {
                pin.track(&crate::vector_cache::chunk_hash(text))?;
            }
            pin.renew()?;
        }
        Ok(Some(crate::hybrid::async_jobs::missing(
            &context.source,
            &context.cache,
            embedder.model_id(),
            texts,
            None,
        )?))
    }
    pub(crate) fn acknowledgement(&self) -> Result<serde_json::Value> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| Error::internal("job database absent"))?;
        crate::daemon::tools::sync_status(db, &serde_json::json!({"job_id":self.sync_id}))
    }
    fn terminal(&mut self) -> Result<serde_json::Value> {
        if let Some(sync) = &self.catalog_sync {
            return sync.terminal(self.error.as_deref());
        }
        let Some(context) = &self.context else {
            return Err(Error::Admission {
                message: "background admission abandoned".into(),
            });
        };
        let Some(db) = &self.db else {
            return Err(Error::internal("background database unavailable"));
        };
        if self.complete
            && self
                .operation
                .as_ref()
                .is_some_and(|operation| operation.check().is_err())
        {
            self.complete = false;
            self.error = Some(
                "background job cancelled or deadline expired before terminal publication".into(),
            );
        }
        let success = self.complete && self.error.is_none();
        if !success
            && self.mutation == Mutation::Started
            && !matches!(context.input, JobInput::Watch(_))
        {
            registry::set_status(db, context.project.id, ProjectStatus::Error)?;
        }
        if self.owns_sync {
            if !success {
                self.record["error"] = serde_json::json!(
                    self.error
                        .as_deref()
                        .unwrap_or("background job cancelled or abandoned")
                );
            }
            let record = serde_json::to_string(&self.record)
                .map_err(|e| Error::internal_with_source("encode terminal sync record", e))?;
            if record.len() as u64 > crate::limits::MAX_STATE_BYTES {
                return Err(Error::internal("terminal sync record exceeds state bound"));
            }
            repo::finish_sync_job(
                db.connection(),
                self.sync_id
                    .ok_or_else(|| Error::internal("owned sync id absent"))?,
                if success {
                    SyncState::Done
                } else {
                    SyncState::Error
                },
                Some(&record),
            )?;
        }
        if !success {
            return Err(Error::Admission {
                message: self
                    .error
                    .take()
                    .unwrap_or_else(|| "background job cancelled or abandoned".into()),
            });
        }
        if self.sync_id.is_some() {
            crate::daemon::tools::sync_status(db, &serde_json::json!({"job_id":self.sync_id}))
        } else {
            serde_json::to_value(
                &self
                    .plan
                    .as_ref()
                    .ok_or_else(|| Error::internal("completed lexical plan absent"))?
                    .stats,
            )
            .map_err(|e| Error::internal_with_source("encode lexical job counters", e))
        }
    }
    pub(crate) fn retire(mut self: Box<Self>, activity: Arc<Activity>) {
        let reply = self.reply.take();
        let result = self.terminal();
        let handoff = if result.is_ok() {
            self.pin.take().and_then(|mut pin| {
                let account = self.retained.clone()?;
                pin.retain_account(account);
                pin.retain_activity(activity);
                Some((pin, self.context.take()?, self.generation?))
            })
        } else {
            None
        };
        #[cfg(test)]
        if self.index.is_some()
            && let Some(observer) = &self.retirement_observer
        {
            observer("before_index");
        }
        drop(self.index.take());
        #[cfg(test)]
        if let Some(observer) = &self.retirement_observer {
            observer("after_index");
        }
        drop(self); // Db/data/account → writer lease, all inside T.
        if let Some((pin, context, generation)) = handoff {
            let warm = (|| {
                let dense = context
                    .dense
                    .as_ref()
                    .ok_or_else(|| Error::internal("ANN owner absent"))?;
                let model = context
                    .stages
                    .embedder
                    .as_ref()
                    .ok_or_else(|| Error::internal("ANN embedder absent"))?
                    .model_id();
                dense.warm_with_job_pin(
                    crate::hybrid::dense::DenseRequest {
                        cache_root: &context.cache,
                        project_id: context.project.id,
                        generation,
                        model_id: model,
                    },
                    &crate::hybrid::work::Cancellation::new(),
                    || {
                        lifecycle::with_project_read_lease(
                            &context.cache,
                            context.project.id,
                            || {
                                let db =
                                    Db::open_with_settings(&context.cache, &context.db_settings)?;
                                if repo::index_generation(db.connection(), context.project.id)?
                                    != generation
                                {
                                    return Err(Error::Admission {
                                        message: "project generation changed before ANN handoff"
                                            .into(),
                                    });
                                }
                                crate::hybrid::dense_metadata::DenseMetadata::capture(
                                    &db,
                                    context.project.id,
                                )
                            },
                        )
                    },
                    || {
                        context
                            .revision
                            .as_ref()
                            .map(|slot| slot.preparation_lease(&context.config))
                            .transpose()
                    },
                    pin,
                )
            })();
            if let Err(error) = warm {
                eprintln!("warning: deferred ANN warm after sync: {error}");
            }
        }
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }
}

fn embedding_dimension(
    config: &Config,
    embedder: &dyn crate::vector_cache::Embedder,
) -> Result<usize> {
    crate::hybrid::dense::model_dim(embedder.model_id())
        .or_else(|| {
            config
                .hybrid_cfg
                .embedding
                .as_ref()
                .map(|cfg| cfg.dimensions as usize)
        })
        .filter(|dim| *dim > 0)
        .ok_or_else(|| Error::internal("embedding stage has no captured dimension"))
}

fn embedding_shape(db: &Db, project: i64) -> Result<(usize, usize)> {
    let (rows, longest): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(length(CAST(c.text AS BLOB))), 0)
         FROM chunks c JOIN docs d ON d.id=c.doc_id WHERE d.project_id=?1",
            [project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(repo::db_error)?;
    Ok((
        usize::try_from(rows).map_err(|_| Error::internal("invalid embedding chunk count"))?,
        usize::try_from(longest).map_err(|_| Error::internal("invalid embedding text length"))?,
    ))
}

fn embedding_texts(
    db: &Db,
    project: i64,
    cursor: &mut (i64, i64),
    policy: &crate::hybrid::remote_settings::RemoteSettings,
    operation: &SearchOperation,
) -> Result<Vec<String>> {
    let mut statement = db.connection().prepare("SELECT c.doc_id,c.seq,length(CAST(c.text AS BLOB)),c.text FROM chunks c JOIN docs d ON d.id=c.doc_id WHERE d.project_id=?1 AND (c.doc_id,c.seq)>(?2,?3) ORDER BY c.doc_id,c.seq LIMIT ?4").map_err(repo::db_error)?;
    let mut rows = statement
        .query(rusqlite::params![
            project,
            cursor.0,
            cursor.1,
            i64::try_from(policy.embed_batch_texts)
                .map_err(|_| Error::internal("batch count overflow"))?
        ])
        .map_err(repo::db_error)?;
    let mut texts = Vec::new();
    let mut bytes = 0_usize;
    while let Some(row) = rows.next().map_err(repo::db_error)? {
        let length: i64 = row.get(2).map_err(repo::db_error)?;
        let length =
            usize::try_from(length).map_err(|_| Error::internal("backfill text length invalid"))?;
        if !texts.is_empty() && bytes.saturating_add(length) > policy.embed_batch_bytes {
            break;
        }
        operation.check()?;
        texts.push(row.get::<_, String>(3).map_err(repo::db_error)?);
        bytes = bytes.saturating_add(length);
        *cursor = (
            row.get(0).map_err(repo::db_error)?,
            row.get(1).map_err(repo::db_error)?,
        );
    }
    Ok(texts)
}

// Includes oversized new/old atomic text, worst-case 6-byte JSON escapes,
// concurrent Rig bytes/Value/final String growth and the live input clones.
fn embedding_workspace(
    policy: &crate::hybrid::remote_settings::RemoteSettings,
    longest: usize,
    captured_peak: usize,
    count: usize,
    dimension: usize,
) -> Result<u64> {
    (policy.embed_batch_bytes.max(longest).max(captured_peak) as u64)
        .checked_mul(40)
        .and_then(|bytes| {
            (policy.embed_batch_texts as u64)
                .checked_mul(dimension as u64)
                .and_then(|n| n.checked_mul(8))
                .and_then(|vectors| bytes.checked_add(vectors))
        })
        .and_then(|bytes| {
            (count as u64)
                .checked_mul(1024)
                .and_then(|hashes| bytes.checked_add(hashes))
        })
        .ok_or_else(|| Error::Admission {
            message: "job embedding workspace overflow".into(),
        })
}

pub(crate) async fn start(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    operation: &SearchOperation,
    budget: &PayloadBudget,
    context: JobContext,
) -> Result<(JobOwner, bool, oneshot::Receiver<Result<serde_json::Value>>)> {
    let owner = service
        .admit_with_account(
            workers.clone(),
            activity.clone(),
            operation,
            budget,
            context.input.account(),
        )
        .await?;
    start_admitted(service, workers, activity, operation, owner, context).await
}

async fn start_admitted(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    operation: &SearchOperation,
    owner: JobOwner,
    context: JobContext,
) -> Result<(JobOwner, bool, oneshot::Receiver<Result<serde_json::Value>>)> {
    let (reply, receiver) = oneshot::channel();
    let call = operation.clone();
    let (owner, prepared) = service
        .phase(workers, activity, operation, owner, move |state| {
            state.prepare(context, reply, &call)
        })
        .await?;
    let work = **prepared;
    drop(prepared);
    Ok((owner, work, receiver))
}

type BatchPacket = (
    Option<crate::hybrid::async_jobs::EmbeddingBatch>,
    Option<Arc<dyn crate::vector_cache::Embedder>>,
    Option<Arc<PayloadPermit>>,
);

async fn infer_batch(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: &Arc<Activity>,
    operation: &SearchOperation,
    mut owner: JobOwner,
    packet: Arc<crate::daemon::job_service::JobValue<BatchPacket>>,
) -> Result<(
    JobOwner,
    Arc<crate::daemon::job_service::JobValue<Vec<Vec<f32>>>>,
)> {
    #[cfg(feature = "hybrid-remote")]
    let batch = packet
        .0
        .as_ref()
        .ok_or_else(|| Error::internal("embedding batch absent"))?;
    let embedder = packet
        .1
        .as_ref()
        .ok_or_else(|| Error::internal("embedding stage absent"))?
        .clone();
    let vectors = {
        #[cfg(feature = "hybrid-remote")]
        if let Some(remote) = embedder.async_remote() {
            let mut call = owner
                .call(operation.clone())
                .with_activity(activity.clone());
            if let Some(account) = &packet.2 {
                call = call.with_payload_reservation(account.clone());
            }
            match call
                .wait(
                    &crate::hybrid::work::Cancellation::new(),
                    remote.embed(batch.texts.clone(), &call),
                )
                .await
            {
                Ok(vectors) => owner.retain(vectors),
                Err(error) => {
                    owner.fail(error.to_string());
                    return Err(error);
                }
            }
        } else {
            let inputs = packet.clone();
            let (next, vectors) = service
                .phase(workers, activity.clone(), operation, owner, move |_| {
                    crate::hybrid::blocking::enter(|| {
                        embedder.embed(
                            &inputs
                                .0
                                .as_ref()
                                .ok_or_else(|| Error::internal("embedding batch absent"))?
                                .texts,
                        )
                    })
                })
                .await?;
            owner = next;
            vectors
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            let inputs = packet.clone();
            let (next, vectors) = service
                .phase(workers, activity.clone(), operation, owner, move |_| {
                    crate::hybrid::blocking::enter(|| {
                        embedder.embed(
                            &inputs
                                .0
                                .as_ref()
                                .ok_or_else(|| Error::internal("embedding batch absent"))?
                                .texts,
                        )
                    })
                })
                .await?;
            owner = next;
            vectors
        }
    };
    Ok((owner, vectors))
}
async fn embed_batches(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: &Arc<Activity>,
    operation: &SearchOperation,
    mut owner: JobOwner,
    backfill: bool,
) -> Result<JobOwner> {
    loop {
        let call = operation.clone();
        let (next, packet) = service
            .phase(workers, activity.clone(), operation, owner, move |state| {
                if let Some(sync) = &mut state.catalog_sync {
                    return sync.batch_packet(&call, state.account.clone());
                }
                let batch = state.batch(backfill, &call)?;
                let context = state
                    .context
                    .as_ref()
                    .ok_or_else(|| Error::internal("job context absent"))?;
                Ok((
                    batch,
                    context.stages.embedder.clone(),
                    state.retained.clone(),
                ))
            })
            .await?;
        owner = next;
        let Some(batch) = &packet.0 else {
            drop(packet);
            return Ok(owner);
        };
        if batch.texts.is_empty() {
            drop(packet);
            continue;
        }
        let (next, vectors) =
            infer_batch(service, workers, activity, operation, owner, packet.clone()).await?;
        owner = next;
        let call = operation.clone();
        let (next, written) = service
            .phase(workers, activity.clone(), operation, owner, move |state| {
                call.check()?;
                if let Some(sync) = &state.catalog_sync {
                    let batch = packet
                        .0
                        .as_ref()
                        .ok_or_else(|| Error::internal("embedding batch absent"))?;
                    return sync.store(batch, &vectors);
                }
                let context = state
                    .context
                    .as_ref()
                    .ok_or_else(|| Error::internal("job context absent"))?;
                let _revision = context
                    .revision
                    .as_ref()
                    .map(|slot| slot.preparation_lease(&context.config))
                    .transpose()?;
                crate::hybrid::async_jobs::store(
                    &context.source,
                    &context.cache,
                    packet
                        .1
                        .as_ref()
                        .ok_or_else(|| Error::internal("embedding stage absent"))?
                        .model_id(),
                    packet
                        .0
                        .as_ref()
                        .ok_or_else(|| Error::internal("embedding batch absent"))?,
                    &vectors,
                    state.dimension,
                )?;
                if let Some(pin) = &state.pin {
                    pin.renew()?;
                }
                Ok(())
            })
            .await?;
        drop(written);
        owner = next;
    }
}

pub(crate) async fn execute(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    operation: SearchOperation,
    mut owner: JobOwner,
    work: bool,
    receiver: oneshot::Receiver<Result<serde_json::Value>>,
) -> Result<serde_json::Value> {
    if work {
        owner = embed_batches(service, workers, &activity, &operation, owner, false).await?;
        let call = operation.clone();
        let (next, committed) = service
            .phase(workers, activity.clone(), &operation, owner, move |state| {
                state.commit(&call)
            })
            .await?;
        let backfill = **committed;
        drop(committed);
        owner = next;
        if backfill {
            owner = embed_batches(service, workers, &activity, &operation, owner, true).await?;
        }
        let call = operation.clone();
        let (next, completed) = service
            .phase(workers, activity, &operation, owner, move |state| {
                call.check()?;
                state.complete = true;
                Ok(())
            })
            .await?;
        drop(completed);
        owner = next;
    }
    drop(owner);
    crate::hybrid::work::RemoteCall::new(operation)
        .wait(&crate::hybrid::work::Cancellation::new(), async {
            receiver
                .await
                .map_err(|e| Error::internal_with_source("background terminal reply", e))?
        })
        .await
}
pub(crate) async fn run(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    operation: SearchOperation,
    budget: &PayloadBudget,
    context: JobContext,
) -> Result<serde_json::Value> {
    let (owner, work, receiver) = start(
        service,
        workers,
        activity.clone(),
        &operation,
        budget,
        context,
    )
    .await?;
    execute(service, workers, activity, operation, owner, work, receiver).await
}

pub(crate) async fn run_reserved(
    service: &JobService,
    workers: &Arc<WorkerAdmission>,
    activity: Arc<Activity>,
    budget: &PayloadBudget,
    context: JobContext,
    reservation: crate::daemon::job_service::JobReservation,
) -> Result<serde_json::Value> {
    let operation = reservation.operation();
    let owner = service
        .admit_reserved(workers.clone(), activity.clone(), budget, reservation)
        .await?;
    let (owner, work, receiver) = start_admitted(
        service,
        workers,
        activity.clone(),
        &operation,
        owner,
        context,
    )
    .await?;
    execute(service, workers, activity, operation, owner, work, receiver).await
}
