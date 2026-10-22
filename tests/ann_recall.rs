//! `AnnIndex` behavior on the public API (spec ann-index T3): recall on
//! separated clusters, persisted round-trip, score scale and tie-breaks.
//! Internal recall-vs-ef measurement and mmap/heap load parity live in
//! `src/hybrid/ann.rs`.

#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;

use tempfile::TempDir;

use docsbase_memory::hybrid::ann::{ANN_MIN_VECTORS, AnnIndex};
use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};
use support::ann::{cluster_vectors, interleaved_rows};

mod support;

const DIM: usize = 64;
const PER_CLUSTER: usize = 2500;

fn key() -> AnnKey {
    AnnKey {
        project_id: 1,
        generation: 1,
        model_id: "test/model@d64".to_owned(),
        dim: DIM,
        chunk_count: 2 * PER_CLUSTER,
        chunk_set_digest: "abcd1234".to_owned(),
        format_version: ANN_FORMAT_VERSION,
    }
}

#[test]
fn build_search_recall_on_separated_clusters() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    let ann = AnnIndex::build(&rows, DIM).expect("build");

    let mut cluster_a_hits = 0usize;
    let mut total = 0usize;
    for vector in a.iter().take(100) {
        let hits = ann.search(vector.as_ref(), 10).expect("search");
        assert_eq!(hits.len(), 10);
        cluster_a_hits += hits.iter().filter(|(id, _)| id % 2 == 0).count();
        total += 10;
    }
    let recall = cluster_a_hits as f64 / total as f64;
    assert!(
        recall >= 0.99,
        "recall@10 on separated clusters too low: {recall}"
    );
}

#[test]
fn build_dump_load_roundtrip_preserves_neighbors() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    let ann = AnnIndex::build(&rows, DIM).expect("build");

    let cache = TempDir::new().expect("cache");
    let store = AnnStore::new(cache.path().to_path_buf());
    let slot = ann.dump(&store, &key()).expect("dump");
    let loaded = AnnIndex::load(&slot).expect("load");

    for vector in a.iter().take(20) {
        let before = ann.search(vector.as_ref(), 10).expect("built search");
        let after = loaded.search(vector.as_ref(), 10).expect("loaded search");
        assert_eq!(before, after, "neighbors must survive dump/load");
    }
}

#[test]
fn scores_are_one_minus_cosine() {
    let rows: Vec<(u64, Arc<[f32]>)> = vec![
        (1, Arc::from(vec![1.0_f32, 0.0])),
        (2, Arc::from(vec![0.0_f32, 1.0])),
        (3, Arc::from(vec![-1.0_f32, 0.0])),
    ];
    let ann = AnnIndex::build(&rows, 2).expect("build");
    let hits = ann.search(&[1.0, 0.0], 3).expect("search");
    let scores: Vec<f32> = hits.iter().map(|(_, score)| *score).collect();
    assert_eq!(
        hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (actual, expected) in scores.iter().zip([1.0, 0.0, -1.0]) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "score {actual} != {expected}"
        );
    }
}

#[test]
fn tie_break_by_chunk_id() {
    let rows: Vec<(u64, Arc<[f32]>)> = vec![
        (5, Arc::from(vec![1.0_f32, 0.0])),
        (2, Arc::from(vec![0.0_f32, 1.0])),
    ];
    let ann = AnnIndex::build(&rows, 2).expect("build");
    let hits = ann
        .search(&[0.5_f32.sqrt(), 0.5_f32.sqrt()], 2)
        .expect("search");
    assert_eq!(hits.len(), 2, "both tied neighbours must be returned");
    assert_eq!(
        hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![2, 5],
        "equal scores order by ascending chunk_id"
    );
    assert!(
        hits.iter()
            .all(|(_, score)| (score - 0.5_f32.sqrt()).abs() < 1e-4)
    );
}

#[test]
fn empty_query_is_an_error() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let ann = AnnIndex::build(&interleaved_rows(&a, &b), DIM).expect("build");
    let err = ann.search(&[], 10).expect_err("empty query");
    assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn dimension_mismatch_is_an_error() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let ann = AnnIndex::build(&interleaved_rows(&a, &b), DIM).expect("build");
    let err = ann
        .search(&vec![0.0; DIM + 1], 10)
        .expect_err("dim mismatch");
    assert!(err.to_string().contains("dim"), "{err}");
}

#[test]
fn build_rejects_dim_mismatch_in_rows() {
    let rows: Vec<(u64, Arc<[f32]>)> = vec![(1, Arc::from(vec![0.0_f32, 0.0]))];
    let err = AnnIndex::build(&rows, 3).expect_err("dim mismatch");
    assert!(err.to_string().contains("dim"), "{err}");
    assert!(
        AnnIndex::build(&[], 3)
            .expect_err("empty rows")
            .to_string()
            .contains("vector")
    );
}

#[test]
fn estimated_bytes_scales_with_count_and_dim() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    let ann = AnnIndex::build(&rows, DIM).expect("build");
    let bytes = ann.estimated_bytes();
    assert!(bytes > 0);
    let small = AnnIndex::build(&rows[..2], DIM).expect("build");
    assert!(small.estimated_bytes() < bytes);
}

#[test]
fn min_vectors_threshold_is_defined() {
    fn threshold_above(corpus: usize) -> bool {
        ANN_MIN_VECTORS > corpus
    }
    assert!(
        threshold_above(331),
        "golden corpus stays below the threshold"
    );
}
