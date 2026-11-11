//! `AnnIndex` behavior on the public API (spec ann-index T3): recall on
//! separated clusters, persisted round-trip, score scale and tie-breaks.
//! Internal recall-vs-ef measurement and mmap/heap load parity live in
//! `src/hybrid/ann.rs`.

#![allow(clippy::cast_precision_loss)]

use std::collections::HashSet;
use std::sync::Arc;

use tempfile::TempDir;

use docsbase_memory::hybrid::ann::{ANN_MIN_VECTORS, AnnIndex};
use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};
use docsbase_memory::hybrid::dense::top_k;
use support::ann::{cluster_vectors, interleaved_rows};

mod support;

#[path = "support/ann_capture.rs"]
mod capture;

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

/// Query validation runs before graph search and does not need a recall corpus.
fn small_search_index() -> AnnIndex {
    let rows: Vec<(u64, Arc<[f32]>)> = vec![
        (1, Arc::from(vec![1.0_f32, 0.0])),
        (2, Arc::from(vec![0.0_f32, 1.0])),
    ];
    let ann = AnnIndex::build(&rows, 2).expect("build validation index");
    let hits = ann.search(&[1.0, 0.0], 1).expect("valid query control");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, 1, "fixture must answer a valid query");
    ann
}

#[test]
fn build_search_recall_on_separated_clusters() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    let ann = AnnIndex::build(&rows, DIM).expect("build");

    let observations: Vec<_> = a
        .iter()
        .take(100)
        .map(|vector| capture::Observation {
            query: vector.to_vec(),
            hits: ann.search(vector.as_ref(), 10).expect("search"),
        })
        .collect();
    let recall = cluster_a_recall(&observations, 10);
    if recall < 0.99 || observations.iter().any(|o| o.hits.len() != 10) {
        let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/ci-observations/ann-recall");
        match capture::save(&parent, &ann, &key(), 10, &observations) {
            Ok(bundle) => eprintln!("ANN recall failure bundle: {}", bundle.display()),
            Err(error) => eprintln!("ANN recall failure capture failed: {error}"),
        }
        for (i, observation) in observations.iter().enumerate() {
            if observation.hits.len() != 10 || observation.hits.iter().any(|(id, _)| id % 2 != 0) {
                eprintln!("query {i}: {:?}", observation.hits);
            }
        }
    }
    assert!(observations.iter().all(|o| o.hits.len() == 10));
    assert!(
        recall >= 0.99,
        "recall@10 on separated clusters too low: {recall}"
    );
}

fn cluster_a_recall(observations: &[capture::Observation], fetch: usize) -> f64 {
    let hits = observations
        .iter()
        .flat_map(|o| &o.hits)
        .filter(|(id, _)| id % 2 == 0)
        .count();
    hits as f64 / (observations.len() * fetch) as f64
}

#[test]
fn failure_capture_replays_the_saved_graph_and_queries() {
    let rows = vec![
        (0, Arc::from(vec![1.0_f32, 0.0])),
        (1, Arc::from(vec![-1.0_f32, 0.0])),
    ];
    let ann = AnnIndex::build(&rows, 2).expect("build");
    let query = vec![1.0, 0.0];
    let observations = vec![capture::Observation {
        hits: ann.search(&query, 2).expect("search"),
        query,
    }];
    let mut key = key();
    key.dim = 2;
    key.chunk_count = 2;
    let parent = TempDir::new().expect("capture parent");
    let bundle = capture::save(parent.path(), &ann, &key, 2, &observations).expect("capture");
    drop(ann);
    let (saved, replay) = capture::load(&bundle).expect("load capture");
    assert_eq!(saved.observations, observations);
    assert_eq!(saved.fetch, 2);
    assert_eq!(saved.meta.key(), key);
    for observation in &saved.observations {
        assert_eq!(
            replay
                .search(&observation.query, saved.fetch)
                .expect("replay"),
            observation.hits
        );
    }
    assert!((cluster_a_recall(&saved.observations, 2) - 0.5).abs() < f64::EPSILON);
}

#[test]
#[ignore = "requires an explicit captured ANN bundle; never rebuilds the graph"]
fn replay_captured_separated_cluster_failure() {
    let bundle = std::env::var_os("DOCSBASE_ANN_REPLAY_BUNDLE")
        .expect("set DOCSBASE_ANN_REPLAY_BUNDLE to the failure directory");
    let (saved, ann) = capture::load(std::path::Path::new(&bundle)).expect("load capture");
    assert_eq!(saved.fetch, 10, "separated-cluster capture contract");
    assert_eq!(saved.observations.len(), 100, "original query count");
    let replayed: Vec<_> = saved
        .observations
        .iter()
        .map(|observation| capture::Observation {
            query: observation.query.clone(),
            hits: ann.search(&observation.query, saved.fetch).expect("replay"),
        })
        .collect();
    let before = cluster_a_recall(&saved.observations, saved.fetch);
    let after = cluster_a_recall(&replayed, saved.fetch);
    eprintln!("captured recall={before:.3}; replay recall={after:.3}");
    assert!(replayed.iter().all(|o| o.hits.len() == saved.fetch));
    assert!(after >= 0.99, "replayed recall@10 too low: {after}");
}

#[test]
fn high_fetch_recall_matches_the_exact_scan() {
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    let ann = AnnIndex::build(&rows, DIM).expect("build");

    // `ef_for` scales with fetch (max(64, fetch * 4), capped); Delta 0.2 made
    // that rule final, so recall must hold at the top of the RRF window too.
    for fetch in [100usize, 1000] {
        let mut hit = 0usize;
        let mut total = 0usize;
        for vector in a.iter().take(20) {
            let exact = top_k(vector.as_ref(), &rows, fetch).expect("exact scan");
            let exact_ids: HashSet<u64> = exact.iter().map(|(id, _)| *id).collect();
            let approx = ann.search(vector.as_ref(), fetch).expect("ann search");
            assert_eq!(approx.len(), fetch);
            hit += approx
                .iter()
                .filter(|(id, _)| exact_ids.contains(id))
                .count();
            total += fetch;
        }
        let recall = hit as f64 / total as f64;
        assert!(
            recall >= 0.95,
            "recall@{fetch} against the exact scan too low: {recall:.3}"
        );
    }
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
    let ann = small_search_index();
    let err = ann.search(&[], 10).expect_err("empty query");
    assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn dimension_mismatch_is_an_error() {
    let ann = small_search_index();
    for query in [&[1.0][..], &[1.0, 0.0, 0.0][..]] {
        let err = ann.search(query, 10).expect_err("dim mismatch");
        assert!(err.to_string().contains("dim"), "{err}");
    }
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
