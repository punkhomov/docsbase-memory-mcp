//! Scale measurement for `AnnIndex` (spec ann-index T6; ignored in CI, run
//! with `-- --ignored`): build/dump/load times, p50/p95 latency, recall@10
//! and estimated memory at 75k x 2048.

#![allow(clippy::cast_precision_loss)]

use std::time::Instant;

use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};
use support::ann::{cluster_vectors, interleaved_rows};

mod support;

const DIM: usize = 2048;
const COUNT: usize = 75_000;
const PER_CLUSTER: usize = COUNT / 2;

fn key() -> AnnKey {
    AnnKey {
        project_id: 1,
        generation: 1,
        model_id: "test/model@d2048".to_owned(),
        dim: DIM,
        chunk_count: COUNT,
        chunk_set_digest: "abcd1234".to_owned(),
        format_version: ANN_FORMAT_VERSION,
    }
}

fn pct(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    let index = (sorted.len() * numerator / denominator).min(sorted.len() - 1);
    sorted[index]
}

#[test]
#[ignore = "manual scale run (75k x 2048); see spec T6"]
fn ann_scale_75k_x_2048() {
    let start = Instant::now();
    let a = cluster_vectors(PER_CLUSTER, DIM, 4.0);
    let b = cluster_vectors(PER_CLUSTER, DIM, -4.0);
    let rows = interleaved_rows(&a, &b);
    eprintln!(
        "data: {} rows x {DIM} generated in {:?}",
        rows.len(),
        start.elapsed()
    );

    let build_start = Instant::now();
    let ann = AnnIndex::build(&rows, DIM).expect("build");
    let build_elapsed = build_start.elapsed();
    eprintln!("build: {build_elapsed:?} (parallel insert, 0.3.5)");
    eprintln!("estimated_bytes: {}", ann.estimated_bytes());

    let cache = tempfile::TempDir::new().expect("cache");
    let store = AnnStore::new(cache.path().to_path_buf());
    let dump_start = Instant::now();
    let slot = ann.dump(&store, &key()).expect("dump");
    eprintln!("dump: {:?}", dump_start.elapsed());

    let load_start = Instant::now();
    let loaded = AnnIndex::load(&slot).expect("load");
    eprintln!("load (mmap): {:?}", load_start.elapsed());

    let mut latencies = Vec::with_capacity(100);
    let mut recall_total = 0.0;
    let queries = a.iter().take(100).collect::<Vec<_>>();
    for vector in &queries {
        let q = Instant::now();
        let hits = loaded.search(vector.as_ref(), 10).expect("search");
        latencies.push(q.elapsed().as_secs_f64() * 1000.0);
        let a_hits = hits.iter().filter(|(id, _)| id % 2 == 0).count();
        recall_total += a_hits as f64 / hits.len() as f64;
    }
    latencies.sort_by(f64::total_cmp);
    let p50 = pct(&latencies, 1, 2);
    let p95 = pct(&latencies, 95, 100);
    eprintln!(
        "search: p50={p50:.2}ms p95={p95:.2}ms over {} queries",
        latencies.len()
    );
    eprintln!(
        "recall@10 (separated clusters): {:.4}",
        recall_total / 100.0
    );
    eprintln!(
        "dump entries: {}",
        std::fs::read_dir(&slot.dir).expect("read").count()
    );
}
