//! Tool implementations shared by the daemon and the direct CLI mode so both
//! return identical payloads (FR-20…FR-26, FR-30).

use std::path::{Component, Path};

use serde_json::{Value, json};

use crate::config::Config;
use crate::daemon::session::Stats;
use crate::daemon::{lifecycle, registry};
use crate::error::{Error, Result};
use crate::hybrid::HybridStages;
use crate::hybrid::Ranking;
use crate::hybrid::blocking;
use crate::hybrid::config::DEFAULT_RERANK_WINDOW;
use crate::hybrid::dense::{self, DenseCache, DenseRequest, DenseRows};
use crate::hybrid::dense_metadata::DenseMetadata;
use crate::hybrid::jobs::{self as hybrid_jobs, Coverage};
use crate::hybrid::meta::{
    self, AnnState, Degraded, FiltersMeta, ScoreScale, SearchMeta, Stage, TrimmedMeta,
};
use crate::hybrid::rrf::rrf_merge;
use crate::hybrid::work::Cancellation;
use crate::index::QueryMode;
use crate::index::job::{JobStats, run_full};
use crate::index::tantivy_index::{MAX_HITS, ReadIndex, chunk_id, chunk_id_parts};
use crate::ipc::protocol::{PROTOCOL_VERSION, build_id};
use crate::search::budget::PayloadBudget;
use crate::search::metadata::{MetadataReader, MetadataSource};
use crate::search::operation::SearchOperation;
use crate::search::payload::{OwnedPayload, PayloadRequest};
use crate::search::scope::{DocumentScope, ScopeRequest, has_tags};
use crate::store::migrations;
use crate::store::models::{Project, ProjectStatus, SyncJob, SyncState};
use crate::store::{Db, repo};

#[path = "search_refill.rs"]
mod search_refill;
use search_refill::Refill;

#[cfg(test)]
#[path = "search_payload_tests.rs"]
mod search_payload_tests;

/// `search_docs`: top-k chunks with citation (FR-20).
///
/// # Errors
/// Returns [`Error::Query`] for invalid queries and [`Error::Project`] when
/// the project is not indexed.
pub fn search_docs(db: &Db, cache: &Path, project: &Project, args: &Value) -> Result<Value> {
    search_docs_with(
        &HybridStages::default(),
        &DenseCache::new(),
        db,
        cache,
        project,
        args,
        DEFAULT_RERANK_WINDOW,
    )
}

/// Applies the rerank stage to `ordered` in place (FR-11): the provider
/// receives exactly the candidate window and ranks all of it (`top_n` =
/// `documents.len()`); candidates it did not rank keep their order after
/// the ranked ones.
fn apply_rerank(
    stages: &HybridStages,
    payload: &OwnedPayload,
    query: &str,
    ordered: &mut Vec<(u64, f32)>,
) -> Result<()> {
    if ordered.is_empty() {
        return Ok(());
    }
    let Some(reranker) = stages.reranker.as_deref() else {
        return Ok(());
    };
    let documents = payload.rerank_documents()?;
    // `documents` is bounded by the ordinary or explicitly refilled window:
    // the provider ranks exactly the
    // candidates it received (FR-11); every ranked score is a
    // `relevance_score`, so the tail can never mix score scales.
    let ranked = blocking::enter(|| reranker.rerank(query, documents, documents.len()))?;
    let mut reordered: Vec<(u64, f32)> = Vec::with_capacity(ordered.len());
    let mut ranked_flags = vec![false; ordered.len()];
    for (index, score) in ranked {
        if let Some(entry) = ordered.get(index)
            && !ranked_flags[index]
        {
            ranked_flags[index] = true;
            reordered.push((entry.0, score));
        }
    }
    // Defensive: candidates the provider did not rank keep the incoming
    // order after the ranked ones (the provider clamps top_n to the
    // candidate count, so this is unreachable with a conforming response).
    for (index, entry) in ordered.iter().enumerate() {
        if !ranked_flags[index] {
            reordered.push(*entry);
        }
    }
    *ordered = reordered;
    Ok(())
}

/// Dense candidate list for the RRF stage (FR-15): the query embedding plus
/// cached per-chunk vectors. `Ready` scans the in-memory rows; `TooLarge`
/// streams the cache with a bounded heap, so memory stays flat above the cap.
#[derive(Clone, Copy)]
struct DenseQuery<'a> {
    text: &'a str,
    generation: i64,
    fetch: usize,
}

fn dense_candidates<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    query: DenseQuery<'_>,
    degraded: &mut Vec<Degraded>,
) -> Result<Vec<(u64, f32)>> {
    let SearchContext {
        stages,
        dense,
        project,
        operation,
        ..
    } = context;
    let Some(embedder) = stages.embedder.as_deref() else {
        return Ok(Vec::new());
    };
    // Search embeds through `embed_query` so local providers apply their
    // registry query prefix (FR-10, SC-9); backfill uses `embed`.
    // A repeated (model, query) pair is served from the query cache: the vector
    // is a pure function of both, so this cannot change the answer
    // (`search-cost` FR-1/FR-3).
    let model_id = embedder.model_id();
    let vector = dense.query_embedding(embedder, query.text, operation.cancellation())?;
    operation.check()?;
    let rows = dense.get_with_metadata_admitted(
        DenseRequest {
            cache_root: context.cache,
            project_id: project.id,
            generation: query.generation,
            model_id,
        },
        operation.cancellation(),
        || capture_dense(source, project, query.generation),
        admit,
    )?;
    operation.check()?;
    let fetch = query.fetch;
    match rows {
        DenseRows::Ready(index) => {
            dense.record_outcome(project.id, AnnState::Ready, None);
            dense::top_k(&vector, index.rows(), fetch)
        }
        DenseRows::Ann(ann) => match ann.search(&vector, fetch) {
            Ok(hits) if hits.len() >= fetch => {
                #[cfg(debug_assertions)]
                ANN_SEARCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                dense.record_outcome(project.id, AnnState::Ready, None);
                Ok(hits)
            }
            Ok(hits) => {
                eprintln!(
                    "warning: ann returned {} candidates for fetch {fetch}; falling back to exact",
                    hits.len()
                );
                degrade(degraded, Degraded::AnnInsufficient);
                dense.record_outcome(
                    project.id,
                    AnnState::Fallback,
                    Some("ann returned fewer candidates than the window"),
                );
                operation.check()?;
                stream_exact(source, project, query.generation, model_id, &vector, fetch)
            }
            Err(err) => {
                eprintln!("warning: ann search failed: {err}; falling back to exact");
                degrade(degraded, Degraded::AnnError);
                dense.record_outcome(project.id, AnnState::Fallback, Some("ann search failed"));
                operation.check()?;
                stream_exact(source, project, query.generation, model_id, &vector, fetch)
            }
        },
        DenseRows::TooLarge(hashes) => {
            // Above the resident cap by design: exact streaming is the answer,
            // not an error.
            degrade(degraded, Degraded::ExactFallback);
            dense.record_outcome(
                project.id,
                AnnState::TooLarge,
                Some("above the resident cap"),
            );
            stream_hashes(context.cache, model_id, &hashes, &vector, fetch)
        }
        DenseRows::Building(building) => {
            degrade(degraded, Degraded::AnnBuilding);
            dense.record_outcome(
                project.id,
                AnnState::Building,
                Some("background build in flight"),
            );
            stream_hashes(context.cache, model_id, &building.hashes, &vector, fetch)
        }
        DenseRows::Failed(failed) => {
            degrade(degraded, Degraded::AnnError);
            dense.record_outcome(
                project.id,
                AnnState::Fallback,
                Some("the last build failed"),
            );
            stream_hashes(context.cache, model_id, &failed.hashes, &vector, fetch)
        }
    }
}

/// Debug-only counter: how many dense lookups the ANN tier actually *served*
/// (a consultation that fell back to exact is not counted).
/// Integration tests use it to prove the read path took the ANN branch rather
/// than silently falling back to exact (Delta 0.3).
#[cfg(debug_assertions)]
static ANN_SEARCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test-only ANN read-path counter; absent in release builds.
#[cfg(debug_assertions)]
#[doc(hidden)]
#[must_use]
pub fn ann_search_count() -> u64 {
    ANN_SEARCHES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Exact scoped dense scan: SQL is reacquired only to capture bounded hash
/// pages, and is dropped before any vector-cache IO. The original generation
/// is checked even for pages whose vectors are missing.
fn scoped_dense<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    query: DenseQuery<'_>,
    scope: &DocumentScope,
) -> Result<Vec<(u64, f32)>> {
    if scope.documents().is_empty() {
        return Ok(Vec::new());
    }
    let Some(embedder) = context.stages.embedder.as_deref() else {
        return Ok(Vec::new());
    };
    let vector =
        context
            .dense
            .query_embedding(embedder, query.text, context.operation.cancellation())?;
    context.operation.check()?;
    let mut cache = crate::vector_cache::VectorCache::open(context.cache)?;
    let mut top = dense::TopK::new(&vector, query.fetch)?;
    for documents in scope.documents().chunks(repo::BATCH_KEYS) {
        let mut after = (0_i64, 0_u32);
        loop {
            context.operation.check()?;
            let hashes = in_read_phase(context, source, admit, |reader| {
                let transaction = reader
                    .connection()
                    .unchecked_transaction()
                    .map_err(repo::db_error)?;
                snapshot_info(&reader, context.project, Some(query.generation))?;
                let hashes = crate::search::scope::hash_page(
                    &transaction,
                    context.project.id,
                    documents,
                    after,
                    &context.operation,
                )?;
                transaction.commit().map_err(repo::db_error)?;
                Ok(hashes)
            })?;
            if let Some((id, _)) = hashes.chunks.last() {
                after = chunk_id_parts(*id);
            }
            cache.visit_page(embedder.model_id(), &hashes.chunks, |id, vector| {
                context.operation.check()?;
                if let Some(vector) = vector {
                    top.push(id, vector)?;
                }
                Ok(())
            })?;
            if !hashes.more {
                break;
            }
        }
    }
    Ok(top.finish())
}

fn stream_exact(
    source: MetadataSource<'_>,
    project: &Project,
    generation: i64,
    model_id: &str,
    vector: &[f32],
    fetch: usize,
) -> Result<Vec<(u64, f32)>> {
    let metadata = capture_dense(source, project, generation)?;
    stream_hashes(
        source.cache_root(),
        model_id,
        metadata.chunks(),
        vector,
        fetch,
    )
}

fn stream_hashes(
    root: &Path,
    model_id: &str,
    hashes: &[(u64, String)],
    vector: &[f32],
    fetch: usize,
) -> Result<Vec<(u64, f32)>> {
    let mut cache = crate::vector_cache::VectorCache::open(root)?;
    let rows = hashes
        .iter()
        .filter_map(move |(chunk_id, hash)| match cache.get(model_id, hash) {
            Ok(Some(vector)) => Some(Ok((*chunk_id, vector))),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        });
    dense::top_k_iter(vector, rows, fetch)
}

/// Compatibility search with resolved hybrid stages and candidate window.
/// Resolves the standalone owner's payload policy for this invocation; the
/// daemon and direct CLI use [`search_docs_in`] with their explicit policy.
///
/// # Errors
/// Returns [`Error::Query`] for invalid queries, [`Error::Project`] when the
/// project is not indexed, and [`Error::Protocol`] for malformed arguments
/// (including `rerank=true` when the project has no rerank stage).
pub fn search_docs_with(
    stages: &HybridStages,
    dense: &DenseCache,
    db: &Db,
    cache: &Path,
    project: &Project,
    args: &Value,
    window: u32,
) -> Result<Value> {
    // Explicit standalone adapter. Production passes its resolved policy and
    // shared admission via search_docs_in instead of constructing a budget here.
    let policy = crate::search::settings::standalone()?;
    let budget = PayloadBudget::new(policy.settings().clone())?;
    let operation = SearchOperation::new(
        policy.settings(),
        std::time::Instant::now(),
        Cancellation::new(),
    )?;
    search_docs_in(
        &SearchContext {
            stages,
            dense,
            cache,
            project,
            window,
            payload_budget: &budget,
            operation,
        },
        db,
        args,
    )
}

/// Explicit resources for a search; no component fills missing policy fields.
pub struct SearchContext<'a> {
    /// Stages belonging to the captured project configuration.
    pub stages: &'a HybridStages,
    /// Shared query/vector cache.
    pub dense: &'a DenseCache,
    /// Owning cache root.
    pub cache: &'a Path,
    /// Bound project snapshot.
    pub project: &'a Project,
    /// Complete resolved candidate window.
    pub window: u32,
    /// Daemon-shared or direct invocation payload allowance.
    pub payload_budget: &'a PayloadBudget,
    /// Original deadline and cooperative cancellation for this invocation.
    pub operation: SearchOperation,
}

/// Search using explicit admission and cancellation. Response stages consume
/// one owned SQL payload; the caller currently controls the preparation Db.
///
/// # Errors
/// Returns query/project/protocol errors as in [`search_docs_with`], payload
/// admission failures, storage errors and cooperative cancellation.
pub fn search_docs_in(context: &SearchContext<'_>, db: &Db, args: &Value) -> Result<Value> {
    search_docs_from(context, MetadataSource::Borrowed(db), args, || Ok(()))
}

/// Search through short metadata phases and explicit configuration admission.
/// Providers and flight followers never retain an SQL/preparation/file lease.
///
/// # Errors
/// As [`search_docs_in`], including stale revision/generation and pool admission.
pub fn search_docs_from<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    args: &Value,
    admit: impl Fn() -> Result<Guard>,
) -> Result<Value> {
    context.operation.check()?;
    if source.cache_root() != context.cache {
        return Err(Error::Admission {
            message: "search metadata belongs to a different cache root".to_owned(),
        });
    }
    ensure_indexed(context.project)?;
    let arguments = parse_search_args(args)?;
    if arguments.limit == 0 {
        if !arguments.explain {
            return Ok(Value::Array(Vec::new()));
        }
        return in_read_phase(context, source, &admit, |reader| {
            snapshot_info(&reader, context.project, None)?;
            empty_response(
                &reader,
                context.project,
                filters_of(
                    arguments.dedupe,
                    arguments.n_per_doc,
                    arguments.path_prefix.as_deref(),
                    &arguments.tags,
                ),
                &context.operation,
            )
        });
    }
    prepare_search(context, source, &admit, arguments)?.finish(context.stages)
}

/// No Db, SQL transaction, index handle or filesystem lease can escape capture.
struct PreparedSearch {
    payload: OwnedPayload,
    arguments: SearchArgs,
    ordered: Vec<(u64, f32)>,
    active: ActiveStages,
    stage_ms: Vec<(Stage, u64)>,
    degraded: Vec<Degraded>,
    operation: SearchOperation,
    fetch: usize,
    refill: Option<Refill>,
}

impl PreparedSearch {
    fn finish(mut self, stages: &HybridStages) -> Result<Value> {
        self.operation.check()?;
        if self.active.rerank_active {
            let started = std::time::Instant::now();
            apply_rerank(
                stages,
                &self.payload,
                &self.arguments.query,
                &mut self.ordered,
            )?;
            self.stage_ms.push((Stage::Rerank, elapsed_ms(started)));
        }
        self.operation.check()?;
        let post = apply_post_filter(
            &self.payload,
            &mut self.ordered,
            self.arguments.dedupe,
            self.arguments.n_per_doc,
            self.arguments.path_prefix.as_deref(),
            &self.arguments.tags,
        );
        self.ordered.truncate(self.arguments.limit);
        let rows = build_rows(&self.payload, self.ordered, self.arguments.snippet);
        self.operation.check()?;
        if !self.arguments.explain {
            return Ok(Value::Array(rows));
        }
        let meta = build_meta(
            self.active,
            self.stage_ms,
            self.degraded,
            post,
            elapsed_ms(self.operation.started_at()),
            self.fetch,
            self.payload.generation(),
        );
        let mut response = meta::envelope_with_queue(&rows, &meta, self.operation.queue_ms());
        if let Some(refill) = self.refill {
            response["meta"]["refill"] = refill.to_json(rows.len(), self.arguments.limit);
        }
        Ok(response)
    }
}

fn search_plan(
    stages: &HybridStages,
    args: &SearchArgs,
    window: u32,
) -> Result<(ActiveStages, usize)> {
    let rrf_active = stages.ranking == Ranking::Rrf;
    if rrf_active && stages.embedder.is_none() {
        return Err(Error::internal(
            "search_docs: rrf ranking requires the embedding stage (FR-17)",
        ));
    }
    let rerank_active = args.rerank.unwrap_or(stages.reranker.is_some());
    if args.rerank == Some(true) && stages.reranker.is_none() {
        return Err(Error::Protocol {
            message: "search_docs: rerank=true but hybrid rerank is not enabled for this project"
                .to_owned(),
        });
    }
    let filtering = args.dedupe != Dedupe::None;
    let fetch = if rerank_active || rrf_active {
        // Ordinary base pool; document caps may refill before one rerank.
        args.limit.max(window as usize).min(MAX_HITS)
    } else if filtering {
        args.limit
            .saturating_mul(SEARCH_UX_OVERSCAN)
            .max(args.limit)
            .min(MAX_HITS)
    } else {
        args.limit
    };
    Ok((
        ActiveStages {
            rerank_active,
            rrf_active,
        },
        fetch,
    ))
}

pub(super) fn snapshot_info(db: &Db, project: &Project, expected: Option<i64>) -> Result<i64> {
    let current = registry::project_by_id(db, project.id)?.ok_or_else(|| Error::Project {
        message: format!("project {} disappeared from the registry", project.id),
        instruction: Some("retry search".to_owned()),
    })?;
    ensure_indexed(&current)?;
    if current.canonical_root != project.canonical_root {
        return Err(Error::Project {
            message: "project binding changed before search preparation".to_owned(),
            instruction: Some("register the session again".to_owned()),
        });
    }
    let generation = repo::index_generation(db.connection(), project.id)?;
    if expected.is_some_and(|expected| expected != generation) {
        return Err(Error::Project {
            message: "project generation changed during search preparation".to_owned(),
            instruction: Some("retry search".to_owned()),
        });
    }
    if repo::has_pending_docs(db.connection(), project.id)? {
        return Err(Error::Project {
            message: "project has an incomplete index update".to_owned(),
            instruction: Some(
                "run `docsbase index` to complete the index before searching".to_owned(),
            ),
        });
    }
    Ok(generation)
}

fn in_read_phase<'a, T, Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'a>,
    admit: &impl Fn() -> Result<Guard>,
    work: impl FnOnce(MetadataReader<'a>) -> Result<T>,
) -> Result<T> {
    context.operation.check()?;
    let reader = source.acquire(&context.operation)?;
    context.operation.check()?;
    let _revision = admit()?;
    context.operation.check()?;
    lifecycle::with_project_read_lease(context.cache, context.project.id, || {
        context.operation.check()?;
        work(reader)
    })
}

// Shared cold/fallback metadata has no caller deadline/token. Its immediate
// prepared admission cannot cancel live peers when a flight leader expires.
pub(super) fn capture_dense(
    source: MetadataSource<'_>,
    project: &Project,
    generation: i64,
) -> Result<DenseMetadata> {
    let reader = source.try_acquire()?;
    lifecycle::with_project_read_lease(source.cache_root(), project.id, || {
        snapshot_info(&reader, project, Some(generation))?;
        DenseMetadata::capture(&reader, project.id)
    })
}

struct LexicalSnapshot {
    generation: i64,
    hits: Vec<(u64, f32)>,
    elapsed_ms: u64,
    scope: Option<DocumentScope>,
}

fn lexical_snapshot<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    arguments: &SearchArgs,
    fetch: usize,
) -> Result<LexicalSnapshot> {
    in_read_phase(context, source, admit, |reader| {
        let started = std::time::Instant::now();
        let transaction = reader
            .connection()
            .unchecked_transaction()
            .map_err(repo::db_error)?;
        let generation = snapshot_info(&reader, context.project, None)?;
        let scope = if arguments.path_prefix.is_some() || !arguments.tags.is_empty() {
            Some(DocumentScope::capture(
                &transaction,
                ScopeRequest {
                    project: context.project.id,
                    prefix: arguments.path_prefix.as_deref(),
                    tags: &arguments.tags,
                    dense: context.stages.ranking == Ranking::Rrf,
                },
                context.payload_budget,
                &context.operation,
            )?)
        } else {
            None
        };
        transaction.commit().map_err(repo::db_error)?;
        drop(reader);
        let hits = lexical_hits(context, arguments, fetch, scope.as_ref())?;
        Ok(LexicalSnapshot {
            generation,
            hits,
            elapsed_ms: elapsed_ms(started),
            scope,
        })
    })
}

fn lexical_hits(
    context: &SearchContext<'_>,
    arguments: &SearchArgs,
    fetch: usize,
    scope: Option<&DocumentScope>,
) -> Result<Vec<(u64, f32)>> {
    let index = ReadIndex::open(&lifecycle::index_dir(context.cache, context.project.id))?;
    let hits = if let Some(scope) = scope {
        index.search_scoped(
            &arguments.query,
            fetch,
            arguments.query_mode,
            scope.documents(),
        )?
    } else {
        index.search_with_mode(&arguments.query, fetch, arguments.query_mode)?
    };
    context.operation.check()?;
    Ok(hits
        .into_iter()
        .map(|hit| (hit.chunk_id, hit.score))
        .collect())
}

fn lexical_refill<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    arguments: &SearchArgs,
    fetch: usize,
    snapshot: &LexicalSnapshot,
) -> Result<Vec<(u64, f32)>> {
    in_read_phase(context, source, admit, |reader| {
        snapshot_info(&reader, context.project, Some(snapshot.generation))?;
        drop(reader);
        lexical_hits(context, arguments, fetch, snapshot.scope.as_ref())
    })
}

fn dense_for_scope<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    query: DenseQuery<'_>,
    scope: Option<&DocumentScope>,
    degraded: &mut Vec<Degraded>,
) -> Result<Vec<(u64, f32)>> {
    if let Some(scope) = scope {
        scoped_dense(context, source, admit, query, scope)
    } else {
        dense_candidates(context, source, admit, query, degraded)
    }
}

fn prepare_search<Guard>(
    context: &SearchContext<'_>,
    source: MetadataSource<'_>,
    admit: &impl Fn() -> Result<Guard>,
    arguments: SearchArgs,
) -> Result<PreparedSearch> {
    context.operation.check()?;
    let (active, mut fetch) = search_plan(context.stages, &arguments, context.window)?;
    let mut lexical = lexical_snapshot(context, source, admit, &arguments, fetch)?;
    let mut refill = Refill::new(
        &arguments,
        fetch,
        context.payload_budget.settings().dedupe_max_candidates,
        active.rrf_active,
    );
    let mut bm25_ms = lexical.elapsed_ms;
    let mut stage_ms = Vec::with_capacity(4);
    let mut degraded = Vec::new();
    if !active.rerank_active && context.stages.reranker.is_some() {
        degrade(&mut degraded, Degraded::NoRerankStage);
    }
    context.operation.check()?;
    let dense_hits = if active.rrf_active {
        let dense_started = std::time::Instant::now();
        let query = DenseQuery {
            text: &arguments.query,
            generation: lexical.generation,
            // One frozen source: no repeated full-vector scans or changing ANN
            // membership as lexical/fusion prefixes grow.
            fetch: refill.as_ref().map_or(fetch, Refill::ceiling),
        };
        let hits = dense_for_scope(
            context,
            source,
            admit,
            query,
            lexical.scope.as_ref(),
            &mut degraded,
        )?;
        stage_ms.push((Stage::Dense, elapsed_ms(dense_started)));
        hits
    } else {
        Vec::new()
    };
    let mut fusion_ms = 0_u64;
    let ordered = loop {
        context.operation.check()?;
        let mut ordered = if active.rrf_active {
            let started = std::time::Instant::now();
            let fused = rrf_merge(&lexical.hits, &dense_hits[..fetch.min(dense_hits.len())]);
            fusion_ms = fusion_ms.saturating_add(elapsed_ms(started));
            fused
        } else {
            lexical.hits.clone()
        };
        let exhausted = lexical.hits.len() < fetch
            && (!active.rrf_active || dense_hits.len() < fetch)
            && ordered.len() <= fetch;
        let next = if let Some(refill) = &mut refill {
            refill.select(&mut ordered, fetch)?;
            refill.next(&ordered, &arguments, fetch, exhausted)
        } else {
            ordered.truncate(fetch);
            None
        };
        let Some(next) = next else { break ordered };
        fetch = next;
        let started = std::time::Instant::now();
        lexical.hits = lexical_refill(context, source, admit, &arguments, fetch, &lexical)?;
        bm25_ms = bm25_ms.saturating_add(elapsed_ms(started));
    };
    stage_ms.insert(0, (Stage::Bm25, bm25_ms));
    if active.rrf_active {
        stage_ms.push((Stage::Rrf, fusion_ms));
    }
    // Preparation and final payload allowances never overlap per request.
    drop(lexical.scope);
    let ids: Vec<_> = ordered.iter().map(|(id, _)| *id).collect();
    let payload = in_read_phase(context, source, admit, |reader| {
        snapshot_info(&reader, context.project, Some(lexical.generation))?;
        OwnedPayload::capture(
            &reader,
            PayloadRequest {
                cache_root: context.cache,
                project_id: context.project.id,
                generation: lexical.generation,
                chunk_ids: &ids,
                include_text: active.rerank_active || arguments.snippet,
                include_filter_metadata: arguments.path_prefix.is_some()
                    || !arguments.tags.is_empty(),
            },
            context.payload_budget,
            context.operation.cancellation(),
        )
    })?;
    context.operation.check()?;
    Ok(PreparedSearch {
        payload,
        arguments,
        ordered,
        active,
        stage_ms,
        degraded,
        operation: context.operation.clone(),
        fetch,
        refill,
    })
}

/// Global query-vector cache counters (`search-cost` FR-4). A caller that
/// cannot see the daemon's cache reports zeroes rather than pretending.
fn query_cache_json(dense: Option<&DenseCache>) -> Value {
    let Some(dense) = dense else {
        return json!({ "hits": 0, "misses": 0, "entries": 0, "bytes": 0 });
    };
    let stats = dense.query_cache_stats();
    json!({
        "hits": stats.hits,
        "misses": stats.misses,
        "entries": stats.entries,
        "bytes": stats.bytes,
    })
}

/// ANN tier state as the daemon last saw it (FR-5/FR-6): the last recorded
/// outcome when there is one, otherwise the config/absence tells the story.
fn ann_state(dense: Option<&DenseCache>, project_id: i64, hybrid_on: bool) -> Value {
    let Some(dense) = dense else {
        // CLI direct mode: the outcome record lives in the daemon process.
        return json!({
            "state": AnnState::Unknown.as_str(),
            "vectors": Value::Null,
            "estimated_bytes": Value::Null,
            "reason": "daemon not running",
        });
    };
    let snapshot = dense.ann_snapshot(project_id);
    let (vectors, bytes) = snapshot.map_or((Value::Null, Value::Null), |(count, bytes)| {
        (json!(count), json!(bytes))
    });
    let (state, reason) = classified_ann_state(dense, project_id, hybrid_on);
    json!({
        "state": state.as_str(),
        "vectors": vectors,
        "estimated_bytes": bytes,
        "reason": reason.map_or(Value::Null, Value::from),
    })
}

/// Shared safe classification for daemon status and the local panel.
pub(super) fn classified_ann_state(
    dense: &DenseCache,
    project_id: i64,
    hybrid_on: bool,
) -> (AnnState, Option<&'static str>) {
    match dense.outcome(project_id) {
        Some(outcome) if outcome.is_stale() => {
            (AnnState::Stale, Some("last search is older than one hour"))
        }
        Some(outcome) => (outcome.state, outcome.reason),
        None if !hybrid_on => (AnnState::Disabled, Some("hybrid off")),
        None => (AnnState::Loading, Some("no search yet")),
    }
}

/// Records a degradation once (order preserved, no duplicates).
fn degrade(degraded: &mut Vec<Degraded>, reason: Degraded) {
    if !degraded.contains(&reason) {
        degraded.push(reason);
    }
}

/// Citation rows use only the captured payload, in final ranking order.
fn build_rows(payload: &OwnedPayload, ordered: Vec<(u64, f32)>, snippet: bool) -> Vec<Value> {
    let mut rows = Vec::with_capacity(ordered.len());
    for (chunk_id, score) in ordered {
        let Some(citation) = payload.citation(chunk_id) else {
            continue;
        };
        let preview = if snippet {
            let Some(text) = payload.chunk_text(chunk_id) else {
                continue;
            };
            Some(snippet_of(text))
        } else {
            None
        };
        rows.push(json!(crate::ipc::panel::SearchHit {
            chunk_id,
            path: citation.path.clone(),
            heading_path: citation.heading_path.clone(),
            lines: [citation.line_start, citation.line_end],
            score,
            snippet: preview.as_ref().map(|(text, _)| text.clone()),
            snippet_truncated: preview.map(|(_, truncated)| truncated),
        }));
    }
    rows
}

/// Post-filter over the candidate window (`search-ux` FR-2…FR-5): path prefix,
/// then frontmatter tags, then the per-document cap. Order of the survivors is
/// preserved because `retain` only drops.
fn apply_post_filter(
    payload: &OwnedPayload,
    ordered: &mut Vec<(u64, f32)>,
    dedupe: Dedupe,
    n_per_doc: usize,
    path_prefix: Option<&str>,
    tags: &[String],
) -> PostFilterOutcome {
    let filters = filters_of(dedupe, n_per_doc, path_prefix, tags);
    let mut trimmed = TrimmedMeta::default();
    if path_prefix.is_some() || !tags.is_empty() {
        ordered.retain(|(chunk_id, _)| {
            let doc_id = chunk_id_parts(*chunk_id).0;
            let Some(meta) = payload.doc_meta(doc_id) else {
                // A vanished document cannot be returned or evaluated by a
                // path/tag filter; it is not a filter rejection (issue #62).
                return false;
            };
            let keep = path_prefix
                .as_ref()
                .is_none_or(|prefix| meta.rel_path.starts_with(prefix))
                && (tags.is_empty() || has_tags(meta.frontmatter_json.as_deref(), tags));
            if !keep {
                trimmed.filtered_out += 1;
            }
            keep
        });
    }
    if dedupe == Dedupe::Path {
        let mut per_doc: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        ordered.retain(|(chunk_id, _)| {
            let doc_id = chunk_id_parts(*chunk_id).0;
            let seen = per_doc.entry(doc_id).or_insert(0);
            if *seen >= n_per_doc {
                trimmed.deduped += 1;
                false
            } else {
                *seen += 1;
                true
            }
        });
    }
    PostFilterOutcome { filters, trimmed }
}

/// Requested post-filters, as reported in `meta.filters` (FR-5).
fn filters_of(
    dedupe: Dedupe,
    n_per_doc: usize,
    path_prefix: Option<&str>,
    tags: &[String],
) -> FiltersMeta {
    FiltersMeta {
        path_prefix: path_prefix.map(str::to_owned),
        tags: tags.to_vec(),
        dedupe: (dedupe == Dedupe::Path).then_some("path"),
        n_per_doc: (dedupe == Dedupe::Path).then_some(n_per_doc),
    }
}

/// Post-filter overscan on the lexical path (`search-ux` FR-2).
pub const SEARCH_UX_OVERSCAN: usize = 4;

/// Upper bound for `n_per_doc` (`search-ux` FR-2).
pub const MAX_CHUNKS_PER_DOC: u64 = 10;

/// Longest snippet returned to a caller (`search-ux` FR-1).
pub const SNIPPET_MAX_CHARS: usize = 240;

/// Prefix of `text` at a char boundary, and whether it was cut (`search-ux`
/// FR-1). `char_indices` keeps multi-byte characters and `\r\n` intact.
fn snippet_of(text: &str) -> (String, bool) {
    match text.char_indices().nth(SNIPPET_MAX_CHARS) {
        Some((cut, _)) => {
            // Do not tear a CRLF pair: if the cut lands right after `\r` and the
            // next character is `\n`, drop the `\r` (it is one byte, so the
            // boundary stays a char boundary).
            let cut = if text[..cut].ends_with('\r') && text[cut..].starts_with('\n') {
                cut - 1
            } else {
                cut
            };
            (text[..cut].to_owned(), true)
        }
        None => (text.to_owned(), false),
    }
}

/// Milliseconds elapsed since `start`, saturating instead of wrapping.
fn elapsed_ms(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Post-filter keys of `search_docs` (`search-ux` FR-2…FR-4), validated in one
/// place so the search body stays about searching.
struct PostFilterArgs {
    dedupe: Dedupe,
    n_per_doc: usize,
    path_prefix: Option<String>,
    tags: Vec<String>,
}

fn parse_post_filter(args: &Value) -> Result<PostFilterArgs> {
    let dedupe = match args.get("dedupe") {
        None | Some(Value::Null) => Dedupe::None,
        Some(value) => match value.as_str() {
            Some("none") => Dedupe::None,
            Some("path") => Dedupe::Path,
            _ => {
                return Err(Error::Protocol {
                    message: format!(
                        "search_docs: dedupe must be \"none\" or \"path\", got {value}"
                    ),
                });
            }
        },
    };
    let n_per_doc = match args.get("n_per_doc") {
        // Measured default: with 1 the ru_morph slice loses 3 of 11 expected-path
        // hits, with 2 it loses one and still halves the repetition
        // (`tests/search_bench.rs::dedupe_slice_diagnostics`).
        None | Some(Value::Null) => 2,
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| Error::Protocol {
                message: format!("search_docs: n_per_doc must be an integer, got {value}"),
            })?;
            if !(1..=MAX_CHUNKS_PER_DOC).contains(&raw) {
                return Err(Error::Protocol {
                    message: format!(
                        "search_docs: n_per_doc must be 1..={MAX_CHUNKS_PER_DOC}, got {raw}"
                    ),
                });
            }
            usize::try_from(raw).unwrap_or(usize::try_from(MAX_CHUNKS_PER_DOC).unwrap_or(10))
        }
    };
    // `null` means "unset", like every sibling key: only a real value demands
    // `dedupe: "path"`.
    if matches!(args.get("n_per_doc"), Some(value) if !value.is_null()) && dedupe != Dedupe::Path {
        return Err(Error::Protocol {
            message: "search_docs: n_per_doc requires dedupe = \"path\"".to_owned(),
        });
    }
    let path_prefix = match args.get("path_prefix") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let prefix = value.as_str().ok_or_else(|| Error::Protocol {
                message: format!("search_docs: path_prefix must be a string, got {value}"),
            })?;
            // An empty prefix matches everything: treat it as unset rather than
            // running the filter machinery for a no-op.
            (!prefix.is_empty()).then(|| prefix.to_owned())
        }
    };
    let tags = match args.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => {
            let raw = value.as_array().ok_or_else(|| Error::Protocol {
                message: format!("search_docs: tags must be an array of strings, got {value}"),
            })?;
            let mut tags = Vec::with_capacity(raw.len());
            for tag in raw {
                let tag = tag.as_str().ok_or_else(|| Error::Protocol {
                    message: format!("search_docs: tags must contain strings, got {tag}"),
                })?;
                tags.push(tag.to_owned());
            }
            tags
        }
    };
    Ok(PostFilterArgs {
        dedupe,
        n_per_doc,
        path_prefix,
        tags,
    })
}

/// Parsed `search_docs` arguments: types are validated once, so the search body
/// stays about searching.
struct SearchArgs {
    query: String,
    query_mode: QueryMode,
    limit: usize,
    explain: bool,
    rerank: Option<bool>,
    snippet: bool,
    dedupe: Dedupe,
    n_per_doc: usize,
    path_prefix: Option<String>,
    tags: Vec<String>,
}

/// Which post-filter the caller asked for (`search-ux` FR-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dedupe {
    /// No post-filter (the default).
    None,
    /// At most `n_per_doc` chunks per document.
    Path,
}

fn parse_search_args(args: &Value) -> Result<SearchArgs> {
    let query_mode = QueryMode::from_argument(args.get("query_mode"))?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol {
            message: "search_docs: missing query".to_owned(),
        })?
        .to_owned();
    // Telemetry is opt-in: the default response stays the bare citation array
    // (FR-1/FR-7, spec `search-telemetry`).
    let explain = match args.get("explain") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| Error::Protocol {
            message: format!("search_docs: explain must be a boolean, got {value}"),
        })?,
    };
    // Client-supplied limits are clamped: the top-k collector allocates
    // proportional to the request (final review C1). A wrong type is a
    // protocol error instead of a silent default.
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 10,
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| Error::Protocol {
                message: format!("search_docs: limit must be a positive integer, got {value}"),
            })?;
            usize::try_from(raw).unwrap_or(usize::MAX).min(MAX_HITS)
        }
    };
    if let Some(scope) = args.get("scope")
        && scope.as_str() != Some("project")
    {
        return Err(Error::Protocol {
            message: format!("search_docs: scope must be \"project\", got {scope}"),
        });
    }
    let snippet = match args.get("snippet") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| Error::Protocol {
            message: format!("search_docs: snippet must be a boolean, got {value}"),
        })?,
    };
    let rerank = match args.get("rerank") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_bool().ok_or_else(|| Error::Protocol {
            message: format!("search_docs: rerank must be a boolean, got {value}"),
        })?),
    };
    let PostFilterArgs {
        dedupe,
        n_per_doc,
        path_prefix,
        tags,
    } = parse_post_filter(args)?;
    Ok(SearchArgs {
        query,
        query_mode,
        limit,
        explain,
        rerank,
        snippet,
        dedupe,
        n_per_doc,
        path_prefix,
        tags,
    })
}

/// `limit = 0` answers without searching (FR-8): `[]`, or an envelope whose
/// `window` is `0` when telemetry was requested.
fn empty_response(
    db: &Db,
    project: &Project,
    filters: FiltersMeta,
    operation: &SearchOperation,
) -> Result<Value> {
    let generation = repo::index_generation(db.connection(), project.id)?;
    operation.check()?;
    let meta = SearchMeta {
        score_scale: ScoreScale::Bm25,
        stages: Vec::new(),
        total_ms: elapsed_ms(operation.started_at()),
        stage_ms: Vec::new(),
        window: 0,
        index_generation: generation,
        degraded: Vec::new(),
        filters: Some(filters),
        trimmed: Some(TrimmedMeta::default()),
    };
    Ok(meta::envelope_with_queue(&[], &meta, operation.queue_ms()))
}

/// Which ranking stages actually ran this search.
#[derive(Clone, Copy)]
struct ActiveStages {
    rerank_active: bool,
    rrf_active: bool,
}

/// Filters applied to the window and what they dropped.
struct PostFilterOutcome {
    filters: FiltersMeta,
    trimmed: TrimmedMeta,
}

/// Telemetry for one search (FR-2). The scale and the stage list come from what
/// actually ran, never from the config: a `rerank: false` request on a project
/// with a stage answers in fused order, so the scale is `rrf`.
fn build_meta(
    active: ActiveStages,
    stage_ms: Vec<(Stage, u64)>,
    degraded: Vec<Degraded>,
    post: PostFilterOutcome,
    total_ms: u64,
    fetch: usize,
    index_generation: i64,
) -> SearchMeta {
    let ActiveStages {
        rerank_active,
        rrf_active,
    } = active;
    let PostFilterOutcome { filters, trimmed } = post;
    let score_scale = if rerank_active {
        ScoreScale::Rerank
    } else if rrf_active {
        ScoreScale::Rrf
    } else {
        ScoreScale::Bm25
    };
    let mut stages = vec![Stage::Bm25];
    if rrf_active {
        stages.push(Stage::Dense);
        stages.push(Stage::Rrf);
    }
    if rerank_active {
        stages.push(Stage::Rerank);
    }
    SearchMeta {
        score_scale,
        stages,
        total_ms,
        stage_ms,
        window: fetch,
        index_generation,
        degraded,
        filters: Some(filters),
        trimmed: Some(trimmed),
    }
}

/// `list_docs`: keyset-paginated documents of one project (FR-25).
///
/// `limit` defaults to 50; `cursor` is the `next_cursor` of the previous page.
/// A stable cursor is the last `rel_path` seen, so late inserts before it do
/// not shift the following pages.
///
/// # Errors
/// Returns [`Error::Project`] when the project is not indexed and
/// [`Error::Protocol`] for malformed arguments.
pub fn list_docs(db: &Db, project: &Project, args: &Value) -> Result<Value> {
    ensure_indexed(project)?;
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 50,
        Some(value) => {
            value
                .as_u64()
                .filter(|limit| *limit > 0)
                .ok_or_else(|| Error::Protocol {
                    message: format!("list_docs: limit must be a positive integer, got {value}"),
                })?
        }
    };
    // Clamp exactly like `search_docs`: pagination allocates proportional to
    // the requested limit even though the store caps projects at 20k docs.
    let limit = usize::try_from(limit).unwrap_or(usize::MAX).min(MAX_HITS);
    let cursor = match args.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(other) => {
            return Err(Error::Protocol {
                message: format!("list_docs: cursor must be a string, got {other}"),
            });
        }
    };
    let mut docs =
        repo::docs_overview_page(db.connection(), project.id, cursor, limit.saturating_add(1))?;
    let next_cursor = if docs.len() > limit {
        let last = docs[limit - 1].path.clone();
        docs.truncate(limit);
        Some(last)
    } else {
        None
    };
    let docs: Vec<Value> = docs
        .into_iter()
        .map(|doc| {
            json!({
                "path": doc.path,
                "title": doc.title,
                "size": doc.size,
                "chunks": doc.chunks,
            })
        })
        .collect();
    Ok(json!({ "project": project.name, "docs": docs, "next_cursor": next_cursor }))
}

/// `get_doc`: full document text for a path strictly inside the project root
/// (FR-23, FR-32; I4).
///
/// # Errors
/// Returns [`Error::Project`] when the path is absolute, escapes the root,
/// resolves through a symlink outside it, is not a markdown file, exceeds
/// [`Config::max_file_size`], or cannot be read.
pub fn get_doc(project: &Project, config: &Config, args: &Value) -> Result<Value> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol {
            message: "get_doc: missing path".to_owned(),
        })?;
    let escape = || Error::Project {
        message: format!("path {raw:?} must stay inside the project root"),
        instruction: Some("pass a relative path of a markdown file".to_owned()),
    };
    if raw.is_empty() {
        return Err(escape());
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(escape());
    }
    let outside_root = || Error::Project {
        message: format!(
            "path {raw:?} resolves outside the project root {}",
            project.canonical_root.display()
        ),
        instruction: Some("symlinks leaving the root are rejected (FR-32)".to_owned()),
    };
    let joined = project.canonical_root.join(path);
    let canonical = joined.canonicalize().map_err(|err| Error::Project {
        message: format!("cannot open {raw:?}: {err}"),
        instruction: Some("check the path exists inside the project root".to_owned()),
    })?;
    if !crate::platform::paths::is_under(&canonical, &project.canonical_root) {
        return Err(outside_root());
    }
    if !canonical.is_file() {
        return Err(Error::Project {
            message: format!("{raw:?} is not a file"),
            instruction: None,
        });
    }
    if !canonical
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err(Error::Project {
            message: format!("{raw:?} is not a markdown document"),
            instruction: Some("only `.md` documents are exposed".to_owned()),
        });
    }
    let metadata = std::fs::metadata(&canonical).map_err(|err| Error::Project {
        message: format!("cannot stat {}: {err}", canonical.display()),
        instruction: None,
    })?;
    if metadata.len() > config.max_file_size {
        return Err(Error::Project {
            message: format!(
                "{raw:?} is {} bytes, over max_file_size {}",
                metadata.len(),
                config.max_file_size
            ),
            instruction: Some(
                "raise max_file_size in .docsbase.toml to read larger documents".to_owned(),
            ),
        });
    }
    let content = std::fs::read_to_string(&canonical).map_err(|err| Error::Project {
        message: format!("cannot read {}: {err}", canonical.display()),
        instruction: None,
    })?;
    let rel = crate::index::walk::portable_rel(
        canonical
            .strip_prefix(&project.canonical_root)
            .unwrap_or(Path::new(raw)),
    );
    Ok(json!({
        "path": rel,
        "content": content,
        "size": content.len(),
        "lines": content.lines().count(),
    }))
}

/// `read_neighbors`: chunks around `chunk_id` inside its document (FR-24).
///
/// `before`/`after` default to 1 and are capped at [`NEIGHBOR_CAP`] each.
///
/// # Errors
/// Returns [`Error::Project`] for unknown chunk ids and [`Error::Protocol`]
/// for malformed arguments.
pub fn read_neighbors(db: &Db, project: &Project, args: &Value) -> Result<Value> {
    ensure_indexed(project)?;
    let raw = args
        .get("chunk_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Protocol {
            message: format!(
                "read_neighbors: chunk_id must be a non-negative integer, got {:?}",
                args.get("chunk_id")
            ),
        })?;
    let side = |key: &str| -> Result<i64> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(1),
            Some(value) => {
                let parsed = value.as_u64().ok_or_else(|| Error::Protocol {
                    message: format!(
                        "read_neighbors: {key} must be a non-negative integer, got {value}"
                    ),
                })?;
                Ok(i64::try_from(parsed.min(NEIGHBOR_CAP)).unwrap_or(i64::MAX))
            }
        }
    };
    let before = side("before")?;
    let after = side("after")?;
    let (doc_id, seq) = chunk_id_parts(raw);
    let from = i64::from(seq).saturating_sub(before);
    let to = i64::from(seq).saturating_add(after);
    let rows = repo::chunk_window(db.connection(), project.id, doc_id, from, to)?;
    if !rows.iter().any(|row| row.seq == seq) {
        return Err(Error::Project {
            message: format!("unknown chunk_id {raw} for project '{}'", project.name),
            instruction: Some("use search_docs to obtain chunk ids".to_owned()),
        });
    }
    let chunks: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "chunk_id": chunk_id(doc_id, row.seq),
                "seq": row.seq,
                "path": row.path,
                "heading_path": row.heading_path,
                "lines": [row.line_start, row.line_end],
                "kind": row.kind,
                "text": row.text,
            })
        })
        .collect();
    Ok(Value::Array(chunks))
}

/// Per-side cap for [`read_neighbors`] payloads.
pub const NEIGHBOR_CAP: u64 = 100;

/// `list_projects`: registry entries with statuses (FR-10, FR-25).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
pub fn list_projects(db: &Db) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        projects.push(project_entry(db, &project, None, false)?);
    }
    Ok(Value::Array(projects))
}

/// `status`: registry-wide snapshot with runtime counters and versions (FR-26).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
pub fn status(
    db: &Db,
    stats: &Stats,
    watchers: u64,
    restart_notice: Option<&str>,
    watching: &dyn Fn(i64) -> bool,
    coverage: &dyn Fn(&Project) -> Option<Coverage>,
    dense: Option<&DenseCache>,
) -> Result<Value> {
    let mut projects = Vec::new();
    for project in registry::list_projects(db)? {
        let mut entry = project_entry(db, &project, Some(watching(project.id)), true)?;
        entry["root_state"] = json!(registry::root_state(&project.canonical_root).as_str());
        entry["index_generation"] = json!(repo::index_generation(db.connection(), project.id)?);
        let hybrid_on = coverage(&project).is_some();
        entry["embed_coverage"] = match coverage(&project) {
            Some(cov) => json!({ "cached": cov.cached, "total": cov.total }),
            None => Value::Null,
        };
        entry["ann"] = ann_state(dense, project.id, hybrid_on);
        projects.push(entry);
    }
    let hint = projects
        .is_empty()
        .then_some("run `docsbase index` in your project");
    Ok(json!({
        "protocol_version": PROTOCOL_VERSION,
        "build_id": build_id(),
        "schema_version": db.schema_version()?,
        "projects": projects,
        "query_cache": query_cache_json(dense),
        "sessions": stats.sessions,
        "fd_count": stats.fd_count,
        "threads": stats.threads,
        "watchers": watchers,
        "hint": hint,
        "restart_required": restart_notice.is_some(),
        "notice": restart_notice,
    }))
}

fn project_entry(
    db: &Db,
    project: &Project,
    watched: Option<bool>,
    with_git: bool,
) -> Result<Value> {
    let counts = repo::project_counts(db.connection(), project.id)?;
    // The newest job's own warnings and error are surfaced whatever its state:
    // a dropped watcher batch records its lost paths here (FR-19), while a
    // failed full job carries `error` and no warnings. An older job's warnings
    // never leak through a newer row (FR-26).
    let (warnings, last_error) = match repo::latest_job(db.connection(), project.id)? {
        Some((_state, Some(raw))) => {
            let stats = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
            (
                stats
                    .get("warnings")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
                stats.get("error").cloned(),
            )
        }
        _ => (Value::Array(Vec::new()), None),
    };
    let mut entry = json!({
        "id": project.id,
        "name": project.name,
        "root": project.canonical_root,
        "status": project.status.as_str(),
        "docs": counts.docs,
        "chunks": counts.chunks,
        "last_indexed_at": project.last_indexed_at,
        "warnings": warnings,
    });
    if let Some(error) = last_error {
        entry["last_error"] = error;
    }
    if let Some(watched) = watched {
        entry["watched"] = json!(watched);
    }
    if with_git && let Some(git) = crate::vcs::state(&project.canonical_root) {
        entry["git"] = json!({
            "branch": git.branch,
            "head": git.head,
            "detached": git.detached,
            "is_worktree": git.is_worktree,
        });
    }
    Ok(entry)
}

/// Creates a `queued` sync job for `project`; when an active job already
/// exists, returns its id with `false` (FR-17).
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures.
pub fn enqueue_sync(db: &Db, project: &Project) -> Result<(i64, bool)> {
    for _ in 0..3 {
        if let Some(id) = repo::create_sync_job(db.connection(), project.id)? {
            // Best effort: a prune failure must not strand a queued job with
            // no runner blocking later syncs (FR-17).
            if let Err(err) = repo::prune_sync_jobs(db.connection(), project.id) {
                eprintln!("warning: cannot prune sync jobs: {err}");
            }
            return Ok((id, true));
        }
        // The blocking insert lost to an active job; it may finish before the
        // select (its runner uses another connection), so retry the pair.
        if let Some(active) = repo::active_sync_job(db.connection(), project.id)? {
            return Ok((active.id, false));
        }
    }
    Err(Error::internal(format!(
        "sync jobs for project {} are racing repeatedly",
        project.id
    )))
}

/// Runs a queued job to completion on its own database connection (daemon
/// background task, FR-17); job state survives restarts.
/// Compatibility adapter; runtime uses [`execute_sync_job_with_settings`].
///
/// Setup failures (missing project, unreadable config) also record `error`,
/// so the project never keeps a stuck `queued` job that would block FR-17
/// syncs; only an unopenable registry leaves the row untouched.
///
/// # Errors
/// Propagates setup and indexing errors after persisting them.
pub fn execute_sync_job(
    cache: &Path,
    job_id: i64,
    project_id: i64,
    config: &Config,
) -> Result<Project> {
    execute_sync_job_with_settings(
        cache,
        job_id,
        project_id,
        config,
        &crate::store::settings::standalone()?,
    )
}

/// Runs a background job with the owning daemon's resolved storage policy.
///
/// # Errors
/// Returns the setup/indexing errors described by [`execute_sync_job`].
pub fn execute_sync_job_with_settings(
    cache: &Path,
    job_id: i64,
    project_id: i64,
    config: &Config,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> Result<Project> {
    let mut db = Db::open_with_settings(cache, settings)?;
    let setup = (|| -> Result<Project> {
        registry::project_by_id(&db, project_id)?.ok_or_else(|| Error::Project {
            message: format!("project {project_id} disappeared from the registry"),
            instruction: None,
        })
    })();
    let outcome = match setup {
        Ok(project) => run_sync_job_in(&mut db, cache, job_id, &project, config),
        Err(err) => {
            let stats = json!({ "error": err.to_string() }).to_string();
            if let Err(record) =
                repo::finish_sync_job(db.connection(), job_id, SyncState::Error, Some(&stats))
            {
                eprintln!("warning: cannot record sync job {job_id} failure: {record}");
            }
            Err(err)
        }
    };
    let final_project =
        registry::project_by_id(&db, project_id)?.ok_or_else(|| Error::Project {
            message: format!("project {project_id} disappeared after job {job_id}"),
            instruction: None,
        })?;
    outcome.map(|_| final_project)
}

/// Marks `job_id` running, runs the full index and records the terminal
/// state with statistics (FR-17).
///
/// # Errors
/// Propagates indexing errors after persisting them in `sync_jobs`.
pub fn run_sync_job_in(
    db: &mut Db,
    cache: &Path,
    job_id: i64,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    repo::mark_sync_running(db.connection(), job_id)?;
    // After the index pass, a full embedding sweep over every project chunk
    // completes the coverage a previous offline period left behind (FR-15);
    // cached chunks cost nothing, so this is a no-op once covered.
    let outcome = run_project_index(db, cache, project, config).and_then(|stats| {
        hybrid_jobs::embed_project_chunks(db, &config.hybrid_cfg, &config.providers, project.id)?;
        Ok(stats)
    });
    let (state, stats_json) = match &outcome {
        Ok(stats) => (SyncState::Done, serde_json::to_string(stats).ok()),
        Err(err) => (
            SyncState::Error,
            Some(json!({ "error": err.to_string() }).to_string()),
        ),
    };
    if let Err(finish_err) =
        repo::finish_sync_job(db.connection(), job_id, state, stats_json.as_deref())
    {
        eprintln!("warning: cannot finish sync job {job_id}: {finish_err}");
    }
    outcome
}

/// `sync_status(job_id)`: persistent job state and statistics (FR-17).
///
/// # Errors
/// Returns [`Error::Protocol`] when `job_id` is missing and
/// [`Error::Project`] for an unknown id.
pub fn sync_status(db: &Db, args: &Value) -> Result<Value> {
    let job_id = match args.get("job_id") {
        Some(value) => value.as_i64().ok_or_else(|| Error::Protocol {
            message: format!("sync_status: job_id must be an integer, got {value}"),
        })?,
        None => {
            return Err(Error::Protocol {
                message: "sync_status: missing job_id".to_owned(),
            });
        }
    };
    let job = repo::sync_job(db.connection(), job_id)?.ok_or_else(|| Error::Project {
        message: format!("unknown sync job id {job_id}"),
        instruction: None,
    })?;
    Ok(sync_job_value(&job))
}

/// JSON view of a sync job row.
#[must_use]
pub fn sync_job_value(job: &SyncJob) -> Value {
    json!({
        "job_id": job.id,
        "project_id": job.project_id,
        "state": job.state.as_str(),
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "stats": job
            .stats_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
    })
}

/// Registers (by the caller) and indexes `project` under the per-project
/// writer lease, updating statuses (FR-11, I5).
///
/// # Errors
/// Propagates indexing and status errors.
pub fn run_project_index(
    db: &mut Db,
    cache: &Path,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    lifecycle::with_writer_lease(cache, project.id, || {
        registry::set_status(db, project.id, ProjectStatus::Indexing)?;
        let outcome = (|| -> Result<JobStats> {
            let mut index = crate::index::tantivy_index::IndexHandle::open_or_create(
                &lifecycle::index_dir(cache, project.id),
            )?;
            run_full(db, &mut index, project, config)
        })();
        match outcome {
            Ok(stats) => {
                registry::mark_indexed(db, project.id)?;
                Ok(stats)
            }
            Err(err) => {
                if let Err(status_err) = registry::set_status(db, project.id, ProjectStatus::Error)
                {
                    eprintln!(
                        "warning: cannot mark project {} as error: {status_err}",
                        project.id
                    );
                }
                Err(err)
            }
        }
    })
}

fn ensure_indexed(project: &Project) -> Result<()> {
    if project.status != ProjectStatus::Indexed {
        return Err(Error::Project {
            message: format!(
                "project '{}' is not indexed (status: {})",
                project.name,
                project.status.as_str()
            ),
            instruction: Some("run `docsbase index`".to_owned()),
        });
    }
    if project.schema_version != migrations::SCHEMA_VERSION {
        return Err(Error::Project {
            message: format!(
                "project '{}' indexes use schema {} while this build expects {}",
                project.name,
                project.schema_version,
                migrations::SCHEMA_VERSION
            ),
            instruction: Some("run `docsbase index` to rebuild".to_owned()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    use crate::hybrid::Reranker;
    use crate::index::job::run_full;
    use crate::index::tantivy_index::IndexHandle;
    use crate::store::models::ProjectStatus;
    use crate::vector_cache::Embedder;

    /// Reranker that reverses the candidates, scores by rank and records the
    /// documents it saw (FR-11 contract; SC-2).
    #[derive(Default)]
    struct ReverseReranker {
        calls: AtomicUsize,
        top_ns: std::sync::Mutex<Vec<usize>>,
        documents: std::sync::Mutex<Vec<String>>,
    }

    impl Reranker for ReverseReranker {
        #[allow(clippy::cast_precision_loss)]
        fn rerank(&self, _query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.top_ns.lock().expect("top_ns").push(top_n);
            self.documents
                .lock()
                .expect("documents")
                .extend(docs.iter().cloned());
            let n = docs.len().min(top_n);
            Ok((0..n)
                .map(|rank| (docs.len() - 1 - rank, (n - rank) as f32))
                .collect())
        }
    }

    const DOCS: &[(&str, &str)] = &[
        ("a.md", "# A\n\nwidgets alpha text about gizmos.\n"),
        ("b.md", "# B\n\nwidgets beta text about sprockets.\n"),
        ("c.md", "# C\n\nwidgets gamma text about cogs.\n"),
    ];

    fn env() -> (TempDir, Db, crate::store::models::Project) {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in DOCS {
            fs::write(root.path().join(rel), body).expect("write doc");
        }
        let mut db = Db::open(cache.path()).expect("db");
        let canonical_root = root.path().canonicalize().expect("canonical root");
        let mut project = registry::ensure_project(&mut db, &canonical_root).expect("project");
        let mut index =
            IndexHandle::open_or_create(&lifecycle::index_dir(cache.path(), project.id))
                .expect("index");
        run_full(&mut db, &mut index, &project, &Config::default()).expect("index run");
        // Persist the same indexed state that production binding reads.
        registry::mark_indexed(&db, project.id).expect("persist indexed status");
        project.status = ProjectStatus::Indexed;
        (cache, db, project)
    }

    fn chunk_ids(value: &Value) -> Vec<u64> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|row| row["chunk_id"].as_u64().expect("chunk_id"))
            .collect()
    }

    /// Spy embedder: records whether `embed_query`/`embed` were called and
    /// returns fixed vectors (FR-10, SC-9).
    #[derive(Default)]
    struct SpyEmbedder {
        query_calls: AtomicUsize,
        passage_calls: AtomicUsize,
    }

    impl Embedder for SpyEmbedder {
        fn model_id(&self) -> &'static str {
            "spy/model@d3@l2"
        }

        #[allow(clippy::cast_precision_loss)]
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.passage_calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|text| vec![text.len() as f32, 0.0, 0.0])
                .collect())
        }

        #[allow(clippy::cast_precision_loss)]
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            self.query_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![text.len() as f32, 0.0, 0.0])
        }
    }

    #[test]
    fn dense_search_path_uses_embed_query() {
        let (_cache, db, project) = env();
        let spy = Arc::new(SpyEmbedder::default());
        let stages = HybridStages {
            embedder: Some(spy.clone()),
            reranker: None,
            ranking: Ranking::Rrf,
        };
        let dense = DenseCache::new();
        let mut degraded = Vec::new();
        let policy = crate::search::settings::standalone().expect("search policy");
        let budget = PayloadBudget::new(policy.settings().clone()).expect("budget");
        let context = SearchContext {
            stages: &stages,
            dense: &dense,
            cache: db.cache_root(),
            project: &project,
            window: 10,
            payload_budget: &budget,
            operation: SearchOperation::new(
                policy.settings(),
                std::time::Instant::now(),
                Cancellation::new(),
            )
            .expect("operation"),
        };
        let hits = dense_candidates(
            &context,
            MetadataSource::Borrowed(&db),
            &|| Ok(()),
            DenseQuery {
                text: "widgets",
                generation: repo::index_generation(db.connection(), project.id)
                    .expect("generation"),
                fetch: 10,
            },
            &mut degraded,
        )
        .expect("dense candidates");
        // The query path must go through `embed_query` (registry prefixes),
        // never through the passage `embed`.
        assert_eq!(spy.query_calls.load(Ordering::SeqCst), 1);
        assert_eq!(spy.passage_calls.load(Ordering::SeqCst), 0);
        // With an empty cache and exact-only dense, no hits are expected but
        // the call must succeed.
        assert!(hits.len() <= 10);
    }

    #[test]
    fn rerank_reorders_hits_and_replaces_scores() {
        let (cache, db, project) = env();
        let args = json!({ "query": "widgets", "limit": 3 });

        let bm25 = search_docs_with(
            &HybridStages::default(),
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &args,
            DEFAULT_RERANK_WINDOW,
        )
        .expect("bm25 search");
        let bm25_ids = chunk_ids(&bm25);
        assert_eq!(bm25_ids.len(), 3);

        let reranker = Arc::new(ReverseReranker::default());
        let stages = HybridStages {
            embedder: None,
            reranker: Some(reranker.clone()),
            ranking: Ranking::Lexical,
        };
        let result = search_docs_with(
            &stages,
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &args,
            50,
        )
        .expect("reranked search");
        let result_ids = chunk_ids(&result);
        let mut expected = bm25_ids.clone();
        expected.reverse();
        assert_eq!(result_ids, expected, "provider order replaces BM25 order");
        assert_eq!(reranker.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            reranker.documents.lock().expect("documents").len(),
            3,
            "every candidate is sent once; window clamps to the hit count"
        );

        let scores: Vec<f64> = result
            .as_array()
            .expect("array")
            .iter()
            .map(|row| row["score"].as_f64().expect("score"))
            .collect();
        assert_eq!(scores, vec![3.0, 2.0, 1.0], "score = relevance_score");
        for row in result.as_array().expect("array") {
            assert!(row["path"].is_string(), "citations survive the rerank");
        }
    }

    #[test]
    fn rerank_false_keeps_bm25_order_and_skips_the_provider() {
        let (cache, db, project) = env();
        let bm25 = search_docs_with(
            &HybridStages::default(),
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &json!({ "query": "widgets", "limit": 3 }),
            DEFAULT_RERANK_WINDOW,
        )
        .expect("bm25 search");

        let reranker = Arc::new(ReverseReranker::default());
        let stages = HybridStages {
            embedder: None,
            reranker: Some(reranker.clone()),
            ranking: Ranking::Lexical,
        };
        let value = search_docs_with(
            &stages,
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &json!({ "query": "widgets", "limit": 3, "rerank": false }),
            50,
        )
        .expect("strict bm25");
        assert_eq!(chunk_ids(&value), chunk_ids(&bm25));
        assert_eq!(reranker.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rerank_ranks_the_whole_fetch_window_even_above_the_config_window() {
        let (cache, db, project) = env();
        let reranker = Arc::new(ReverseReranker::default());
        let stages = HybridStages {
            embedder: None,
            reranker: Some(reranker.clone()),
            ranking: Ranking::Lexical,
        };
        let value = search_docs_with(
            &stages,
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &json!({ "query": "widgets", "limit": 3 }),
            2,
        )
        .expect("limit above the config window");
        assert_eq!(value.as_array().expect("array").len(), 3);
        assert_eq!(
            *reranker.top_ns.lock().expect("top_ns"),
            vec![3],
            "top_n = fetch = max(limit, window)"
        );
    }

    #[test]
    fn rerank_true_without_a_stage_is_a_protocol_error() {
        let (cache, db, project) = env();
        let error = search_docs_with(
            &HybridStages::default(),
            &DenseCache::new(),
            &db,
            cache.path(),
            &project,
            &json!({ "query": "widgets", "rerank": true }),
            DEFAULT_RERANK_WINDOW,
        )
        .expect_err("explicit rerank without a stage must fail");
        assert!(
            matches!(&error, Error::Protocol { message } if message.contains("rerank")),
            "{error}"
        );
    }
}
