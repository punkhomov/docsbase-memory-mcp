//! HNSW wrapper for the dense channel (spec ann-index T3): build, search and
//! safe load of a persisted index (`hnsw_rs 0.3.5` + `ouroboros 0.18.5`).
//!
//! The wrapper is the seam that keeps ANN panic/error handling inside this
//! module: the underlying crate panics on malformed dumps, and every entry
//! point converts those into [`Error::Internal`] so callers fall back to
//! exact search (FR-5).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use hnsw_rs::prelude::*;

use crate::error::{Error, Result};
use crate::hybrid::ann_store::{ANN_DEFAULT_EF_CONSTRUCTION, AnnKey, AnnSlot, AnnStore};

// FR-5/SC-4 convert `hnsw_rs` panics into `Error::Internal` through
// `catch_unwind`, which only works while unwinding is enabled. With
// `panic = "abort"` every such panic would abort the process instead of
// falling back to exact search, and the regression would be invisible in
// dev/test builds. Reject the profile at compile time instead.
const _: () = assert!(
    cfg!(panic = "unwind"),
    "panic = \"abort\" removes the ANN-to-exact fallback (FR-5/SC-4)"
);

/// Projects below this vector count always use exact `top_k` (OQ-3,
/// provisional; calibrated in T6).
pub const ANN_MIN_VECTORS: usize = 4096;

/// Indexes at or below this size are built sequentially: the parallel insert
/// is racy on tiny graphs (two points can lose a neighbour) and provides no
/// benefit at this scale.
const SEQUENTIAL_BUILD_MAX: usize = 64;

/// Graph fan-out during construction (design §6).
const M: usize = 16;

/// Number of layers in the constructed graph (design §6).
const LAYERS: usize = 16;

/// Minimum beam width during search; the effective beam is
/// `max(this, fetch * 4)` so high `limit`/window values keep recall.
const EF_SEARCH_DEFAULT: usize = 64;

/// Upper bound on the dynamic beam to keep a pathological `fetch` bounded.
const EF_SEARCH_MAX: usize = 4096;

/// Per-point graph overhead estimate used by [`AnnIndex::estimated_bytes`]
/// (design §8). Measured from the T1 spike dump: ~780 B/vertex at 10k×64;
/// rounded up to 1024 so the cap estimate never understates resident bytes.
const GRAPH_OVERHEAD_BYTES: usize = 1024;

/// Beam width for a requested `fetch`: `max(64, fetch * 4)`, capped.
fn ef_for(fetch: usize) -> usize {
    EF_SEARCH_DEFAULT
        .max(fetch.saturating_mul(4))
        .min(EF_SEARCH_MAX)
}

/// Owning wrapper for a persisted index (spike T1): `HnswIo` owns the mmap
/// or heap storage and `Hnsw` borrows it, so `HnswIo` must outlive `Hnsw`.
#[ouroboros::self_referencing]
struct LoadedAnn {
    io: HnswIo,
    #[borrows(mut io)]
    #[not_covariant]
    index: Hnsw<'this, f32, DistCosine>,
}

enum Kind {
    /// Freshly built, not yet persisted.
    Built(Hnsw<'static, f32, DistCosine>),
    /// Loaded from a persisted slot.
    Loaded(LoadedAnn),
}

/// An ANN index over one `(chunk_id, vector)` set: either freshly built in
/// memory or loaded from a persisted slot. Both variants answer cosine
/// nearest-neighbour searches.
pub struct AnnIndex {
    kind: Kind,
    dim: usize,
    count: usize,
}

impl std::fmt::Debug for AnnIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnIndex")
            .field("dim", &self.dim)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl AnnIndex {
    /// Builds an in-memory index over `rows` (parallel insert, default).
    ///
    /// The index owns its copy of the vectors; callers may drop `rows`
    /// afterwards. The graph is built with the pinned defaults (design §6)
    /// and switched to searching mode before returning.
    ///
    /// # Errors
    /// Zero `dim` or empty `rows`, a vector whose length differs from `dim`,
    /// or a `chunk_id` that does not fit into `usize`.
    pub fn build(rows: &[(u64, Arc<[f32]>)], dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(Error::internal("ann dimension is zero"));
        }
        if rows.is_empty() {
            return Err(Error::internal("ann build requires at least one vector"));
        }
        let mut items: Vec<(&[f32], usize)> = Vec::with_capacity(rows.len());
        for (id, vector) in rows {
            if vector.len() != dim {
                return Err(Error::internal(format!(
                    "ann vector dim {} does not match index dim {dim} (chunk {id})",
                    vector.len()
                )));
            }
            let id_usize = usize::try_from(*id)
                .map_err(|_| Error::internal("ann chunk_id does not fit usize"))?;
            items.push((vector.as_ref(), id_usize));
        }
        let mut hnsw = Hnsw::new(
            M,
            rows.len(),
            LAYERS,
            ANN_DEFAULT_EF_CONSTRUCTION,
            DistCosine,
        );
        if items.len() <= SEQUENTIAL_BUILD_MAX {
            // `parallel_insert_slice` races on tiny point sets (a two-point
            // graph can lose a neighbour), and threads buy nothing here.
            for item in &items {
                hnsw.insert_slice(*item);
            }
        } else {
            hnsw.parallel_insert_slice(&items);
        }
        hnsw.set_searching_mode(true);
        Ok(Self {
            kind: Kind::Built(hnsw),
            dim,
            count: rows.len(),
        })
    }

    /// Searches the `fetch` nearest neighbours, best first.
    ///
    /// Scores are cosine similarities (`1 - distance`); ties break by
    /// ascending `chunk_id` (FR-1 contract). Panics from the underlying
    /// index surface as `Err` so callers can fall back to exact search
    /// (FR-5).
    ///
    /// # Errors
    /// Empty `query` or a dimension mismatch with the index.
    pub fn search(&self, query: &[f32], fetch: usize) -> Result<Vec<(u64, f32)>> {
        self.search_inner(query, fetch, ef_for(fetch))
    }

    /// [`Self::search`] with an explicit `ef` beam (recall-vs-ef curve).
    #[cfg(test)]
    pub(crate) fn search_with_ef(
        &self,
        query: &[f32],
        fetch: usize,
        ef: usize,
    ) -> Result<Vec<(u64, f32)>> {
        self.search_inner(query, fetch, ef)
    }

    fn search_inner(&self, query: &[f32], fetch: usize, ef: usize) -> Result<Vec<(u64, f32)>> {
        if query.is_empty() {
            return Err(Error::internal("ann query vector is empty"));
        }
        if query.len() != self.dim {
            return Err(Error::internal(format!(
                "ann query dim {} does not match index dim {}",
                query.len(),
                self.dim
            )));
        }
        if fetch == 0 {
            return Ok(Vec::new());
        }
        let neighbours = match &self.kind {
            Kind::Built(hnsw) => run_unwind(|| hnsw.search(query, fetch, ef))?,
            Kind::Loaded(loaded) => {
                loaded.with_index(|index| run_unwind(|| index.search(query, fetch, ef)))?
            }
        };
        let mut out: Vec<(u64, f32)> = Vec::with_capacity(neighbours.len());
        for neighbour in neighbours {
            let Ok(id) = u64::try_from(neighbour.d_id) else {
                continue;
            };
            let score = 1.0 - neighbour.distance;
            if score.is_finite() {
                out.push((id, score));
            }
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(fetch);
        Ok(out)
    }

    /// Persists this index under `key` via `store` (design §6).
    ///
    /// Only a built index can be dumped; the dump is published atomically by
    /// the store after the `READY` marker.
    ///
    /// # Errors
    /// The index is not built, or the dump/publish fails.
    pub fn dump(&self, store: &AnnStore, key: &AnnKey) -> Result<AnnSlot> {
        let Kind::Built(hnsw) = &self.kind else {
            return Err(Error::internal("ann dump requires a built index"));
        };
        store.persist_dump(key, |dir| {
            let basename = run_unwind(|| hnsw.file_dump(dir, "index"))?
                .map_err(|err| Error::internal(format!("ann file_dump: {err}")))?;
            Ok(basename)
        })
    }

    /// Loads a persisted slot: mmap first, heap retry (design §5.2).
    ///
    /// # Errors
    /// The dump cannot be read even after the heap retry.
    pub fn load(slot: &AnnSlot) -> Result<Self> {
        let loaded = match load_loaded(&slot.dir, true) {
            Ok(loaded) => loaded,
            Err(_) => load_loaded(&slot.dir, false)?,
        };
        Ok(Self {
            kind: Kind::Loaded(loaded),
            dim: slot.meta.dim,
            count: slot.meta.count,
        })
    }

    /// Estimated resident bytes: vectors plus graph overhead (design §8).
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        ann_estimated_bytes(self.count, self.dim)
    }
}

/// Estimated resident bytes for `count` vectors of `dim` dimensions (design
/// §8): vectors plus a per-point graph overhead.
#[must_use]
pub(crate) fn ann_estimated_bytes(count: usize, dim: usize) -> usize {
    count.saturating_mul(dim * 4 + GRAPH_OVERHEAD_BYTES)
}

/// Loads a dump with `mmap` storage; panics and load errors are unified into
/// [`Error::Internal`].
fn load_loaded(dir: &Path, mmap: bool) -> Result<LoadedAnn> {
    let mut io = HnswIo::new(dir, "index");
    io.set_options(ReloadOptions::new(mmap));
    run_unwind(|| LoadedAnn::try_new(io, |io| io.load_hnsw::<f32, DistCosine>()))?
        .map_err(|err| Error::internal(format!("ann load (mmap={mmap}): {err}")))
}

/// Runs `f`, mapping any panic to [`Error::Internal`].
fn run_unwind<T>(f: impl FnOnce() -> T) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let message = if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_owned()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            "unknown panic".to_owned()
        };
        Error::internal(format!("ann panic: {message}"))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss)]

    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::hybrid::ann_store::ANN_FORMAT_VERSION;

    const TEST_DIM: usize = 32;
    const TEST_COUNT: usize = 2000;

    fn lcg(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (((*state >> 32) as u32) as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn random_rows(count: usize, dim: usize) -> Vec<(u64, Arc<[f32]>)> {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        (0..count)
            .map(|i| {
                let mut vector: Vec<f32> = (0..dim).map(|_| lcg(&mut state)).collect();
                let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
                for x in &mut vector {
                    *x /= norm;
                }
                (i as u64, Arc::from(vector))
            })
            .collect()
    }

    fn key() -> AnnKey {
        AnnKey {
            project_id: 1,
            generation: 1,
            model_id: "test/model@d32".to_owned(),
            dim: TEST_DIM,
            chunk_count: TEST_COUNT,
            chunk_set_digest: "abcd1234".to_owned(),
            format_version: ANN_FORMAT_VERSION,
        }
    }

    fn exact_top_k(query: &[f32], rows: &[(u64, Arc<[f32]>)], fetch: usize) -> Vec<(u64, f32)> {
        crate::hybrid::dense::top_k(query, rows, fetch).expect("exact top_k")
    }

    #[test]
    fn ef_scales_with_fetch_and_caps() {
        assert_eq!(ef_for(0), EF_SEARCH_DEFAULT);
        assert_eq!(ef_for(10), EF_SEARCH_DEFAULT);
        assert_eq!(ef_for(50), 200);
        assert_eq!(ef_for(1000), 4_000);
        assert_eq!(ef_for(usize::MAX), EF_SEARCH_MAX);
    }

    #[test]
    fn mmap_and_heap_load_agree_with_each_other() {
        let rows = random_rows(TEST_COUNT, TEST_DIM);
        let ann = AnnIndex::build(&rows, TEST_DIM).expect("build");
        let cache = TempDir::new().expect("cache");
        let store = AnnStore::new(cache.path().to_path_buf());
        let slot = ann.dump(&store, &key()).expect("dump");
        let query = &rows[0].1;

        let mmap = AnnIndex::load(&slot).expect("mmap load");
        let heap = AnnIndex {
            kind: Kind::Loaded(load_loaded(&slot.dir, false).expect("heap load")),
            dim: TEST_DIM,
            count: TEST_COUNT,
        };
        let a = mmap.search(query, 10).expect("mmap search");
        let b = heap.search(query, 10).expect("heap search");
        assert_eq!(a, b);
    }

    #[test]
    fn corrupt_dump_load_errors_instead_of_panicking() {
        let rows = random_rows(TEST_COUNT, TEST_DIM);
        let ann = AnnIndex::build(&rows, TEST_DIM).expect("build");
        let cache = TempDir::new().expect("cache");
        let store = AnnStore::new(cache.path().to_path_buf());
        let slot = ann.dump(&store, &key()).expect("dump");
        std::fs::write(slot.dir.join("index.hnsw.graph"), b"garbage").expect("corrupt");
        assert!(load_loaded(&slot.dir, true).is_err());
        assert!(load_loaded(&slot.dir, false).is_err());
    }

    #[test]
    fn recall_vs_ef_curve_is_measured() {
        let rows = random_rows(TEST_COUNT, TEST_DIM);
        let ann = AnnIndex::build(&rows, TEST_DIM).expect("build");
        let queries: Vec<&[f32]> = rows[..100].iter().map(|(_, v)| v.as_ref()).collect();
        let exact: Vec<Vec<(u64, f32)>> = queries
            .iter()
            .map(|query| exact_top_k(query, &rows, 10))
            .collect();
        let mut observed = Vec::new();
        for ef in [16, 32, 64, 128, 256] {
            let mut total = 0.0;
            for (query, exact) in queries.iter().zip(&exact) {
                let hits = ann.search_with_ef(query, 10, ef).expect("search");
                let exact_ids: Vec<u64> = exact.iter().map(|(id, _)| *id).collect();
                let overlap = hits.iter().filter(|(id, _)| exact_ids.contains(id)).count();
                total += overlap as f64 / hits.len() as f64;
            }
            let mean = total / queries.len() as f64;
            observed.push((ef, mean));
            eprintln!("recall@10 ef={ef}: {mean:.4}");
        }
        let best = observed.last().expect("curve").1;
        assert!(best >= 0.95, "recall@10 at ef=256 too low: {best}");
    }
}
