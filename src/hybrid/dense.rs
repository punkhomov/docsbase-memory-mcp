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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::hybrid::ann::{self, ANN_MIN_VECTORS, AnnIndex};
use crate::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};
use crate::store::Db;
use crate::store::repo;
use crate::vector_cache::{CacheLimits, VectorCache};

/// Hard per-project cap on cached vectors (DELTA-7): above it the cache
/// refuses to build and callers stream the scan instead.
pub const DENSE_CACHE_MAX_VECTORS: usize = 1_000_000;

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
    /// The project exceeds the vector or byte cap; stream [`top_k_iter`]
    /// over the cached hashes.
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
    /// Maximum resident bytes across entries, including exact rows and the
    /// transient graph+rows peak of an in-flight build. The default admits the
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

/// Per-project dense cache keyed by `(index generation, model_id)` with an
/// optional ANN tier (spec ann-index): a new generation or embedding model
/// replaces the entry, a no-op sync keeps it.
pub struct DenseCache {
    limits: DenseCacheLimits,
    policy: AnnPolicy,
    store: Option<AnnStore>,
    entries: Arc<Mutex<HashMap<i64, Entry>>>,
    digests: Arc<Mutex<HashMap<(i64, i64), String>>>,
    vector_cache_limits: Arc<Mutex<HashMap<i64, u64>>>,
    clock: Arc<AtomicU64>,
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
        Self::with_policy_and_limits(AnnPolicy::Disabled, None, DenseCacheLimits::default())
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
        )
    }

    /// Cache with an ANN policy and store root (daemon/CLI; design §10).
    #[must_use]
    pub fn with_policy(policy: AnnPolicy, cache_root: &std::path::Path) -> Self {
        let store =
            (policy != AnnPolicy::Disabled).then(|| AnnStore::new(cache_root.to_path_buf()));
        Self::with_policy_and_limits(policy, store, DenseCacheLimits::default())
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
        Self::with_policy_and_limits(policy, store, limits)
    }

    fn with_policy_and_limits(
        policy: AnnPolicy,
        store: Option<AnnStore>,
        limits: DenseCacheLimits,
    ) -> Self {
        Self {
            limits,
            policy,
            store,
            entries: Arc::new(Mutex::new(HashMap::new())),
            digests: Arc::new(Mutex::new(HashMap::new())),
            vector_cache_limits: Arc::new(Mutex::new(HashMap::new())),
            clock: Arc::new(AtomicU64::new(0)),
        }
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, AtomicOrdering::Relaxed)
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Digest of the current chunk set for `(project, generation)`, computed
    /// once and reused until the generation changes (design §8).
    fn digest_for(&self, project_id: i64, generation: i64, chunks: &[(u64, String)]) -> String {
        let key = (project_id, generation);
        {
            let digests = self.digests.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(digest) = digests.get(&key) {
                return digest.clone();
            }
        }
        let digest = crate::hybrid::ann_store::chunk_set_digest(chunks);
        let mut digests = self.digests.lock().unwrap_or_else(PoisonError::into_inner);
        digests.insert(key, digest.clone());
        digest
    }

    /// Returns the project's rows (or ANN state), rebuilding when the index
    /// generation moved.
    ///
    /// # Errors
    /// Registry failures (generation, chunk rows) and vector-cache failures.
    pub fn get(&self, db: &Db, project_id: i64, model_id: &str) -> Result<DenseRows> {
        let generation = repo::index_generation(db.connection(), project_id)?;
        {
            let mut entries = self.lock_entries();
            if let Some(entry) = entries.get_mut(&project_id)
                && entry.generation == generation
                && entry.model_id == model_id
            {
                entry.last_used = self.tick();
                let retry_due = matches!(
                    entry.state,
                    DenseRows::Failed(ref failed) if failed.retry_at <= Instant::now()
                );
                if !retry_due {
                    return Ok(entry.state.clone());
                }
            }
        }
        self.resolve(db, project_id, generation, model_id)
    }

    /// Full lookup path: exact rows, ANN load or background build.
    fn resolve(
        &self,
        db: &Db,
        project_id: i64,
        generation: i64,
        model_id: &str,
    ) -> Result<DenseRows> {
        let chunks = repo::chunk_hashes(db.connection(), project_id)?;
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
            && u64::try_from(exact_bytes).unwrap_or(u64::MAX) > self.limits.max_bytes
        {
            return Ok(self.stash_too_large(project_id, generation, model_id, chunks));
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
                        self.stash(project_id, generation, model_id, state.clone());
                        return Ok(state);
                    }
                    Err(err) => {
                        eprintln!("warning: ann load failed: {err}; falling back to exact");
                    }
                }
            }
        }
        let vector_cache_max_bytes = self.vector_cache_max_bytes(project_id);
        let mut cache = VectorCache::open_with_limits(
            db.cache_root(),
            CacheLimits {
                max_bytes: vector_cache_max_bytes,
                ..CacheLimits::default()
            },
        )?;
        let mut rows = Vec::with_capacity(chunks.len());
        let mut full_coverage = true;
        for (chunk_id, hash) in &chunks {
            match cache.get(model_id, hash)? {
                Some(vector) => rows.push((*chunk_id, Arc::from(vector))),
                None => full_coverage = false,
            }
        }
        let dim = rows
            .first()
            .map_or(0, |(_, vector): &(u64, Arc<[f32]>)| vector.len());
        Ok(self.resolve_rows(
            project_id,
            generation,
            model_id,
            DenseInput {
                chunks,
                digest,
                rows,
                dim,
                full_coverage,
            },
        ))
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
            let other = entries
                .iter()
                .filter(|(id, _)| **id != project_id)
                .map(|(_, entry)| entry.bytes)
                .sum::<u64>();
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
            return self.stash_ready(project_id, generation, model_id, rows);
        }
        let key = ann_key(project_id, generation, model_id, dim, chunks.len(), digest);
        if let Some(store) = &self.store
            && let Ok(Some(slot)) = store.load(&key)
        {
            match AnnIndex::load(&slot) {
                Ok(ann) => {
                    let state = DenseRows::Ann(Arc::new(ann));
                    self.stash(project_id, generation, model_id, state.clone());
                    return state;
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
            self.stash_ready(project_id, generation, model_id, rows)
        }
    }

    fn stash_ready(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        rows: Vec<(u64, Arc<[f32]>)>,
    ) -> DenseRows {
        let state = DenseRows::Ready(Arc::new(DenseIndex {
            generation,
            model_id: model_id.to_owned(),
            rows,
        }));
        self.stash(project_id, generation, model_id, state.clone());
        state
    }

    fn stash_too_large(
        &self,
        project_id: i64,
        generation: i64,
        model_id: &str,
        chunks: Vec<(u64, String)>,
    ) -> DenseRows {
        let state = DenseRows::TooLarge(Arc::new(chunks));
        self.stash(project_id, generation, model_id, state.clone());
        state
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
            if let Some(entry) = entries.get(&job.project_id)
                && entry.generation == job.generation
                && entry.model_id == job.model_id
                && matches!(entry.state, DenseRows::Building(_))
            {
                return entry.state.clone();
            }
            // Re-check aggregate admission while holding the entries lock:
            // two concurrent lookups must not both insert a reservation from a
            // stale snapshot (Delta 0.3.2; the resolve_rows check is a fast
            // path only). Falling back to exact keeps the cap honest.
            let others = entries
                .iter()
                .filter(|(id, _)| **id != job.project_id)
                .map(|(_, entry)| entry.bytes)
                .sum::<u64>();
            if job.building_bytes.saturating_add(others) > self.limits.max_bytes {
                drop(entries);
                return self.stash_ready(job.project_id, job.generation, &job.model_id, job.rows);
            }
            let rows = Arc::from(job.rows);
            let hashes = Arc::clone(&job.hashes);
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
            let spawn = {
                let cache = Arc::clone(&cache);
                let key = job.key.clone();
                let project_id = job.project_id;
                let dim = job.dim;
                let store_root = job.store_root;
                move || {
                    if let Some(store_root) = store_root {
                        build_in_background(cache, project_id, key, rows, hashes, dim, store_root);
                    }
                }
            };
            std::thread::spawn(spawn);
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
        {
            let entries = self.lock_entries();
            if let Some(entry) = entries.get(&project_id)
                && entry.generation == generation
                && entry.model_id == model_id
            {
                let waiting = match &entry.state {
                    DenseRows::Ann(_) | DenseRows::Building(_) => true,
                    DenseRows::Failed(failed) => failed.retry_at > Instant::now(),
                    DenseRows::Ready(_) | DenseRows::TooLarge(_) => false,
                };
                if waiting {
                    return Ok(());
                }
            }
        }
        self.resolve(db, project_id, generation, model_id)
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

    fn stash(&self, project_id: i64, generation: i64, model_id: &str, state: DenseRows) {
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
        let entry = Entry {
            state,
            generation,
            model_id: model_id.to_owned(),
            last_used: self.tick(),
            bytes,
        };
        let mut entries = self.lock_entries();
        if entries.len() >= self.limits.max_projects && !entries.contains_key(&project_id) {
            self.evict_oldest(&mut entries, Some(project_id));
        }
        entries.insert(project_id, entry);
        self.enforce_byte_cap(&mut entries, project_id);
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
        if let Some(entry) = entries.get(&oldest) {
            self.digests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&(oldest, entry.generation));
        }
        entries.remove(&oldest);
        Some(oldest)
    }

    /// Enforces the global byte cap, keeping at least the just-inserted
    /// entry (single entries over the cap are handled by `resolve`).
    fn enforce_byte_cap(&self, entries: &mut HashMap<i64, Entry>, keep: i64) {
        loop {
            let total = entries.values().map(|entry| entry.bytes).sum::<u64>();
            if total <= self.limits.max_bytes || entries.len() <= 1 {
                return;
            }
            if self.evict_oldest(entries, Some(keep)).is_none() {
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
            entries: Arc::clone(&self.entries),
            digests: Arc::clone(&self.digests),
            vector_cache_limits: Arc::clone(&self.vector_cache_limits),
            clock: Arc::clone(&self.clock),
        }
    }
}

/// Test-only build delay in milliseconds (SC-11 coverage).
#[cfg(debug_assertions)]
static TEST_BUILD_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test-only hook: slows background builds so a test can prove that a search
/// ran while the build was still in flight (SC-11). Absent in release builds.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_test_build_delay(ms: u64) {
    TEST_BUILD_DELAY_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(debug_assertions)]
fn test_build_delay() {
    let ms = TEST_BUILD_DELAY_MS.load(std::sync::atomic::Ordering::Relaxed);
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

#[cfg(not(debug_assertions))]
fn test_build_delay() {}

/// Background build: builds, dumps, loads and publishes the `Ann` entry
/// unless the project moved on meanwhile (design §8). Any panic from the
/// underlying index is caught and treated like a build failure (FR-5).
#[allow(clippy::needless_pass_by_value)]
fn build_in_background(
    cache: Arc<DenseCache>,
    project_id: i64,
    key: AnnKey,
    rows: Arc<[(u64, Arc<[f32]>)]>,
    hashes: Arc<Vec<(u64, String)>>,
    dim: usize,
    store_root: PathBuf,
) {
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
    if let Ok((loaded, published_bytes)) = &result {
        // Publish last: once `Ann` is visible the build thread holds no
        // `vectors.db` handle, so a purge can delete it immediately.
        let mut entries = cache.lock_entries();
        if let Some(entry) = entries.get_mut(&project_id)
            && entry.generation == key.generation
            && entry.model_id == key.model_id
        {
            entry.state = DenseRows::Ann(Arc::clone(loaded));
            entry.bytes = *published_bytes;
            entry.last_used = cache.tick();
        }
        cache.enforce_byte_cap(&mut entries, project_id);
    }
    if let Err(err) = result {
        eprintln!("warning: ann build failed for project {project_id}: {err}; keeping exact");
        let mut entries = cache.lock_entries();
        if let Some(entry) = entries.get_mut(&project_id)
            && entry.generation == key.generation
            && entry.model_id == key.model_id
            && matches!(entry.state, DenseRows::Building(_))
        {
            entry.bytes = hashes.len() as u64 * 64;
            entry.state = DenseRows::Failed(Arc::new(FailedAnn {
                hashes,
                retry_at: Instant::now() + cache.limits.build_backoff,
            }));
        }
        cache.enforce_byte_cap(&mut entries, project_id);
    }
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
        let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);

        let first = dense.resolve_rows(1, 1, "m@d64", dense_input(wide_rows(count, dim), dim));
        assert!(matches!(first, DenseRows::Building(_)));

        let second = dense.resolve_rows(2, 1, "m@d64", dense_input(wide_rows(count, dim), dim));
        assert!(
            matches!(second, DenseRows::Ready(_)),
            "a second build must stay exact while the first occupies the cap, got {second:?}"
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
        let dense = Arc::new(DenseCache::with_policy_limits(
            AnnPolicy::BuildOrLoad,
            cache.path(),
            limits,
        ));
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
}
