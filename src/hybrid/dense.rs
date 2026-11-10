//! Dense candidate generation: cosine top-`fetch` over cached vectors (FR-15).
//!
//! The scan is intentionally brute-force: iteration 2 selects the best
//! `fetch` chunks by cosine similarity against the query embedding; iteration
//! 3 (spec ann-index) adds a persisted ANN path for projects above
//! `ANN_MIN_VECTORS`, keeping exact fallback on any ANN failure (FR-5).
//! Every stored vector is scored with a true cosine (division by both norms),
//! which degenerates to the dot product for the L2-normalized vectors
//! produced by the default embedder configuration, so the same code serves
//! `normalize = false`.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::hybrid::ann::{self, ANN_MIN_VECTORS, AnnIndex};
use crate::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};
use crate::hybrid::dense_metadata::DenseMetadata;
use crate::hybrid::flight::Flights;
use crate::hybrid::meta::{AnnOutcome, AnnState};
use crate::hybrid::work::Cancellation;
use crate::store::Db;
use crate::store::repo;
use crate::vector_cache::{CacheLimits, VectorCache};

#[cfg(test)]
#[path = "query_flight_tests.rs"]
mod query_flight_tests;

#[cfg(test)]
#[path = "dense_cold_tests.rs"]
mod cold_tests;

/// Hard per-project cap on cached vectors (DELTA-7): above it the cache
/// refuses to build and callers stream the scan instead.
pub const DENSE_CACHE_MAX_VECTORS: usize = 1_000_000;

/// Longest-lived query-vector cache entries (`search-cost` FR-2).
pub const QUERY_CACHE_MAX_ENTRIES: usize = 256;

/// Byte budget for cached query vectors (`search-cost` FR-2).
pub const QUERY_CACHE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Backoff after a failed background ANN build before retrying.
const BUILD_BACKOFF: Duration = Duration::from_secs(30);

/// Whether a background ANN build for `count` vectors of width `dim` fits the
/// resident cap: the graph estimate plus the exact rows the build thread keeps
/// alive. Mirrors the admission check in [`DenseCache::resolve_rows`].
#[must_use]
pub fn ann_build_admitted(count: usize, dim: usize, max_bytes: u64) -> bool {
    let exact = u64::try_from(count.saturating_mul(dim).saturating_mul(4)).unwrap_or(u64::MAX);
    let estimated = u64::try_from(ann::ann_estimated_bytes(count, dim)).unwrap_or(u64::MAX);
    estimated.saturating_add(exact) <= max_bytes
}

/// [`ann_build_admitted`] for a cache model id that encodes the width
/// (`...@d2048@l2`). Returns `false` when the width is absent.
#[must_use]
pub fn ann_build_admitted_for_model(count: usize, model_id: &str, max_bytes: u64) -> bool {
    model_dim(model_id).is_some_and(|dim| ann_build_admitted(count, dim, max_bytes))
}

/// Embedding width encoded in a cache model id (`...@d2048@l2`), when present.
fn model_dim(model_id: &str) -> Option<usize> {
    let (_, after) = model_id.rsplit_once("@d")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|dim| *dim > 0)
}

/// Key of the persisted ANN for one project/generation/model.
fn ann_key(
    project_id: i64,
    generation: i64,
    model_id: &str,
    dim: usize,
    chunk_count: usize,
    digest: String,
) -> AnnKey {
    AnnKey {
        project_id,
        generation,
        model_id: model_id.to_owned(),
        dim,
        chunk_count,
        chunk_set_digest: digest,
        format_version: ANN_FORMAT_VERSION,
    }
}

/// Returns the best `fetch` candidates by cosine similarity, best first.
///
/// Ties are broken deterministically: equal scores order by ascending
/// `chunk_id`. Zero-length or non-finite inputs score `0.0` instead of
/// poisoning the ranking; a dimension mismatch between the query and a
/// candidate is an internal invariant violation (the cache key includes the
/// embedding dimensions).
///
/// # Errors
/// [`Error::Internal`] when the query vector is empty or a candidate has a
/// different dimension.
pub fn top_k<V: AsRef<[f32]>>(
    query: &[f32],
    rows: &[(u64, V)],
    fetch: usize,
) -> Result<Vec<(u64, f32)>> {
    if query.is_empty() {
        return Err(Error::internal("dense query vector is empty"));
    }
    if fetch == 0 || rows.is_empty() {
        return Ok(Vec::new());
    }
    let query_norm = norm(query);
    let mut scored: Vec<(u64, f32)> = Vec::with_capacity(rows.len());
    for (chunk_id, vector) in rows {
        let vector = vector.as_ref();
        if vector.len() != query.len() {
            return Err(Error::internal(format!(
                "dense candidate vector dim {} does not match query dim {} (chunk {chunk_id})",
                vector.len(),
                query.len()
            )));
        }
        scored.push((*chunk_id, cosine(query_norm, query, vector)));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(fetch);
    Ok(scored)
}

/// Cosine similarity; zero-length or non-finite inputs collapse to `0.0`
/// instead of poisoning the ranking with `NaN`.
fn cosine(query_norm: f32, query: &[f32], vector: &[f32]) -> f32 {
    if query_norm == 0.0 {
        return 0.0;
    }
    let vector_norm = norm(vector);
    if vector_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = query.iter().zip(vector).map(|(a, b)| a * b).sum();
    let score = dot / (query_norm * vector_norm);
    if score.is_finite() { score } else { 0.0 }
}

fn norm(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// Streaming variant of [`top_k`] for projects above the cache cap: rows are
/// consumed one by one and only the best `fetch` candidates stay in memory
/// (a bounded binary heap), so a scan never allocates proportional to the
/// project size.
///
/// # Errors
/// Propagates row errors and [`Error::Internal`] for an empty query or a
/// dimension mismatch, exactly like [`top_k`].
pub fn top_k_iter<I>(query: &[f32], rows: I, fetch: usize) -> Result<Vec<(u64, f32)>>
where
    I: Iterator<Item = Result<(u64, Vec<f32>)>>,
{
    if query.is_empty() {
        return Err(Error::internal("dense query vector is empty"));
    }
    if fetch == 0 {
        return Ok(Vec::new());
    }
    let query_norm = norm(query);
    let mut heap: BinaryHeap<Worst> = BinaryHeap::new();
    for row in rows {
        let (chunk_id, vector) = row?;
        if vector.len() != query.len() {
            return Err(Error::internal(format!(
                "dense candidate vector dim {} does not match query dim {} (chunk {chunk_id})",
                vector.len(),
                query.len()
            )));
        }
        let candidate = Worst {
            score: cosine(query_norm, query, &vector),
            chunk_id,
        };
        if heap.len() < fetch {
            heap.push(candidate);
            continue;
        }
        if let Some(victim) = heap.peek()
            && candidate < *victim
        {
            heap.pop();
            heap.push(candidate);
        }
    }
    let mut out: Vec<(u64, f32)> = heap
        .into_iter()
        .map(|row| (row.chunk_id, row.score))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(out)
}

/// Heap ordering where `peek()` is the eviction victim: the lowest score,
/// and on ties the highest `chunk_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Worst {
    score: f32,
    chunk_id: u64,
}

impl Eq for Worst {}

impl Ord for Worst {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.chunk_id.cmp(&other.chunk_id))
    }
}

impl PartialOrd for Worst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Per-project dense rows for a fixed `(index generation, model)` pair.
#[derive(Debug)]
pub struct DenseIndex {
    generation: i64,
    model_id: String,
    rows: Vec<(u64, Arc<[f32]>)>,
}

impl DenseIndex {
    /// Index generation the rows were built for (DELTA-7).
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// Cache key of the embedding stage the rows were built with (FR-5).
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// `(chunk_id, vector)` rows with vectors only, deterministic order.
    #[must_use]
    pub fn rows(&self) -> &[(u64, Arc<[f32]>)] {
        &self.rows
    }
}

/// In-flight ANN build handle: while it is `Building`, search streams exact
/// `top_k_iter` from the cached hashes and never blocks on the build.
#[derive(Debug, Clone)]
pub struct BuildingAnn {
    pub(crate) hashes: Arc<Vec<(u64, String)>>,
    pub(crate) bytes: u64,
}

/// Failed ANN build: search streams exact from the cached hashes until
/// `retry_at`, then a new background build starts (FR-5/FR-12).
#[derive(Debug, Clone)]
pub struct FailedAnn {
    pub(crate) hashes: Arc<Vec<(u64, String)>>,
    pub(crate) retry_at: Instant,
}

/// Outcome of a dense-cache lookup.
#[derive(Debug, Clone)]
pub enum DenseRows {
    /// Rows for the current index generation (fresh or cached).
    Ready(Arc<DenseIndex>),
    /// A loaded ANN index for the current key (persisted).
    Ann(Arc<AnnIndex>),
    /// Rows do not fit the vector or available byte budget (including live
    /// builds); stream [`top_k_iter`] over the cached hashes.
    TooLarge(Arc<Vec<(u64, String)>>),
    /// An ANN build is running in the background; search streams exact.
    Building(Arc<BuildingAnn>),
    /// The last ANN build failed; stream exact, retry after `retry_at`.
    Failed(Arc<FailedAnn>),
}

/// ANN build/load policy (design §8, FR-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnPolicy {
    /// Load if present, otherwise build in the background (daemon).
    BuildOrLoad,
    /// Load if present, never build (direct CLI).
    LoadOnly,
    /// Never touch ANN (tests/explicit fallback).
    Disabled,
}

/// Memory/disk caps for the dense cache (FR-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenseCacheLimits {
    /// Maximum vectors per project; above it the project streams exact.
    pub max_vectors: usize,
    /// Maximum charged bytes across resident entries and every live build,
    /// including its transient graph+rows peak even after entry eviction.
    /// The default admits the
    /// documented 75k x 2048 corpus (691 MB graph + 586 MB rows = 1.22 GiB).
    pub max_bytes: u64,
    /// Maximum cached projects; oldest entries are evicted (LRU).
    pub max_projects: usize,
    /// ANN is considered only at or above this vector count (OQ-3, T6
    /// calibrates; defaults to [`ANN_MIN_VECTORS`]).
    pub min_vectors: usize,
    /// Backoff after a failed background build before the next retry.
    pub build_backoff: Duration,
    /// Vector-cache byte budget used by this dense path (DELTA-9).
    pub vector_cache_max_bytes: u64,
    /// Global ANN dump budget (T9).
    pub max_ann_disk_bytes: u64,
}

impl Default for DenseCacheLimits {
    fn default() -> Self {
        Self {
            max_vectors: DENSE_CACHE_MAX_VECTORS,
            // 75k x 2048 needs 1.22 GiB while a background build keeps the
            // exact rows alive; 1.5 GiB leaves headroom without changing the
            // steady-state ANN model (691 MB, see NFR-4).
            max_bytes: 1536 * 1024 * 1024,
            max_projects: 8,
            min_vectors: ANN_MIN_VECTORS,
            build_backoff: BUILD_BACKOFF,
            vector_cache_max_bytes: CacheLimits::default().max_bytes,
            max_ann_disk_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

/// One cached project entry.
struct Entry {
    state: DenseRows,
    generation: i64,
    model_id: String,
    last_used: u64,
    bytes: u64,
}

struct CachedDigest {
    digest: String,
    last_used: u64,
}

type DigestCache = HashMap<(i64, i64), CachedDigest>;

/// Charged until the worker drops its working set, independently of LRU or
/// generation replacement. Reservations are acquired under the entries lock;
/// dropping one can only reduce the charge and needs no entries lock.
struct BuildReservation {
    bytes: u64,
    total: Arc<AtomicU64>,
}

impl BuildReservation {
    fn new(total: Arc<AtomicU64>, bytes: u64) -> Self {
        total.fetch_add(bytes, AtomicOrdering::AcqRel);
        Self { bytes, total }
    }

    fn release(&mut self) {
        self.total
            .fetch_sub(std::mem::take(&mut self.bytes), AtomicOrdering::AcqRel);
    }
}

impl Drop for BuildReservation {
    fn drop(&mut self) {
        self.release();
    }
}

/// Everything a background ANN build needs (design §8).
struct BuildJob {
    project_id: i64,
    generation: i64,
    model_id: String,
    rows: Vec<(u64, Arc<[f32]>)>,
    hashes: Arc<Vec<(u64, String)>>,
    key: AnnKey,
    /// Graph estimate plus the exact rows the build thread keeps alive
    /// (Delta 0.3; FR-7/NFR-4 count the whole resident working set).
    building_bytes: u64,
    dim: usize,
    store_root: Option<PathBuf>,
}

/// Rows/hashes loaded for one lookup, before the ANN exact/build decision.
struct DenseInput {
    chunks: Vec<(u64, String)>,
    digest: String,
    rows: Vec<(u64, Arc<[f32]>)>,
    dim: usize,
    full_coverage: bool,
}

/// Query-vector cache (`search-cost`): a vector is a pure function of
/// `(model_id, text)`, so a repeated query needs no provider call. Bounded by
/// entries and bytes, evicting least-recently-used first.
///
/// `(model_id, blake3(query))` — the cache key.
type QueryKey = (String, String);

#[derive(Default)]
struct QueryCache {
    entries: HashMap<QueryKey, (std::sync::Arc<[f32]>, u64)>,
    order: std::collections::VecDeque<QueryKey>,
    bytes: u64,
    hits: u64,
    misses: u64,
    flights: Arc<Flights<QueryKey, Arc<[f32]>>>,
}

/// Explicit identity for lazy dense initialization. The loader must supply a
/// snapshot with this root, project and generation; model identity selects the
/// corresponding vectors within that snapshot.
#[derive(Debug, Clone, Copy)]
pub struct DenseRequest<'a> {
    /// Database/vector-cache root, already resolved by the caller.
    pub cache_root: &'a Path,
    /// Registry project identity.
    pub project_id: i64,
    /// Required metadata generation.
    pub generation: i64,
    /// Full embedding identity, including dimension and normalization.
    pub model_id: &'a str,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ColdKey {
    root: PathBuf,
    project_id: i64,
    generation: i64,
    model_id: String,
}

#[derive(Clone, Copy)]
enum Lookup {
    Get,
    Warm,
}

/// Per-project dense cache keyed by `(index generation, model_id)` with an
/// optional ANN tier (spec ann-index): a new generation or embedding model
/// replaces the entry, a no-op sync keeps it.
pub struct DenseCache {
    limits: DenseCacheLimits,
    policy: AnnPolicy,
    store: Option<AnnStore>,
    bound_root: Arc<Mutex<Option<PathBuf>>>,
    cold_flights: Arc<Flights<ColdKey, DenseRows>>,
    entries: Arc<Mutex<HashMap<i64, Entry>>>,
    active_build_bytes: Arc<AtomicU64>,
    digests: Arc<Mutex<DigestCache>>,
    vector_cache_limits: Arc<Mutex<HashMap<i64, u64>>>,
    outcomes: Arc<Mutex<HashMap<i64, AnnOutcome>>>,
    query_cache: Arc<Mutex<QueryCache>>,
    clock: Arc<AtomicU64>,
    #[cfg(test)]
    test_build_gate: Option<TestBuildGate>,
    #[cfg(test)]
    test_workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

/// Counters of the query-vector cache (`status.query_cache`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryCacheStats {
    /// Lookups served without the provider.
    pub hits: u64,
    /// Lookups not served by the completed cache (including coalesced misses).
    pub misses: u64,
    /// Cached vectors.
    pub entries: usize,
    /// Bytes held by cached vectors.
    pub bytes: u64,
}

impl Default for DenseCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DenseCache {
    /// Cache with default limits and ANN disabled (exact-only, as before the
    /// ANN iteration). Daemon/CLI opt into ANN via [`Self::with_policy`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_policy_and_limits(AnnPolicy::Disabled, None, DenseCacheLimits::default(), None)
    }

    /// Cache with an explicit vector cap (tests and future tuning).
    #[must_use]
    pub fn with_cap(cap: usize) -> Self {
        Self::with_policy_and_limits(
            AnnPolicy::Disabled,
            None,
            DenseCacheLimits {
                max_vectors: cap,
                ..DenseCacheLimits::default()
            },
            None,
        )
    }

    /// Cache with an ANN policy and store root (daemon/CLI; design §10).
    #[must_use]
    pub fn with_policy(policy: AnnPolicy, cache_root: &std::path::Path) -> Self {
        let store =
            (policy != AnnPolicy::Disabled).then(|| AnnStore::new(cache_root.to_path_buf()));
        Self::with_policy_and_limits(policy, store, DenseCacheLimits::default(), Some(cache_root))
    }

    /// Full constructor used by tests.
    #[must_use]
    pub fn with_policy_limits(
        policy: AnnPolicy,
        cache_root: &std::path::Path,
        limits: DenseCacheLimits,
    ) -> Self {
        let store =
            (policy != AnnPolicy::Disabled).then(|| AnnStore::new(cache_root.to_path_buf()));
        Self::with_policy_and_limits(policy, store, limits, Some(cache_root))
    }

    fn with_policy_and_limits(
        policy: AnnPolicy,
        store: Option<AnnStore>,
        limits: DenseCacheLimits,
        root: Option<&Path>,
    ) -> Self {
        Self {
            limits,
            policy,
            store,
            bound_root: Arc::new(Mutex::new(root.map(Path::to_path_buf))),
            cold_flights: Arc::new(Flights::default()),
            entries: Arc::new(Mutex::new(HashMap::new())),
            active_build_bytes: Arc::new(AtomicU64::new(0)),
            digests: Arc::new(Mutex::new(HashMap::new())),
            vector_cache_limits: Arc::new(Mutex::new(HashMap::new())),
            outcomes: Arc::new(Mutex::new(HashMap::new())),
            query_cache: Arc::new(Mutex::new(QueryCache::default())),
            clock: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            test_build_gate: None,
            #[cfg(test)]
            test_workers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Records what the ANN tier did for the last search of `project_id`
    /// (FR-6). One record per project; the oldest is dropped at the project cap.
    pub fn record_outcome(&self, project_id: i64, state: AnnState, reason: Option<&'static str>) {
        let outcome = AnnOutcome {
            state,
            reason,
            at: std::time::Instant::now(),
        };
        let mut outcomes = self.lock_outcomes();
        if outcomes.len() >= self.limits.max_projects
            && !outcomes.contains_key(&project_id)
            && let Some((oldest, _)) = outcomes
                .iter()
                .min_by_key(|(_, record)| record.at)
                .map(|(id, record)| (*id, *record))
        {
            outcomes.remove(&oldest);
        }
        outcomes.insert(project_id, outcome);
    }

    /// Last recorded ANN outcome for `project_id`, if any.
    #[must_use]
    pub fn outcome(&self, project_id: i64) -> Option<AnnOutcome> {
        self.lock_outcomes().get(&project_id).copied()
    }

    /// Resident ANN size for `project_id`: `(vectors, estimated_bytes)`.
    #[must_use]
    pub fn ann_snapshot(&self, project_id: i64) -> Option<(usize, u64)> {
        let entries = self.lock_entries();
        match &entries.get(&project_id)?.state {
            DenseRows::Ann(ann) => Some((
                ann.count(),
                u64::try_from(ann.estimated_bytes()).unwrap_or(u64::MAX),
            )),
            _ => None,
        }
    }

    /// Cached query vector for `(model_id, text)`, counting a hit or a miss
    /// (`search-cost` FR-1/FR-3). A miss does not insert: the caller embeds and
    /// calls [`Self::store_query_vector`], so a failed embed is never cached.
    #[must_use]
    pub fn query_vector(&self, model_id: &str, text: &str) -> Option<std::sync::Arc<[f32]>> {
        let key = (model_id.to_owned(), crate::vector_cache::chunk_hash(text));
        let mut cache = self.lock_query_cache();
        let cached = cache
            .entries
            .get(&key)
            .map(|(vector, _)| std::sync::Arc::clone(vector));
        if let Some(vector) = cached {
            cache.hits += 1;
            // Refresh recency: move the key to the back of the order queue.
            if let Some(position) = cache.order.iter().position(|entry| entry == &key) {
                cache.order.remove(position);
            }
            cache.order.push_back(key);
            return Some(vector);
        }
        cache.misses += 1;
        None
    }

    /// A completed cached query vector or one shared in-flight embedding.
    /// Cancellation affects this caller, not other live participants; an
    /// already running synchronous provider call is allowed to finish.
    ///
    /// # Errors
    /// Returns caller cancellation or the provider error with its category and
    /// source chain. Leader unwind wakes waiters but keeps unwinding itself.
    pub fn query_embedding(
        &self,
        embedder: &dyn crate::vector_cache::Embedder,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Arc<[f32]>> {
        cancellation.check()?;
        let model_id = embedder.model_id();
        if let Some(cached) = self.query_vector(model_id, text) {
            cancellation.check()?;
            return Ok(cached);
        }
        let key = (model_id.to_owned(), crate::vector_cache::chunk_hash(text));
        let flights = Arc::clone(&self.lock_query_cache().flights);
        flights.run(
            &key,
            cancellation,
            || {
                self.lock_query_cache()
                    .entries
                    .get(&key)
                    .map(|(vector, _)| Arc::clone(vector))
            },
            || {
                let vector: Arc<[f32]> = Arc::from(crate::hybrid::blocking::enter(|| {
                    embedder.embed_query(text)
                })?);
                let bytes =
                    u64::try_from(std::mem::size_of_val(vector.as_ref())).unwrap_or(u64::MAX);
                Ok(self
                    .cache_query(key.clone(), bytes, || Arc::clone(&vector))
                    .unwrap_or(vector))
            },
        )
    }

    /// Stores a query vector, evicting least-recently-used entries until both
    /// limits hold (`search-cost` FR-2).
    pub fn store_query_vector(&self, model_id: &str, text: &str, vector: &[f32]) {
        let key = (model_id.to_owned(), crate::vector_cache::chunk_hash(text));
        let bytes = u64::try_from(std::mem::size_of_val(vector)).unwrap_or(u64::MAX);
        self.cache_query(key, bytes, || Arc::from(vector));
    }

    fn cache_query(
        &self,
        key: QueryKey,
        bytes: u64,
        make: impl FnOnce() -> Arc<[f32]>,
    ) -> Option<Arc<[f32]>> {
        if bytes == 0 || bytes > QUERY_CACHE_MAX_BYTES {
            return None;
        }
        let mut cache = self.lock_query_cache();
        if let Some((vector, _)) = cache.entries.get(&key) {
            return Some(Arc::clone(vector));
        }
        let arc = make();
        while (!cache.entries.is_empty() && cache.entries.len() >= QUERY_CACHE_MAX_ENTRIES)
            || cache.bytes.saturating_add(bytes) > QUERY_CACHE_MAX_BYTES
        {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            if let Some((_, evicted_bytes)) = cache.entries.remove(&oldest) {
                cache.bytes = cache.bytes.saturating_sub(evicted_bytes);
            }
        }
        cache.bytes = cache.bytes.saturating_add(bytes);
        cache.order.push_back(key.clone());
        cache.entries.insert(key, (Arc::clone(&arc), bytes));
        Some(arc)
    }

    /// A cache with fresh row/ANN state that **shares** this one's query cache
    /// (`search-cost` SC-4): a benchmark run can reuse query vectors across
    /// configurations without also warming chunk rows, which would corrupt the
    /// cold-scan latency it measures.
    #[must_use]
    pub fn sharing_query_cache(&self) -> Self {
        Self {
            limits: self.limits,
            policy: self.policy,
            store: self.store.clone(),
            bound_root: Arc::new(Mutex::new(
                self.bound_root
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            )),
            cold_flights: Arc::new(Flights::default()),
            entries: Arc::new(Mutex::new(HashMap::new())),
            active_build_bytes: Arc::new(AtomicU64::new(0)),
            digests: Arc::new(Mutex::new(HashMap::new())),
            vector_cache_limits: Arc::new(Mutex::new(HashMap::new())),
            outcomes: Arc::new(Mutex::new(HashMap::new())),
            query_cache: Arc::clone(&self.query_cache),
            clock: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            test_build_gate: None,
            #[cfg(test)]
            test_workers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Snapshot of the query-vector cache counters (`search-cost` FR-4).
    #[must_use]
    pub fn query_cache_stats(&self) -> QueryCacheStats {
        let cache = self.lock_query_cache();
        QueryCacheStats {
            hits: cache.hits,
            misses: cache.misses,
            entries: cache.entries.len(),
            bytes: cache.bytes,
        }
    }

    fn lock_query_cache(&self) -> std::sync::MutexGuard<'_, QueryCache> {
        self.query_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_outcomes(&self) -> std::sync::MutexGuard<'_, HashMap<i64, AnnOutcome>> {
        self.outcomes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Digest of the current chunk set for `(project, generation)`, computed
    /// once and reused until the generation changes (design §8).
    fn digest_for(&self, project_id: i64, generation: i64, chunks: &[(u64, String)]) -> String {
        let key = (project_id, generation);
        {
            let mut digests = self.digests.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(cached) = digests.get_mut(&key) {
                cached.last_used = self.tick();
                return cached.digest.clone();
            }
        }
        let digest = crate::hybrid::ann_store::chunk_set_digest(chunks);
        let mut digests = self.digests.lock().unwrap_or_else(PoisonError::into_inner);
        // An older lookup may finish after a newer generation. Return its
        // digest to that caller without retaining retired history.
        if self.limits.max_projects == 0
            || digests
                .keys()
                .any(|(project, newer)| *project == project_id && *newer > generation)
        {
            return digest;
        }
        digests.retain(|(project, _), _| *project != project_id);
        if digests.len() >= self.limits.max_projects
            && let Some(oldest) = digests
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| *key)
        {
            digests.remove(&oldest);
        }
        digests.insert(
            key,
            CachedDigest {
                digest: digest.clone(),
                last_used: self.tick(),
            },
        );
        digest
    }

    /// Returns the project's rows (or ANN state), rebuilding when the index
    /// generation moved.
    ///
    /// # Errors
    /// Registry failures (generation, chunk rows) and vector-cache failures.
    pub fn get(&self, db: &Db, project_id: i64, model_id: &str) -> Result<DenseRows> {
        let generation = repo::index_generation(db.connection(), project_id)?;
        self.get_with_metadata(
            DenseRequest {
                cache_root: db.cache_root(),
                project_id,
                generation,
                model_id,
            },
            &Cancellation::new(),
            || DenseMetadata::capture(db, project_id),
        )
    }

    /// Returns cached state or coalesces cold initialization before invoking
    /// `load`. The loader owns its short SQL lease and must release it before
    /// returning the owned snapshot. Waiters never invoke their loaders.
    ///
    /// # Errors
    /// Cancellation, mismatched snapshot/cache identity, metadata or vector IO.
    pub fn get_with_metadata(
        &self,
        request: DenseRequest<'_>,
        cancellation: &Cancellation,
        load: impl FnOnce() -> Result<DenseMetadata>,
    ) -> Result<DenseRows> {
        self.access(request, cancellation, Lookup::Get, load)
    }

    fn bind_root(&self, root: &Path) -> Result<()> {
        let mut bound = self
            .bound_root
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(bound) = bound.as_ref() {
            if bound != root {
                return Err(Error::Admission {
                    message: "dense cache belongs to a different cache root".to_owned(),
                });
            }
        } else {
            *bound = Some(root.to_path_buf());
        }
        Ok(())
    }

    fn cached_rows(&self, request: DenseRequest<'_>, lookup: Lookup) -> Option<DenseRows> {
        let mut entries = self.lock_entries();
        let entry = entries.get_mut(&request.project_id)?;
        if entry.generation != request.generation || entry.model_id != request.model_id {
            return None;
        }
        if matches!(lookup, Lookup::Get) {
            entry.last_used = self.tick();
        }
        let ready = match &entry.state {
            DenseRows::Ann(_) | DenseRows::Building(_) => true,
            DenseRows::Failed(failed) => failed.retry_at > Instant::now(),
            DenseRows::Ready(_) | DenseRows::TooLarge(_) => matches!(lookup, Lookup::Get),
        };
        ready.then(|| entry.state.clone())
    }

    fn access(
        &self,
        request: DenseRequest<'_>,
        cancellation: &Cancellation,
        lookup: Lookup,
        load: impl FnOnce() -> Result<DenseMetadata>,
    ) -> Result<DenseRows> {
        cancellation.check()?;
        self.bind_root(request.cache_root)?;
        if let Some(rows) = self.cached_rows(request, lookup) {
            cancellation.check()?;
            return Ok(rows);
        }
        let key = ColdKey {
            root: request.cache_root.to_path_buf(),
            project_id: request.project_id,
            generation: request.generation,
            model_id: request.model_id.to_owned(),
        };
        self.cold_flights.run(
            &key,
            cancellation,
            || self.cached_rows(request, lookup),
            || {
                let metadata = load()?;
                if metadata.cache_root() != request.cache_root
                    || metadata.project_id() != request.project_id
                    || metadata.generation() != request.generation
                {
                    return Err(Error::Admission {
                        message: "dense metadata snapshot does not match the requested identity"
                            .to_owned(),
                    });
                }
                self.resolve(request, metadata.into_chunks())
            },
        )
    }

    /// Full lookup path: exact rows, ANN load or background build.
    fn resolve(&self, request: DenseRequest<'_>, chunks: Vec<(u64, String)>) -> Result<DenseRows> {
        let DenseRequest {
            project_id,
            generation,
            model_id,
            ..
        } = request;
        let digest = self.digest_for(project_id, generation, &chunks);
        if chunks.len() > self.limits.max_vectors {
            return Ok(self.stash_too_large(project_id, generation, model_id, chunks));
        }
        // Refuse to materialise exact rows we already know exceed the byte
        // cap. The model id carries the width (`...@d2048@l2`), so this check
        // usually happens before any vector is decoded (FR-7/NFR-4).
        if let Some(dim) = model_dim(model_id)
            && let Some(exact_bytes) = chunks
                .len()
                .checked_mul(dim)
                .and_then(|bytes| bytes.checked_mul(4))
        {
            let exact_bytes = u64::try_from(exact_bytes).unwrap_or(u64::MAX);
            if exact_bytes > self.limits.max_bytes {
                return Ok(self.stash_too_large(project_id, generation, model_id, chunks));
            }
            if exact_bytes.saturating_add(self.active_build_bytes.load(AtomicOrdering::Acquire))
                > self.limits.max_bytes
            {
                return Ok(self.stream_transient(
                    project_id,
                    generation,
                    model_id,
                    Arc::new(chunks),
                ));
            }
        }
        // Cold start must not read every vector just to discover that a valid
        // persisted ANN already exists (FR-8/NFR-3). The model id carries the
        // width, so the key can be built before VectorCache is opened.
        if self.policy != AnnPolicy::Disabled
            && self.store.is_some()
            && chunks.len() >= self.limits.min_vectors
            && let Some(dim) = model_dim(model_id)
            && u64::try_from(ann::ann_estimated_bytes(chunks.len(), dim)).unwrap_or(u64::MAX)
                <= self.limits.max_bytes
        {
            let key = ann_key(
                project_id,
                generation,
                model_id,
                dim,
                chunks.len(),
                digest.clone(),
            );
            if let Some(store) = &self.store
                && let Ok(Some(slot)) = store.load(&key)
            {
                match AnnIndex::load(&slot) {
                    Ok(ann) => {
                        let state = DenseRows::Ann(Arc::new(ann));
                        if let Some(retained) =
                            self.stash(project_id, generation, model_id, state.clone())
                        {
                            return Ok(retained);
                        }
                        drop(state);
                        return Ok(self.stream_transient(
                            project_id,
                            generation,
                            model_id,
                            Arc::new(chunks),
                        ));
                    }
                    Err(err) => {
                        eprintln!("warning: ann load failed: {err}; falling back to exact");
                    }
                }
            }
        }
        let input = self.load_rows(request, chunks, digest)?;
        Ok(self.resolve_rows(project_id, generation, model_id, input))
    }

    fn load_rows(
        &self,
        request: DenseRequest<'_>,
        chunks: Vec<(u64, String)>,
        digest: String,
    ) -> Result<DenseInput> {
        let vector_cache_max_bytes = self.vector_cache_max_bytes(request.project_id);
        let mut cache = VectorCache::open_with_limits(
            request.cache_root,
            CacheLimits {
                max_bytes: vector_cache_max_bytes,
                ..CacheLimits::default()
            },
        )?;
        let mut rows = Vec::with_capacity(chunks.len());
        let mut full_coverage = true;
        for (chunk_id, hash) in &chunks {
            match cache.get(request.model_id, hash)? {
                Some(vector) => rows.push((*chunk_id, Arc::from(vector))),
                None => full_coverage = false,
            }
        }
        let dim = rows
            .first()
            .map_or(0, |(_, vector): &(u64, Arc<[f32]>)| vector.len());
        Ok(DenseInput {
            chunks,
            digest,
            rows,
            dim,
            full_coverage,
        })
    }

    /// ANN/exact decision for already-loaded rows.
    fn resolve_rows(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        input: DenseInput,
    ) -> DenseRows {
        let DenseInput {
            chunks,
            digest,
            rows,
            dim,
            full_coverage,
        } = input;
        let exact_bytes = rows
            .len()
            .checked_mul(dim)
            .and_then(|bytes| bytes.checked_mul(4))
            .and_then(|bytes| u64::try_from(bytes).ok())
            .unwrap_or(u64::MAX);
        let estimated = ann::ann_estimated_bytes(rows.len(), dim);
        let (in_backoff, other_resident) = {
            let entries = self.lock_entries();
            let in_backoff = matches!(entries.get(&project_id), Some(entry)
                if entry.generation == generation
                    && entry.model_id == model_id
                    && matches!(&entry.state, DenseRows::Failed(failed) if failed.retry_at > Instant::now()));
            // A build reservation must fit next to the projects already
            // resident, otherwise two admitted corpora would double the cap.
            let other = self.charged_bytes(&entries, Some(project_id));
            (in_backoff, other)
        };
        let building_bytes = u64::try_from(estimated)
            .unwrap_or(u64::MAX)
            .saturating_add(exact_bytes);
        let seek_ann = self.policy != AnnPolicy::Disabled
            && self.store.is_some()
            && chunks.len() >= self.limits.min_vectors
            && full_coverage
            && !in_backoff
            && building_bytes <= self.limits.max_bytes
            && building_bytes.saturating_add(other_resident) <= self.limits.max_bytes
            && ann_build_admitted(chunks.len(), dim, self.limits.max_bytes);
        if !seek_ann {
            if exact_bytes > self.limits.max_bytes {
                return self.stash_too_large(project_id, generation, model_id, chunks);
            }
            return self.stash_ready(project_id, generation, model_id, rows, Arc::new(chunks));
        }
        let key = ann_key(project_id, generation, model_id, dim, chunks.len(), digest);
        if let Some(store) = &self.store
            && let Ok(Some(slot)) = store.load(&key)
        {
            match AnnIndex::load(&slot) {
                Ok(ann) => {
                    let state = DenseRows::Ann(Arc::new(ann));
                    drop(rows);
                    if let Some(retained) =
                        self.stash(project_id, generation, model_id, state.clone())
                    {
                        return retained;
                    }
                    drop(state);
                    return self.stream_transient(
                        project_id,
                        generation,
                        model_id,
                        Arc::new(chunks),
                    );
                }
                Err(err) => {
                    eprintln!("warning: ann load failed: {err}; falling back to exact");
                }
            }
        }
        if self.policy == AnnPolicy::BuildOrLoad {
            let hashes = Arc::new(chunks);
            let store_root = self.store.as_ref().map(|store| store.root().to_path_buf());
            self.start_build(BuildJob {
                project_id,
                generation,
                model_id: model_id.to_owned(),
                rows,
                hashes,
                key,
                building_bytes,
                dim,
                store_root,
            })
        } else {
            self.stash_ready(project_id, generation, model_id, rows, Arc::new(chunks))
        }
    }

    fn stash_ready(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        rows: Vec<(u64, Arc<[f32]>)>,
        hashes: Arc<Vec<(u64, String)>>,
    ) -> DenseRows {
        let state = DenseRows::Ready(Arc::new(DenseIndex {
            generation,
            model_id: model_id.to_owned(),
            rows,
        }));
        if let Some(retained) = self.stash(project_id, generation, model_id, state.clone()) {
            retained
        } else {
            // Live builds cannot be reclaimed by evicting their entries. Do
            // not keep another full row copy when their reservation fills
            // the cap; exact streaming needs only the chunk hashes.
            drop(state);
            self.stream_transient(project_id, generation, model_id, hashes)
        }
    }

    fn stash_too_large(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        chunks: Vec<(u64, String)>,
    ) -> DenseRows {
        let state = DenseRows::TooLarge(Arc::new(chunks));
        self.stash(project_id, generation, model_id, state.clone())
            .unwrap_or(state)
    }

    /// A lookup already decided that reservation pressure prevents retention.
    /// Keep that decision even if its last worker finishes before this call.
    fn stream_transient(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        hashes: Arc<Vec<(u64, String)>>,
    ) -> DenseRows {
        self.existing_ann(&mut self.lock_entries(), project_id, generation, model_id)
            .unwrap_or(DenseRows::TooLarge(hashes))
    }

    /// Starts one background build for `(project, generation, model)` and
    /// publishes the `Building` state; a build already in flight is reused
    /// (design §8).
    #[allow(clippy::needless_pass_by_value)]
    fn start_build(&self, job: BuildJob) -> DenseRows {
        let building = Arc::new(BuildingAnn {
            hashes: Arc::clone(&job.hashes),
            bytes: job.building_bytes,
        });
        {
            let mut entries = self.lock_entries();
            if entries
                .get(&job.project_id)
                .is_some_and(|entry| entry.generation > job.generation)
            {
                // This request can finish from its own rows, but it cannot
                // replace the newer slot or start an obsolete ANN worker.
                drop(entries);
                return self.stash_ready(
                    job.project_id,
                    job.generation,
                    &job.model_id,
                    job.rows,
                    job.hashes,
                );
            }
            if let Some(entry) = entries.get(&job.project_id)
                && entry.generation == job.generation
                && entry.model_id == job.model_id
                && matches!(entry.state, DenseRows::Building(_) | DenseRows::Ann(_))
            {
                return entry.state.clone();
            }
            // Re-check aggregate admission while holding the entries lock:
            // two concurrent lookups must not both insert a reservation from a
            // stale snapshot (Delta 0.3.2; the resolve_rows check is a fast
            // path only). Falling back to exact keeps the cap honest.
            let others = self.charged_bytes(&entries, Some(job.project_id));
            if self.limits.max_projects == 0
                || job.building_bytes.saturating_add(others) > self.limits.max_bytes
            {
                drop(entries);
                return self.stash_ready(
                    job.project_id,
                    job.generation,
                    &job.model_id,
                    job.rows,
                    job.hashes,
                );
            }
            let reservation =
                BuildReservation::new(Arc::clone(&self.active_build_bytes), job.building_bytes);
            let cache = Arc::new(self.snapshot());
            let entry = Entry {
                state: DenseRows::Building(Arc::clone(&building)),
                generation: job.generation,
                model_id: job.model_id.clone(),
                last_used: self.tick(),
                bytes: building.bytes,
            };
            if entries.len() >= self.limits.max_projects && !entries.contains_key(&job.project_id) {
                self.evict_oldest(&mut entries, Some(job.project_id));
            }
            entries.insert(job.project_id, entry);
            self.enforce_byte_cap(&mut entries, job.project_id);
            let build_state = Arc::clone(&building);
            let spawn = move || build_in_background(cache, job, build_state, reservation);
            let worker = std::thread::spawn(spawn);
            #[cfg(test)]
            self.test_workers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(worker);
            #[cfg(not(test))]
            drop(worker);
        }
        DenseRows::Building(building)
    }

    /// Idempotent background warm for `(project, model)`: starts a build if
    /// none exists for the current generation and returns immediately
    /// (design §8). Search keeps streaming exact while it runs.
    ///
    /// # Errors
    /// Registry or vector-cache failures on the way to a build.
    pub fn warm(&self, db: &Db, project_id: i64, model_id: &str) -> Result<()> {
        let generation = repo::index_generation(db.connection(), project_id)?;
        self.warm_with_metadata(
            DenseRequest {
                cache_root: db.cache_root(),
                project_id,
                generation,
                model_id,
            },
            &Cancellation::new(),
            || DenseMetadata::capture(db, project_id),
        )
    }

    /// Warms the requested identity with lazy metadata capture. Ready/TooLarge
    /// coverage is rechecked; existing ANN/build/backoff states need no loader.
    ///
    /// # Errors
    /// Cancellation, mismatched snapshot/cache identity, metadata or vector IO.
    pub fn warm_with_metadata(
        &self,
        request: DenseRequest<'_>,
        cancellation: &Cancellation,
        load: impl FnOnce() -> Result<DenseMetadata>,
    ) -> Result<()> {
        self.access(request, cancellation, Lookup::Warm, load)
            .map(|_| ())
    }

    fn vector_cache_max_bytes(&self, project_id: i64) -> u64 {
        self.vector_cache_limits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&project_id)
            .copied()
            .unwrap_or(self.limits.vector_cache_max_bytes)
    }

    /// Applies a project-level vector-cache budget (daemon config reload).
    pub fn set_vector_cache_max_bytes(&self, project_id: i64, max_bytes: u64) {
        self.vector_cache_limits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(project_id, max_bytes);
    }

    /// Invalidates a project: drops the cached entry/digest and the on-disk
    /// ANN tree (FR-4 lifecycle helper).
    ///
    /// Backfill staleness is handled by the `AnnKey` (generation/digest/model),
    /// so this is reserved for project deletion; no caller exists yet.
    ///
    /// # Errors
    /// Filesystem failures while removing the ANN tree.
    pub fn invalidate(&self, project_id: i64) -> Result<()> {
        self.lock_entries().remove(&project_id);
        self.digests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(project, _), _| *project != project_id);
        if let Some(store) = &self.store {
            store.invalidate(project_id)?;
        }
        Ok(())
    }

    /// Best-effort cleanup of old ANN generations (design §9).
    ///
    /// # Errors
    /// Filesystem failures while scanning the ann root.
    pub fn cleanup(&self, project_id: i64) -> Result<()> {
        if let Some(store) = &self.store {
            store.cleanup(project_id)?;
        }
        Ok(())
    }

    /// Best-effort global ANN disk budget (T9).
    ///
    /// # Errors
    /// Filesystem failures while scanning the cache root.
    pub fn cleanup_disk(&self, project_id: i64) -> Result<()> {
        if let Some(store) = &self.store {
            store.cleanup_disk(self.limits.max_ann_disk_bytes, Some(project_id))?;
        }
        Ok(())
    }

    fn stash(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        state: DenseRows,
    ) -> Option<DenseRows> {
        if self.limits.max_projects == 0 {
            return None;
        }
        let bytes = match &state {
            DenseRows::Ann(ann) => ann.estimated_bytes() as u64,
            DenseRows::Ready(index) => index
                .rows()
                .iter()
                .map(|(_, vector)| vector.len() * 4)
                .sum::<usize>() as u64,
            DenseRows::TooLarge(hashes) => hashes.len() as u64 * 64,
            DenseRows::Building(building) => building.bytes,
            DenseRows::Failed(failed) => failed.hashes.len() as u64 * 64,
        };
        let mut entries = self.lock_entries();
        // Another cold lookup may have installed/published this build after
        // get's fast-path miss. Never retire it with exact fallback or reload.
        if let Some(existing) = self.existing_ann(&mut entries, project_id, generation, model_id) {
            return Some(existing);
        }
        let active = self.active_build_bytes.load(AtomicOrdering::Acquire);
        if bytes.saturating_add(active) > self.limits.max_bytes {
            // Reservation pressure is temporary. Leave this fallback uncached
            // so get retries once a worker finishes. Evicting Building entries
            // here would not free their separately owned working sets.
            return None;
        }
        if entries
            .get(&project_id)
            .is_some_and(|entry| entry.generation > generation)
        {
            // Preserve the caller's coherent generation without publishing it
            // over newer state. Existing memory/disabled-cache refusal above
            // still takes priority over keeping these caller-owned rows.
            return Some(state);
        }
        let entry = Entry {
            state,
            generation,
            model_id: model_id.to_owned(),
            last_used: self.tick(),
            bytes,
        };
        if entries.len() >= self.limits.max_projects && !entries.contains_key(&project_id) {
            self.evict_oldest(&mut entries, Some(project_id));
        }
        entries.insert(project_id, entry);
        self.enforce_byte_cap(&mut entries, project_id);
        entries.get(&project_id).map(|entry| entry.state.clone())
    }

    fn existing_ann(
        &self,
        entries: &mut HashMap<i64, Entry>,
        project_id: i64,
        generation: i64,
        model_id: &str,
    ) -> Option<DenseRows> {
        let entry = entries.get_mut(&project_id)?;
        if entry.generation != generation
            || entry.model_id != model_id
            || !matches!(entry.state, DenseRows::Building(_) | DenseRows::Ann(_))
        {
            return None;
        }
        entry.last_used = self.tick();
        Some(entry.state.clone())
    }

    /// Building entries are views of a separately charged worker, not an
    /// additional copy of its working set. Include retired/evicted workers.
    fn charged_bytes(&self, entries: &HashMap<i64, Entry>, replacing: Option<i64>) -> u64 {
        entries
            .iter()
            .filter(|(id, entry)| {
                Some(**id) != replacing && !matches!(entry.state, DenseRows::Building(_))
            })
            .fold(
                self.active_build_bytes.load(AtomicOrdering::Acquire),
                |total, (_, entry)| total.saturating_add(entry.bytes),
            )
    }

    /// Drops the least-recently-used entry (optionally skipping `keep`).
    fn evict_oldest(&self, entries: &mut HashMap<i64, Entry>, keep: Option<i64>) -> Option<i64> {
        let pick = |only_idle: bool| {
            entries
                .iter()
                .filter(|(id, _)| Some(**id) != keep)
                // Prefer entries whose memory is actually reclaimable: an
                // in-flight build keeps its rows and graph alive after the
                // entry is gone (Delta 0.3.2).
                .filter(|(_, entry)| !only_idle || !matches!(entry.state, DenseRows::Building(_)))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(id, _)| *id)
        };
        let oldest = pick(true).or_else(|| pick(false))?;
        self.digests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(project, _), _| *project != oldest);
        entries.remove(&oldest);
        Some(oldest)
    }

    /// Live workers remain charged after eviction. If even the newest entry
    /// cannot coexist with them, leave it uncached and let rows stream.
    fn enforce_byte_cap(&self, entries: &mut HashMap<i64, Entry>, keep: i64) {
        loop {
            if self.charged_bytes(entries, None) <= self.limits.max_bytes {
                return;
            }
            if self
                .evict_oldest(entries, Some(keep))
                .or_else(|| self.evict_oldest(entries, None))
                .is_none()
            {
                return;
            }
        }
    }

    /// Snapshot of the cache sharing its entries, digests and clock; used by
    /// background builds to publish results (design §8).
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            limits: self.limits,
            policy: self.policy,
            store: self.store.clone(),
            bound_root: Arc::clone(&self.bound_root),
            cold_flights: Arc::clone(&self.cold_flights),
            entries: Arc::clone(&self.entries),
            active_build_bytes: Arc::clone(&self.active_build_bytes),
            digests: Arc::clone(&self.digests),
            vector_cache_limits: Arc::clone(&self.vector_cache_limits),
            outcomes: Arc::clone(&self.outcomes),
            query_cache: Arc::clone(&self.query_cache),
            clock: Arc::clone(&self.clock),
            #[cfg(test)]
            test_build_gate: self.test_build_gate.clone(),
            #[cfg(test)]
            test_workers: Arc::clone(&self.test_workers),
        }
    }
}

/// Test-only hook: holds background builds at their start until the test opens
/// the gate, so "the search ran while the build was still in flight" (SC-11)
/// is a structural guarantee instead of a race against a fixed sleep. A sleep
/// cannot do this: on a loaded runner the search itself outlasts any constant
/// (CI: 29.5 s for the same test that takes 11 s locally). Absent in release
/// builds.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct TestBuildGate(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

#[cfg(any(test, debug_assertions))]
impl TestBuildGate {
    /// A closed gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Releases every build waiting at this gate.
    pub fn open(&self) {
        let (lock, ready) = &*self.0;
        if let Ok(mut open) = lock.lock() {
            *open = true;
        }
        ready.notify_all();
    }

    fn wait(&self) {
        let (lock, ready) = &*self.0;
        if let Ok(open) = lock.lock() {
            let _ = ready.wait_timeout_while(open, Duration::from_secs(120), |open| !*open);
        }
    }
}

#[cfg(debug_assertions)]
static TEST_BUILD_GATE: std::sync::Mutex<Option<TestBuildGate>> = std::sync::Mutex::new(None);

/// Test-only hook: installs (or clears) the build gate. Absent in release
/// builds.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_test_build_gate(gate: Option<TestBuildGate>) {
    if let Ok(mut slot) = TEST_BUILD_GATE.lock() {
        *slot = gate;
    }
}

#[cfg(debug_assertions)]
fn test_build_delay() {
    let gate = TEST_BUILD_GATE.lock().ok().and_then(|slot| slot.clone());
    let Some(gate) = gate else {
        return;
    };
    // A panicking test must not wedge a build thread forever.
    gate.wait();
}

#[cfg(not(debug_assertions))]
fn test_build_delay() {}

/// Background build: builds, dumps, loads and publishes the `Ann` entry
/// unless the project moved on meanwhile (design §8). Any panic from the
/// underlying index is caught and treated like a build failure (FR-5).
#[allow(clippy::needless_pass_by_value)]
fn build_in_background(
    cache: Arc<DenseCache>,
    job: BuildJob,
    building: Arc<BuildingAnn>,
    reservation: BuildReservation,
) {
    // Declare the guard before the working-set locals: on unwind or stale
    // completion, rows/loaded graphs must drop before their charge is freed.
    let mut reservation = reservation;
    let BuildJob {
        project_id,
        key,
        rows,
        hashes,
        dim,
        store_root,
        ..
    } = job;
    let Some(store_root) = store_root else {
        return;
    };
    #[cfg(test)]
    if let Some(gate) = &cache.test_build_gate {
        gate.wait();
    }
    test_build_delay();
    let vector_root = store_root.clone();
    let unpin_hashes = Arc::clone(&hashes);
    // The build returns the loaded index instead of publishing it: the lease
    // must be released *before* the ANN becomes visible. Publishing first left
    // a window in which the vectors DB was still open (lease release re-opens
    // it) while callers already saw `Ann`, so a purge racing the tail of a
    // build hit a Windows sharing violation on `vectors.db` (CI run
    // 41043371926, `cold_start_loads_persisted_ann_without_vectors_db`).
    let result: Result<(Arc<AnnIndex>, u64)> = catch_unwind(AssertUnwindSafe(|| {
        let ann = AnnIndex::build(&rows, dim)?;
        let store = AnnStore::new(store_root);
        let slot = ann.dump(&store, &key)?;
        // Loading must not hold the source rows, the built graph and the loaded
        // copy at once: on the heap-reload path that would add a third full
        // copy (~1.86 GiB at 75k x 2048) on top of the admitted peak.
        let published_bytes = ann.estimated_bytes() as u64;
        drop(ann);
        drop(rows);
        let loaded = AnnIndex::load(&slot)?;
        Ok((Arc::new(loaded), published_bytes))
    }))
    .unwrap_or_else(|payload| Err(Error::internal(format!("ann build panic: {payload:?}"))));
    if result.is_ok() {
        // The dump is READY: release the backfill lease. On failure the pins
        // stay until their TTL so the retry still sees full coverage. Do this
        // without holding the entries lock (the vector DB may be busy).
        let max_bytes = cache.vector_cache_max_bytes(project_id);
        if let Ok(mut vectors) = VectorCache::open_with_limits(
            &vector_root,
            CacheLimits {
                max_bytes,
                ..CacheLimits::default()
            },
        ) {
            let hashes: Vec<String> = unpin_hashes.iter().map(|(_, hash)| hash.clone()).collect();
            if let Err(err) = vectors.unpin(project_id, &key.model_id, &hashes) {
                eprintln!("warning: cannot release ann vector lease: {err}");
            }
        }
    }
    // Publish last: once `Ann` is visible the build thread holds no
    // `vectors.db` handle, so a purge can delete it immediately.
    let mut entries = cache.lock_entries();
    match result {
        Ok((loaded, published_bytes)) => {
            if let Some(entry) = entries.get_mut(&project_id)
                && entry.generation == key.generation
                && entry.model_id == key.model_id
                && matches!(&entry.state, DenseRows::Building(current) if Arc::ptr_eq(current, &building))
            {
                entry.state = DenseRows::Ann(loaded);
                entry.bytes = published_bytes;
                entry.last_used = cache.tick();
                // Transfer ownership and charge together under the entries
                // lock; the worker keeps no graph reference after publication.
                reservation.release();
            }
        }
        Err(err) => {
            eprintln!("warning: ann build failed for project {project_id}: {err}; keeping exact");
            if let Some(entry) = entries.get_mut(&project_id)
                && entry.generation == key.generation
                && entry.model_id == key.model_id
                && matches!(&entry.state, DenseRows::Building(current) if Arc::ptr_eq(current, &building))
            {
                entry.bytes = hashes.len() as u64 * 64;
                entry.state = DenseRows::Failed(Arc::new(FailedAnn {
                    hashes,
                    retry_at: Instant::now() + cache.limits.build_backoff,
                }));
                reservation.release();
            }
        }
    }
    cache.enforce_byte_cap(&mut entries, project_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_cache::Embedder;

    fn dense_input(rows: Vec<(u64, Arc<[f32]>)>, dim: usize) -> DenseInput {
        let chunks = rows.iter().map(|(id, _)| (*id, format!("h{id}"))).collect();
        DenseInput {
            chunks,
            digest: "digest".to_owned(),
            rows,
            dim,
            full_coverage: true,
        }
    }

    fn wide_rows(count: usize, dim: usize) -> Vec<(u64, Arc<[f32]>)> {
        (0..count)
            .map(|id| {
                (
                    u64::try_from(id).expect("id"),
                    Arc::from(vec![0.5_f32; dim]),
                )
            })
            .collect()
    }

    struct PausedBuilds {
        cache: DenseCache,
    }

    impl PausedBuilds {
        fn new(root: &std::path::Path, max_projects: usize, budget_builds: u64) -> Self {
            let per_build =
                u64::try_from(ann::ann_estimated_bytes(4, 128)).expect("bytes") + 4 * 128 * 4;
            Self::from_limits(
                root,
                DenseCacheLimits {
                    min_vectors: 1,
                    max_bytes: per_build * budget_builds + 1024,
                    max_projects,
                    ..DenseCacheLimits::default()
                },
            )
        }

        fn from_limits(root: &std::path::Path, limits: DenseCacheLimits) -> Self {
            let mut cache = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, root, limits);
            cache.test_build_gate = Some(TestBuildGate::new());
            Self { cache }
        }

        fn finish(&self) {
            self.cache.test_build_gate.as_ref().expect("gate").open();
            let workers = std::mem::take(
                &mut *self
                    .cache
                    .test_workers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            for worker in workers {
                let joined = worker.join();
                if !std::thread::panicking() {
                    joined.expect("worker must not panic");
                }
            }
            if !std::thread::panicking() {
                assert_eq!(
                    self.cache.active_build_bytes.load(AtomicOrdering::Acquire),
                    0,
                    "joined workers must release every reservation"
                );
            }
        }
    }

    impl Drop for PausedBuilds {
        fn drop(&mut self) {
            // Join even after a failed assertion so no worker outlives its
            // cache directory or contaminates another parallel test.
            self.finish();
        }
    }

    fn small_input() -> DenseInput {
        dense_input(wide_rows(4, 128), 128)
    }

    #[test]
    fn stale_generation_cannot_replace_cached_rows() {
        let cache = DenseCache::new();
        let current = cache.stash_ready(
            1,
            2,
            "model",
            wide_rows(1, 3),
            Arc::new(vec![(0, "new".to_owned())]),
        );
        let old = cache.stash_ready(
            1,
            1,
            "model",
            wide_rows(1, 3),
            Arc::new(vec![(0, "old".to_owned())]),
        );
        assert!(matches!(old, DenseRows::Ready(ref rows) if rows.generation() == 1));
        let entries = cache.lock_entries();
        let entry = entries.get(&1).expect("current entry");
        assert_eq!(
            entry.generation, 2,
            "late old completion cannot overwrite newer state"
        );
        match (&current, &entry.state) {
            (DenseRows::Ready(expected), DenseRows::Ready(actual)) => {
                assert!(Arc::ptr_eq(expected, actual));
            }
            _ => panic!("current ready rows changed"),
        }
    }

    #[test]
    fn stale_generation_does_not_start_an_ann_build() {
        let root = tempfile::tempdir().expect("cache");
        let paused = PausedBuilds::new(root.path(), 1, 1);
        let cache = &paused.cache;
        let input = small_input();
        cache.stash_ready(1, 2, "model", input.rows, Arc::new(input.chunks));
        let old = cache.resolve_rows(1, 1, "model", small_input());
        assert!(matches!(old, DenseRows::Ready(ref rows) if rows.generation() == 1));
        assert_eq!(cache.lock_entries().get(&1).expect("current").generation, 2);
        assert!(
            cache.test_workers.lock().expect("workers").is_empty(),
            "obsolete generation must not spawn a worker"
        );
        assert_eq!(cache.active_build_bytes.load(AtomicOrdering::Acquire), 0);
    }

    #[test]
    fn build_lifetime_survives_generation_model_and_lru_replacement() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 1, 1);
        let dense = &paused.cache;
        let input = small_input();
        let retained = Arc::downgrade(&input.rows[0].1);
        assert!(matches!(
            dense.resolve_rows(1, 1, "m@d128", input),
            DenseRows::Building(_)
        ));
        for (project, generation, model) in
            [(1, 2, "m@d128"), (1, 2, "other@d128"), (2, 1, "m@d128")]
        {
            let next = small_input();
            let rejected = Arc::downgrade(&next.rows[0].1);
            let state = dense.resolve_rows(project, generation, model, next);
            assert!(
                matches!(state, DenseRows::TooLarge(_)),
                "reserved memory requires streaming fallback, got {state:?}"
            );
            assert!(
                rejected.upgrade().is_none(),
                "fallback must drop exact rows"
            );
            assert!(retained.upgrade().is_some(), "first worker is still alive");
            // A smaller exact entry can fit and replace the project's old
            // generation/model, or evict it by LRU. Neither frees its worker.
            let mut small = dense_input(wide_rows(1, 128), 128);
            small.full_coverage = false;
            assert!(matches!(
                dense.resolve_rows(project, generation, model, small),
                DenseRows::Ready(_)
            ));
        }
        assert!(dense.lock_entries().get(&1).is_none(), "project 1 evicted");
        paused.finish();
        assert!(
            retained.upgrade().is_none(),
            "finished worker drops its rows"
        );
        assert!(
            dense.ann_snapshot(1).is_none(),
            "stale worker cannot publish"
        );
        assert!(matches!(
            dense.resolve_rows(3, 1, "m@d128", small_input()),
            DenseRows::Building(_)
        ));
    }

    #[test]
    fn build_lifetime_survives_invalidation() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 1);
        let dense = &paused.cache;
        assert!(matches!(
            dense.resolve_rows(1, 1, "m@d128", small_input()),
            DenseRows::Building(_)
        ));
        dense.invalidate(1).expect("invalidate");
        assert!(matches!(
            dense.resolve_rows(2, 1, "m@d128", small_input()),
            DenseRows::TooLarge(_)
        ));
        paused.finish();
        assert!(dense.ann_snapshot(1).is_none());
        assert!(matches!(
            dense.resolve_rows(3, 1, "m@d128", small_input()),
            DenseRows::Building(_)
        ));
    }

    #[test]
    fn build_lifetime_full_budget_streams_before_opening_vector_cache() {
        let (root, db, project_id) = indexed_project();
        let paused = PausedBuilds::new(root.path(), 2, 1);
        let dense = &paused.cache;
        dense.resolve_rows(project_id + 1, 1, "m@d128", small_input());
        let hashes = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
        let exact_bytes = u64::try_from(hashes.len() * 512 * 4).expect("bytes");
        assert!(
            exact_bytes <= dense.limits.max_bytes,
            "rows fit on their own"
        );
        assert!(
            exact_bytes + dense.active_build_bytes.load(AtomicOrdering::Acquire)
                > dense.limits.max_bytes,
            "rows cannot coexist with the gated worker"
        );
        assert!(!root.path().join("vectors.db").exists());
        let state = dense.get(&db, project_id, "m@d512").expect("get");
        assert!(matches!(state, DenseRows::TooLarge(ref actual) if **actual == hashes));
        assert!(
            !root.path().join("vectors.db").exists(),
            "budget fallback must precede opening or scanning the vector cache"
        );
        paused.finish();
        assert!(
            matches!(
                dense.get(&db, project_id, "m@d512").expect("retry"),
                DenseRows::Ready(_)
            ),
            "temporary pressure must not remain cached after the build finishes"
        );
    }

    #[test]
    fn build_lifetime_overlapping_same_key_cold_lookups_reuse_the_worker() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 1);
        // Both lookups have missed get's fast path and loaded their inputs;
        // the second reaches admission after the first installed Building.
        let first_input = small_input();
        let second_input = small_input();
        let states = [
            paused.cache.resolve_rows(1, 1, "m@d128", first_input),
            paused.cache.resolve_rows(1, 1, "m@d128", second_input),
        ];
        assert!(
            matches!(&states, [DenseRows::Building(first), DenseRows::Building(second)] if Arc::ptr_eq(first, second)),
            "same-key lookups must retain and reuse the original worker: {states:?}"
        );
        paused.finish();
        assert!(paused.cache.ann_snapshot(1).is_some());
    }

    #[test]
    fn build_lifetime_same_key_early_budget_fallback_preserves_the_worker() {
        let (root, db, project_id) = indexed_project();
        let embedder = FakeEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        crate::hybrid::jobs::embed_project_chunks_with(&db, &embedder, project_id).expect("embed");
        let count = repo::chunk_hashes(db.connection(), project_id)
            .expect("hashes")
            .len();
        let peak =
            u64::try_from(ann::ann_estimated_bytes(count, 3) + count * 3 * 4).expect("bytes");
        let paused = PausedBuilds::from_limits(
            root.path(),
            DenseCacheLimits {
                min_vectors: 1,
                max_bytes: peak + 1,
                ..DenseCacheLimits::default()
            },
        );
        let dense = &paused.cache;
        let first = dense
            .get(&db, project_id, embedder.model_id())
            .expect("get");
        let generation = repo::index_generation(db.connection(), project_id).expect("generation");
        // Resume a concurrent cold lookup that missed get before the first
        // installed its worker. The pre-scan budget check now sees that worker.
        let second = dense
            .resolve(
                DenseRequest {
                    cache_root: db.cache_root(),
                    project_id,
                    generation,
                    model_id: embedder.model_id(),
                },
                DenseMetadata::capture(&db, project_id)
                    .expect("metadata")
                    .into_chunks(),
            )
            .expect("resolve");
        assert!(
            matches!((&first, &second), (DenseRows::Building(first), DenseRows::Building(second)) if Arc::ptr_eq(first, second)),
            "early fallback must reuse the same-key worker: {first:?}, {second:?}"
        );
        paused.finish();
        assert!(dense.ann_snapshot(project_id).is_some());
    }

    #[test]
    fn build_lifetime_failure_releases_reservation() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 1);
        let dense = &paused.cache;
        let mut broken = small_input();
        broken.rows[1].1 = Arc::from(vec![0.5; 127]);
        assert!(matches!(
            dense.resolve_rows(1, 1, "m@d128", broken),
            DenseRows::Building(_)
        ));
        paused.finish();
        assert!(matches!(
            dense.lock_entries()[&1].state,
            DenseRows::Failed(_)
        ));
        assert!(matches!(
            dense.resolve_rows(2, 1, "m@d128", small_input()),
            DenseRows::Building(_)
        ));
    }

    #[test]
    fn build_lifetime_success_transfers_reservation_to_resident_index() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 1);
        let dense = &paused.cache;
        dense.resolve_rows(1, 1, "m@d128", small_input());
        paused.finish();
        assert!(
            dense.ann_snapshot(1).is_some(),
            "published index fits the cap"
        );
        dense.invalidate(1).expect("drop resident ANN");
        assert!(matches!(
            dense.resolve_rows(2, 1, "m@d128", small_input()),
            DenseRows::Building(_)
        ));
    }

    #[test]
    fn build_lifetime_delayed_pressure_fallback_stays_uncached() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 1);
        paused.cache.resolve_rows(1, 1, "m@d128", small_input());
        // A lookup already decided to stream under pressure, but the worker
        // finishes before that lookup reaches the fallback retention step.
        paused.finish();
        let hashes = Arc::new(vec![(1, "hash".to_owned())]);
        assert!(matches!(
            paused.cache.stream_transient(2, 1, "m@d128", hashes),
            DenseRows::TooLarge(_)
        ));
        assert!(!paused.cache.lock_entries().contains_key(&2));
    }

    #[test]
    fn build_lifetime_delayed_admission_reuses_published_ann() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 3);
        let dense = &paused.cache;
        dense.resolve_rows(1, 1, "m@d128", small_input());
        paused.finish();
        let DenseRows::Ann(published) = dense.lock_entries()[&1].state.clone() else {
            panic!("first build must publish");
        };
        // A cold lookup made its seek_ann decision before publication and
        // resumes at start_build after the first worker has published.
        let DenseInput {
            chunks,
            digest,
            rows,
            dim,
            ..
        } = small_input();
        let job = BuildJob {
            project_id: 1,
            generation: 1,
            model_id: "m@d128".to_owned(),
            key: ann_key(1, 1, "m@d128", dim, chunks.len(), digest),
            building_bytes: u64::try_from(
                ann::ann_estimated_bytes(rows.len(), dim) + rows.len() * dim * 4,
            )
            .expect("bytes"),
            rows,
            hashes: Arc::new(chunks),
            dim,
            store_root: Some(root.path().to_path_buf()),
        };
        let state = dense.start_build(job);
        assert!(matches!(state, DenseRows::Ann(current) if Arc::ptr_eq(&current, &published)));
        assert_eq!(dense.active_build_bytes.load(AtomicOrdering::Acquire), 0);
    }

    #[test]
    fn build_lifetime_stale_worker_cannot_replace_same_key_entry() {
        let root = tempfile::TempDir::new().expect("cache");
        let paused = PausedBuilds::new(root.path(), 2, 3);
        let dense = &paused.cache;
        dense.resolve_rows(1, 1, "m@d128", small_input());
        let replacement = Arc::new(BuildingAnn {
            hashes: Arc::new(Vec::new()),
            bytes: 0,
        });
        // Simulate an ABA replacement after invalidation/re-admission. The
        // key and state kind match, but this is not the original worker.
        dense.lock_entries().get_mut(&1).expect("entry").state =
            DenseRows::Building(Arc::clone(&replacement));
        paused.finish();
        let entries = dense.lock_entries();
        assert!(
            matches!(&entries[&1].state, DenseRows::Building(current) if Arc::ptr_eq(current, &replacement)),
            "completion must match the specific Building entry, not only its key"
        );
    }

    #[test]
    fn second_build_is_not_admitted_while_the_first_occupies_the_cap() {
        let cache = tempfile::TempDir::new().expect("cache");
        let dim = 64;
        let count = 64;
        let per_build =
            u64::try_from(ann::ann_estimated_bytes(count, dim) + count * dim * 4).expect("bytes");
        let limits = DenseCacheLimits {
            min_vectors: 1,
            // Room for exactly one build reservation, none for two.
            max_bytes: per_build + 1024,
            ..DenseCacheLimits::default()
        };
        let paused = PausedBuilds::from_limits(cache.path(), limits);
        let dense = &paused.cache;

        let first = dense.resolve_rows(1, 1, "m@d64", dense_input(wide_rows(count, dim), dim));
        assert!(matches!(first, DenseRows::Building(_)));

        let second = dense.resolve_rows(2, 1, "m@d64", dense_input(wide_rows(count, dim), dim));
        assert!(
            matches!(&second, DenseRows::TooLarge(hashes) if hashes.len() == count),
            "exact rows do not fit next to the live build, so stream, got {second:?}"
        );
    }

    #[test]
    fn concurrent_lookups_admit_only_one_build() {
        use std::sync::Barrier;

        let cache = tempfile::TempDir::new().expect("cache");
        let dim = 64;
        let count = 64;
        let per_build =
            u64::try_from(ann::ann_estimated_bytes(count, dim) + count * dim * 4).expect("bytes");
        let limits = DenseCacheLimits {
            min_vectors: 1,
            max_bytes: per_build + 1024,
            ..DenseCacheLimits::default()
        };
        let paused = PausedBuilds::from_limits(cache.path(), limits);
        let dense = Arc::new(paused.cache.snapshot());
        let barrier = Arc::new(Barrier::new(2));
        let spawn = |cache: DenseCache, project: i64, barrier: Arc<Barrier>| {
            std::thread::spawn(move || {
                barrier.wait();
                cache.resolve_rows(project, 1, "m@d64", dense_input(wide_rows(count, dim), dim))
            })
        };
        let first = spawn(dense.snapshot(), 1, Arc::clone(&barrier));
        let second = spawn(dense.snapshot(), 2, barrier);
        let states = [first.join().expect("join"), second.join().expect("join")];
        let builds = states
            .iter()
            .filter(|state| matches!(state, DenseRows::Building(_)))
            .count();
        assert_eq!(
            builds, 1,
            "concurrent lookups must admit exactly one build under the cap, got {states:?}"
        );
    }

    #[test]
    fn default_cap_admits_the_documented_target_corpus() {
        let count = 75_000;
        let dim = 2048;
        let limits = DenseCacheLimits::default();
        assert!(
            ann_build_admitted(count, dim, limits.max_bytes),
            "the default resident cap must admit a 75k x 2048 build (Delta 0.3.1)"
        );
        let steady = u64::try_from(ann::ann_estimated_bytes(count, dim)).expect("estimate");
        assert!(
            steady <= 1024 * 1024 * 1024,
            "steady ANN must stay within the 1 GiB model, got {steady}"
        );
    }

    #[test]
    fn build_is_skipped_when_exact_rows_and_graph_exceed_the_cap() {
        let cache = tempfile::TempDir::new().expect("cache");
        let dim = 2048;
        let count = 100;
        let estimated = u64::try_from(ann::ann_estimated_bytes(count, dim)).expect("estimate");
        let exact = u64::try_from(count * dim * 4).expect("bytes");
        let limits = DenseCacheLimits {
            min_vectors: 1,
            // The graph estimate alone fits; graph plus retained rows does not.
            max_bytes: estimated + exact / 2,
            ..DenseCacheLimits::default()
        };
        let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
        let state =
            dense.resolve_rows(1, 1, "model@d2048", dense_input(wide_rows(count, dim), dim));
        assert!(
            matches!(state, DenseRows::Ready(_)),
            "a build that would exceed the resident cap must stay exact, got {state:?}"
        );
    }

    #[test]
    fn building_entry_accounts_exact_rows_it_keeps_alive() {
        let cache = tempfile::TempDir::new().expect("cache");
        let dim = 64;
        let count = 64;
        let estimated = u64::try_from(ann::ann_estimated_bytes(count, dim)).expect("estimate");
        let exact = u64::try_from(count * dim * 4).expect("bytes");
        let limits = DenseCacheLimits {
            min_vectors: 1,
            max_bytes: 4 * 1024 * 1024,
            ..DenseCacheLimits::default()
        };
        let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
        let state = dense.resolve_rows(1, 1, "model@d64", dense_input(wide_rows(count, dim), dim));
        match state {
            DenseRows::Building(building) => {
                assert_eq!(
                    building.bytes,
                    estimated + exact,
                    "Building must account the rows it keeps alive"
                );
            }
            other => panic!("expected a background build, got {other:?}"),
        }
    }

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn orders_by_cosine_score() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![1.0, 0.0]),
            (11, vec![0.0, 1.0]),
            (12, vec![0.6, 0.8]),
        ];
        let hits = top_k(&query, &rows, 10).expect("scan");
        let ids: Vec<u64> = hits.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![10, 12, 11]);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 0.6);
        approx(hits[2].1, 0.0);
    }

    #[test]
    fn cosine_is_scale_invariant() {
        let query = [2.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![5.0, 0.0]), (11, vec![0.1, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits[0].0, 10);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 1.0);
    }

    #[test]
    fn arrives_at_unit_vectors_as_plain_dot() {
        let query = [0.6, 0.8];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![0.8, 0.6])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        approx(hits[0].1, 0.96);
    }

    #[test]
    fn equal_scores_break_ties_by_chunk_id() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(12, vec![2.0, 0.0]), (10, vec![1.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(
            hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![10, 12]
        );
    }

    #[test]
    fn zero_query_scores_zero_for_every_candidate() {
        let query = [0.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(12, vec![1.0, 0.0]), (10, vec![1.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(
            hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![10, 12]
        );
        assert!(hits.iter().all(|(_, score)| *score == 0.0));
    }

    #[test]
    fn zero_candidate_scores_zero() {
        let query = [1.0, 1.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![0.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        approx(hits[0].1, 0.0);
    }

    #[test]
    fn non_finite_values_score_zero() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![f32::NAN, 0.0]),
            (11, vec![f32::INFINITY, 0.0]),
            (12, vec![1.0, 0.0]),
        ];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits[0].0, 12);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 0.0);
        approx(hits[2].1, 0.0);
    }

    #[test]
    fn fetch_caps_the_result() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![1.0, 0.0]),
            (11, vec![0.9, 0.1]),
            (12, vec![0.8, 0.2]),
        ];
        let hits = top_k(&query, &rows, 2).expect("scan");
        assert_eq!(
            hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!(top_k(&query, &rows, 0).expect("scan").is_empty());
        assert_eq!(top_k(&query, &rows, 99).expect("scan").len(), 3);
    }

    #[test]
    fn empty_inputs_are_empty() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = Vec::new();
        assert!(top_k(&query, &rows, 10).expect("scan").is_empty());
    }

    #[test]
    fn empty_query_is_an_internal_error() {
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0])];
        let err = top_k(&[], &rows, 10).expect_err("empty query");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn dimension_mismatch_is_an_internal_error() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0, 0.0])];
        let err = top_k(&query, &rows, 10).expect_err("dim mismatch");
        assert!(err.to_string().contains("dim"), "{err}");
    }

    fn iter_rows(rows: &[(u64, Vec<f32>)]) -> impl Iterator<Item = Result<(u64, Vec<f32>)>> + '_ {
        rows.iter().map(|(id, vector)| Ok((*id, vector.clone())))
    }

    #[test]
    fn iter_matches_the_batch_scan_on_the_same_rows() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![1.0, 0.0]),
            (11, vec![0.0, 1.0]),
            (12, vec![0.6, 0.8]),
            (13, vec![2.0, 0.0]),
        ];
        let batch = top_k(&query, &rows, 3).expect("batch");
        let streamed = top_k_iter(&query, iter_rows(&rows), 3).expect("stream");
        assert_eq!(batch, streamed);
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn iter_keeps_only_the_best_fetch_candidates() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = (0..1000)
            .map(|id| (id, vec![1.0 - (id as f32) / 2000.0, 0.0]))
            .collect();
        let streamed = top_k_iter(&query, iter_rows(&rows), 3).expect("stream");
        assert_eq!(
            streamed.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn iter_propagates_row_errors() {
        let query = [1.0, 0.0];
        let rows: Vec<Result<(u64, Vec<f32>)>> =
            vec![Ok((10, vec![1.0, 0.0])), Err(Error::internal("boom"))];
        let err = top_k_iter(&query, rows.into_iter(), 10).expect_err("row error");
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn iter_dim_mismatch_is_an_internal_error() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0, 0.0])];
        let err = top_k_iter(&query, iter_rows(&rows), 10).expect_err("dim mismatch");
        assert!(err.to_string().contains("dim"), "{err}");
    }

    #[test]
    fn iter_edge_inputs_are_empty_or_errors() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0])];
        assert!(
            top_k_iter(&query, iter_rows(&rows), 0)
                .expect("fetch 0")
                .is_empty()
        );
        assert!(
            top_k_iter(&query, iter_rows(&[]), 10)
                .expect("no rows")
                .is_empty()
        );
        let err = top_k_iter(&[], iter_rows(&rows), 10).expect_err("empty query");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    fn indexed_project() -> (tempfile::TempDir, Db, i64) {
        use std::fs;

        use crate::config::Config;
        use crate::daemon::{registry, tools::run_project_index};

        let cache = tempfile::TempDir::new().expect("cache");
        let root = tempfile::TempDir::new().expect("root");
        for (name, body) in [
            ("a.md", "# A\n\nwidgets and sprockets.\n"),
            ("b.md", "# B\n\ncogs and gears.\n"),
            ("c.md", "# C\n\nmore widgets here.\n"),
        ] {
            fs::write(root.path().join(name), body).expect("write doc");
        }
        let mut db = Db::open(cache.path()).expect("db");
        let canonical = root.path().canonicalize().expect("canonical");
        let project = registry::ensure_project(&mut db, &canonical).expect("project");
        run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
        (cache, db, project.id)
    }

    struct FakeEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl crate::vector_cache::Embedder for FakeEmbedder {
        fn model_id(&self) -> &'static str {
            "fake/model@d3@l2"
        }

        #[allow(clippy::cast_precision_loss)]
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|text| vec![text.len() as f32, 0.5, 0.25])
                .collect())
        }
    }

    fn ready(rows: DenseRows) -> Arc<DenseIndex> {
        match rows {
            DenseRows::Ready(index) => index,
            DenseRows::TooLarge(_) => panic!("fixture project must fit the dense cache"),
            DenseRows::Ann(_) | DenseRows::Building(_) | DenseRows::Failed(_) => {
                panic!("fixture cache has ANN disabled")
            }
        }
    }

    #[test]
    fn chunk_hashes_matches_chunk_texts_in_id_order() {
        let (_cache, db, project_id) = indexed_project();
        let texts = repo::chunk_texts(db.connection(), project_id).expect("texts");
        let hashes = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
        let expected: Vec<String> = texts
            .iter()
            .map(|text| crate::vector_cache::chunk_hash(text))
            .collect();
        let actual: Vec<String> = hashes.iter().map(|(_, hash)| hash.clone()).collect();
        assert_eq!(actual, expected);
        assert!(
            hashes.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "chunk ids are strictly increasing"
        );
    }

    #[test]
    fn dense_cache_serves_rows_and_rebuilds_on_new_generation() {
        let (_cache, mut db, project_id) = indexed_project();
        let embedder = FakeEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let stored = crate::hybrid::jobs::embed_project_chunks_with(&db, &embedder, project_id)
            .expect("embed");
        assert!(stored > 0, "the backfill stores vectors");

        let dense = DenseCache::new();
        let first = ready(
            dense
                .get(&db, project_id, embedder.model_id())
                .expect("get"),
        );
        let hashes = repo::chunk_hashes(db.connection(), project_id).expect("hashes");
        assert_eq!(first.rows().len(), hashes.len(), "every chunk has a vector");
        assert!(
            first.rows().windows(2).all(|pair| pair[0].0 < pair[1].0),
            "deterministic id order"
        );

        let again = ready(
            dense
                .get(&db, project_id, embedder.model_id())
                .expect("get"),
        );
        assert!(Arc::ptr_eq(&first, &again), "cache hit without a rebuild");

        {
            let tx = db.begin_write().expect("tx");
            repo::bump_index_generation(&tx, project_id).expect("bump");
            tx.commit().expect("commit");
        }
        let rebuilt = ready(
            dense
                .get(&db, project_id, embedder.model_id())
                .expect("get"),
        );
        assert!(
            !Arc::ptr_eq(&first, &rebuilt),
            "a new generation invalidates"
        );
        assert_eq!(rebuilt.generation(), first.generation() + 1);
        assert_eq!(rebuilt.rows().len(), first.rows().len());
    }

    #[test]
    fn dense_cache_rebuilds_when_the_model_changes() {
        let (_cache, db, project_id) = indexed_project();
        let embedder = FakeEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        crate::hybrid::jobs::embed_project_chunks_with(&db, &embedder, project_id).expect("embed");
        let dense = DenseCache::new();
        let first = ready(
            dense
                .get(&db, project_id, embedder.model_id())
                .expect("get"),
        );
        assert!(!first.rows().is_empty());

        let other = ready(
            dense
                .get(&db, project_id, "fake/model@d99@l2")
                .expect("get"),
        );
        assert!(!Arc::ptr_eq(&first, &other), "a model change invalidates");
        assert_eq!(other.generation(), first.generation());
        assert_eq!(other.model_id(), "fake/model@d99@l2");
        assert!(other.rows().is_empty(), "no vectors under the other key");

        let again = ready(
            dense
                .get(&db, project_id, embedder.model_id())
                .expect("get"),
        );
        assert_eq!(again.model_id(), embedder.model_id());
        assert!(
            !again.rows().is_empty(),
            "the first model's vectors survive"
        );
    }

    #[test]
    fn dense_cache_refuses_projects_above_the_cap() {
        let (_cache, db, project_id) = indexed_project();
        let dense = DenseCache::with_cap(1);
        let out = dense.get(&db, project_id, "fake/model@d3@l2").expect("get");
        assert!(matches!(out, DenseRows::TooLarge(_)));
    }

    #[test]
    fn digest_is_cached_per_project_generation() {
        let (cache, db, project_id) = indexed_project();
        let embedder = FakeEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        crate::hybrid::jobs::embed_project_chunks_with(&db, &embedder, project_id).expect("embed");

        let dense = DenseCache::with_policy_limits(
            AnnPolicy::Disabled,
            cache.path(),
            DenseCacheLimits::default(),
        );
        dense
            .get(&db, project_id, embedder.model_id())
            .expect("get");
        {
            let digests = dense.digests.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(digests.len(), 1, "first lookup computes the digest");
        }
        dense
            .get(&db, project_id, embedder.model_id())
            .expect("get hit");
        {
            let digests = dense.digests.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(digests.len(), 1, "a hit must not recompute the digest");
        }
    }

    #[test]
    fn digest_history_is_bounded_across_generation_changes() {
        let dense = DenseCache::new();
        for generation in 1..=64 {
            let chunks = vec![(1, format!("hash-{generation}"))];
            assert_eq!(
                dense.digest_for(1, generation, &chunks),
                crate::hybrid::ann_store::chunk_set_digest(&chunks)
            );
            let digests = dense.digests.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(digests.len(), 1, "old generations must be discarded");
            assert!(digests.contains_key(&(1, generation)));
        }
        // Work that started before a newer generation must still get its own
        // correct digest without repopulating the cache's retired history.
        let old = vec![(1, "retired-hash".to_owned())];
        assert_eq!(
            dense.digest_for(1, 1, &old),
            crate::hybrid::ann_store::chunk_set_digest(&old)
        );
        let digests = dense.digests.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(digests.len(), 1);
        assert!(digests.contains_key(&(1, 64)));
    }

    #[test]
    fn digest_cache_bounds_projects_even_without_resident_entries() {
        let dense = DenseCache::with_policy_and_limits(
            AnnPolicy::Disabled,
            None,
            DenseCacheLimits {
                max_projects: 2,
                ..DenseCacheLimits::default()
            },
            None,
        );
        let chunks = vec![(1, "hash".to_owned())];
        dense.digest_for(1, 1, &chunks);
        dense.digest_for(2, 1, &chunks);
        dense.digest_for(1, 1, &chunks); // Refresh project 1's recency.
        dense.digest_for(3, 1, &chunks);
        {
            let digests = dense.digests.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(digests.len(), 2);
            assert!(digests.contains_key(&(1, 1)));
            assert!(digests.contains_key(&(3, 1)));
        }
        for project in 4..=32 {
            dense.digest_for(project, 1, &chunks);
            assert!(
                dense
                    .digests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len()
                    <= 2
            );
        }
        dense.invalidate(32).expect("invalidate");
        assert!(
            !dense
                .digests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .any(|(project, _)| *project == 32)
        );
    }

    #[test]
    fn digest_cache_can_be_disabled_with_zero_project_capacity() {
        let dense = DenseCache::with_policy_and_limits(
            AnnPolicy::Disabled,
            None,
            DenseCacheLimits {
                max_projects: 0,
                ..DenseCacheLimits::default()
            },
            None,
        );
        let chunks = vec![(1, "hash".to_owned())];
        assert_eq!(
            dense.digest_for(1, 1, &chunks),
            crate::hybrid::ann_store::chunk_set_digest(&chunks)
        );
        assert!(
            dense
                .digests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
    }
}
