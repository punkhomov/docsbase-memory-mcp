//! End-to-end ANN path (spec ann-index T10): real `DenseCache` + `VectorCache`
//! coverage, background build, and `search_docs_with` on a synthetic project
//! large enough to exercise the ANN tier.

#![allow(clippy::cast_precision_loss)]

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::{run_project_index, search_docs_with};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
use docsbase_memory::hybrid::jobs::{coverage_with, embed_project_chunks_with};
use docsbase_memory::hybrid::{HybridStages, Ranking};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const VECTORS: usize = 8_000;
const DIM: usize = 256;

struct DimEmbedder;

impl Embedder for DimEmbedder {
    fn model_id(&self) -> &'static str {
        "e2e/model@d256@l2"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; DIM];
                vector[0] = text.len() as f32 / 10_000.0;
                vector[1] = 1.0;
                vector
            })
            .collect())
    }
}

fn project_with_many_docs() -> (TempDir, Db, Project) {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for i in 0..VECTORS {
        let body = format!("# doc {i}\n\nbody {i} {}\n", "x".repeat(200 + i % 300));
        fs::write(root.path().join(format!("d{i:05}.md")), body).expect("write doc");
    }
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
    run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
    project.status = ProjectStatus::Indexed;
    (cache, db, project)
}

fn stages() -> HybridStages {
    HybridStages {
        embedder: Some(Arc::new(DimEmbedder)),
        reranker: None,
        ranking: Ranking::Rrf,
    }
}

fn search(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["path"].as_str().expect("path").to_owned())
        .collect()
}

fn wait_for_ann(dense: &DenseCache, db: &Db, project_id: i64) -> Arc<AnnIndex> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match dense
            .get(db, project_id, DimEmbedder.model_id())
            .expect("get")
        {
            DenseRows::Ann(ann) => return ann,
            _ if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => panic!("no ANN within timeout: {other:?}"),
        }
    }
}

#[test]
fn ann_e2e_8k_x_256() {
    let (cache, db, project) = project_with_many_docs();
    let stored = embed_project_chunks_with(&db, &DimEmbedder, project.id).expect("embed");
    assert!(stored >= VECTORS as u64, "every doc has a vector");

    let coverage = coverage_with(&db, &DimEmbedder, project.id).expect("coverage");
    assert_eq!(
        coverage.cached, coverage.total,
        "ANN requires full coverage"
    );

    let limits = DenseCacheLimits {
        min_vectors: 2,
        ..DenseCacheLimits::default()
    };
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);
    let ann = wait_for_ann(&dense, &db, project.id);
    assert!(ann.estimated_bytes() > 0);

    let value = search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 100 }),
        50,
    )
    .expect("search");
    let hits = search(&value);
    assert_eq!(hits.len(), 100, "high fetch must return a full window");
    let repeat = search(
        &search_docs_with(
            &stages(),
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 100 }),
            50,
        )
        .expect("repeat"),
    );
    assert_eq!(hits, repeat, "ANN answers deterministically");
}
