//! Dense candidate generation: cosine top-`fetch` over cached vectors (FR-15).
//!
//! The scan is intentionally brute-force: iteration 2 selects the best
//! `fetch` chunks by cosine similarity against the query embedding; an ANN
//! index is a later iteration (DELTA-7 non-goals). Every stored vector is
//! scored with a true cosine (division by both norms), which degenerates to
//! the dot product for the L2-normalized vectors produced by the default
//! embedder configuration, so the same code serves `normalize = false`.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{Error, Result};
use crate::store::Db;
use crate::store::repo;
use crate::vector_cache::VectorCache;

/// Hard per-project cap on cached vectors (DELTA-7): above it the cache
/// refuses to build and callers stream the scan instead.
pub const DENSE_CACHE_MAX_VECTORS: usize = 1_000_000;

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

/// Per-project dense rows for a fixed index generation.
#[derive(Debug)]
pub struct DenseIndex {
    generation: i64,
    rows: Vec<(u64, Arc<[f32]>)>,
}

impl DenseIndex {
    /// Index generation the rows were built for (DELTA-7).
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// `(chunk_id, vector)` rows with vectors only, deterministic order.
    #[must_use]
    pub fn rows(&self) -> &[(u64, Arc<[f32]>)] {
        &self.rows
    }
}

/// Outcome of a dense-cache lookup.
#[derive(Debug)]
pub enum DenseRows {
    /// Rows for the current index generation (fresh or cached).
    Ready(Arc<DenseIndex>),
    /// The project exceeds [`DENSE_CACHE_MAX_VECTORS`]; stream [`top_k_iter`].
    TooLarge,
}

/// Per-project dense cache keyed by the index generation (DELTA-7): a new
/// generation replaces the entry, a no-op sync keeps it.
#[derive(Debug)]
pub struct DenseCache {
    cap: usize,
    entries: Mutex<HashMap<i64, Arc<DenseIndex>>>,
}

impl Default for DenseCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DenseCache {
    /// Cache with the [`DENSE_CACHE_MAX_VECTORS`] cap.
    #[must_use]
    pub fn new() -> Self {
        Self::with_cap(DENSE_CACHE_MAX_VECTORS)
    }

    /// Cache with an explicit cap (tests and future tuning).
    #[must_use]
    pub fn with_cap(cap: usize) -> Self {
        Self {
            cap,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the project's rows, rebuilding when the index generation moved.
    ///
    /// # Errors
    /// Registry failures (generation, chunk rows) and vector-cache failures.
    pub fn get(&self, db: &Db, project_id: i64, model_id: &str) -> Result<DenseRows> {
        let generation = repo::index_generation(db.connection(), project_id)?;
        {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = entries.get(&project_id)
                && entry.generation == generation
            {
                return Ok(DenseRows::Ready(Arc::clone(entry)));
            }
        }
        let chunks = repo::chunk_hashes(db.connection(), project_id)?;
        if chunks.len() > self.cap {
            return Ok(DenseRows::TooLarge);
        }
        let mut cache = VectorCache::open(db.cache_root())?;
        let mut rows = Vec::with_capacity(chunks.len());
        for (chunk_id, hash) in chunks {
            if let Some(vector) = cache.get(model_id, &hash)? {
                rows.push((chunk_id, Arc::from(vector)));
            }
        }
        let entry = Arc::new(DenseIndex { generation, rows });
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(project_id, Arc::clone(&entry));
        Ok(DenseRows::Ready(entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_cache::Embedder;

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
            DenseRows::TooLarge => panic!("fixture project must fit the dense cache"),
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
    fn dense_cache_refuses_projects_above_the_cap() {
        let (_cache, db, project_id) = indexed_project();
        let dense = DenseCache::with_cap(1);
        let out = dense.get(&db, project_id, "fake/model@d3@l2").expect("get");
        assert!(matches!(out, DenseRows::TooLarge));
    }
}
