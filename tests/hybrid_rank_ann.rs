//! End-to-end search through the `search_docs_with` seam with the ANN tier
//! enabled (spec ann-index T4/T5): search stays correct during a background
//! build (exact streaming, SC-11) and keeps using the persisted ANN once it
//! is published.

#![allow(clippy::cast_precision_loss, clippy::field_reassign_with_default)]

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::{run_project_index, search_docs_with};
use docsbase_memory::error::Result;
use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits, DenseRows};
use docsbase_memory::hybrid::jobs::embed_project_chunks_with;
use docsbase_memory::hybrid::{HybridStages, Ranking};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::{Value, json};
use tempfile::TempDir;

const VECTORS: usize = 600;
const DIM: usize = 64;

struct LengthEmbedder;

impl Embedder for LengthEmbedder {
    fn model_id(&self) -> &'static str {
        "rank/model@d64@l2"
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
        let body = "x".repeat(1000 + i * 100);
        fs::write(root.path().join(format!("d{i:04}.md")), body).expect("write doc");
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
        embedder: Some(Arc::new(LengthEmbedder)),
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
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match dense
            .get(db, project_id, LengthEmbedder.model_id())
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
fn search_is_correct_during_build_and_after_ann_publish() {
    let (cache, db, project) = project_with_many_docs();
    let stored = embed_project_chunks_with(&db, &LengthEmbedder, project.id).expect("embed");
    assert!(
        stored >= VECTORS as u64,
        "every doc produced at least one chunk"
    );

    let mut limits = DenseCacheLimits::default();
    limits.min_vectors = 2;
    let dense = DenseCache::with_policy_limits(AnnPolicy::BuildOrLoad, cache.path(), limits);

    let first = dense
        .get(&db, project.id, LengthEmbedder.model_id())
        .expect("get");
    let was_building = matches!(first, DenseRows::Building(_));

    let value = search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        50,
    )
    .expect("search during build");
    let during_build = search(&value);
    assert_eq!(during_build.len(), 10);
    assert!(
        !during_build.is_empty(),
        "search during build must return exact results"
    );
    for pair in during_build.windows(2) {
        assert!(
            pair[0] <= pair[1],
            "deterministic ranking: {during_build:?}"
        );
    }

    let _ann = wait_for_ann(&dense, &db, project.id);
    let value = search_docs_with(
        &stages(),
        &dense,
        &db,
        cache.path(),
        &project,
        &json!({ "query": "alpha", "limit": 10 }),
        50,
    )
    .expect("search after publish");
    let after_publish = search(&value);
    let repeat = search(
        &search_docs_with(
            &stages(),
            &dense,
            &db,
            cache.path(),
            &project,
            &json!({ "query": "alpha", "limit": 10 }),
            50,
        )
        .expect("search after publish, repeat"),
    );
    assert_eq!(
        after_publish, repeat,
        "persisted ANN answers deterministically"
    );
    eprintln!(
        "first get observed state: {}",
        if was_building {
            "Building"
        } else {
            "Ann/Ready"
        }
    );
}
