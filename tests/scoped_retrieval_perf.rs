//! Manual, offline 20k diagnostic. No thresholds or baseline updates.
use docsbase_memory::vector_cache::Embedder;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

struct Geometry;
impl Embedder for Geometry {
    fn model_id(&self) -> &'static str {
        "scope-perf@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::hybrid::HybridStages;
use docsbase_memory::hybrid::dense::DenseCache;
use docsbase_memory::index::tantivy_index::ReadIndex;
use docsbase_memory::store::models::ProjectStatus;
use docsbase_memory::store::{Db, repo};
use serde_json::json;
use tantivy::TantivyDocument;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value as _;

// Query-first reference for this all-short-chunk fixture only: grow the raw
// BM25 collector until ten eligible hits or exhaustion. Deliberately uncapped
// by the public reader's MAX_HITS, so it does not silently return a short list.
// It is a measurement reference, not a production implementation/proposal.
fn query_first(index: &ReadIndex, db: &Db, query: &str, prefix: &str) -> (usize, usize) {
    let searcher = index.reader().searcher();
    let schema = searcher.schema();
    let field = schema.get_field("text").expect("text");
    let doc = schema.get_field("doc_id").expect("doc id");
    let parser = QueryParser::for_index(searcher.index(), vec![field]);
    let parsed = parser.parse_query(query).expect("query");
    let mut fetch = 16;
    let mut examined = 0;
    loop {
        let hits = searcher
            .search(&parsed, &TopDocs::with_limit(fetch).order_by_score())
            .expect("reference");
        let ids: Vec<_> = hits
            .iter()
            .map(|(_, address)| {
                let row: TantivyDocument = searcher.doc(*address).expect("document");
                row.get_first(doc)
                    .and_then(|value| value.as_i64())
                    .expect("id")
            })
            .collect();
        examined += ids.len();
        let metadata = repo::doc_meta(db.connection(), &ids).expect("paths");
        let count = ids
            .iter()
            .filter(|id| {
                metadata
                    .get(id)
                    .is_some_and(|meta| meta.rel_path.starts_with(prefix))
            })
            .count();
        if count >= 10 || ids.len() < fetch || fetch >= 20_000 {
            return (count.min(10), examined);
        }
        fetch = (fetch * 4).min(20_000);
    }
}

#[test]
#[ignore = "manual 20k diagnostic; creates a real file/SQLite/Tantivy corpus"]
fn two_folder_scope_diagnostics() {
    let cache = tempfile::tempdir().expect("cache");
    let root = tempfile::tempdir().expect("root");
    for folder in ["a", "b"] {
        fs::create_dir(root.path().join(folder)).expect("directory");
        for id in 0..10_000 {
            let rare = if id % 1000 == 0 {
                "rareword"
            } else {
                "ordinary"
            };
            fs::write(
                root.path().join(format!("{folder}/{id:05}.md")),
                format!("# Manual\n\nshared {rare} document {id}\n"),
            )
            .expect("file");
        }
    }
    let mut db = Db::open(cache.path()).expect("db");
    let mut project = registry::ensure_project(&mut db, root.path()).expect("project");
    tools::run_project_index(&mut db, cache.path(), &project, &Config::default()).expect("index");
    project.status = ProjectStatus::Indexed;
    docsbase_memory::hybrid::jobs::embed_project_chunks_with(&db, &Geometry, project.id)
        .expect("offline fixture vectors");
    for (query, prefix) in [
        ("shared", "b/"),
        ("rareword", "b/"),
        ("shared", "b/09999.md"),
    ] {
        let dense = DenseCache::new();
        for phase in ["first", "warm"] {
            let started = Instant::now();
            let result = tools::search_docs_with(
                &HybridStages::default(),
                &dense,
                &db,
                cache.path(),
                &project,
                &json!({"query":query,"limit":10,"path_prefix":prefix}),
                40,
            )
            .expect("scoped");
            let scope_ms = started.elapsed().as_secs_f64() * 1000.0;
            let hybrid = HybridStages {
                embedder: Some(Arc::new(Geometry)),
                ranking: docsbase_memory::hybrid::Ranking::Rrf,
                reranker: None,
            };
            let dense_started = Instant::now();
            let dense_result = tools::search_docs_with(
                &hybrid,
                &dense,
                &db,
                cache.path(),
                &project,
                &json!({"query":query,"limit":10,"path_prefix":prefix}),
                40,
            )
            .expect("scoped dense");
            let dense_ms = dense_started.elapsed().as_secs_f64() * 1000.0;
            assert!(dense.ann_snapshot(project.id).is_none());
            assert!(
                dense_result
                    .as_array()
                    .expect("hits")
                    .iter()
                    .all(|hit| hit["path"].as_str().expect("path").starts_with(prefix))
            );
            let index = ReadIndex::open(
                &cache
                    .path()
                    .join(format!("projects/{}/tantivy", project.id)),
            )
            .expect("reference index");
            let started = Instant::now();
            let (reference_count, examined) = query_first(&index, &db, query, prefix);
            let reference_ms = started.elapsed().as_secs_f64() * 1000.0;
            let count = result.as_array().expect("hits").len();
            assert_eq!(count, reference_count);
            assert!(
                result
                    .as_array()
                    .expect("hits")
                    .iter()
                    .all(|hit| hit["path"].as_str().expect("path").starts_with(prefix))
            );
            println!(
                "query={query} prefix={prefix} phase={phase} hits={count} scoped_ms={scope_ms:.3} scoped_rrf_ms={dense_ms:.3} query_first_ms={reference_ms:.3} reference_examined={examined}"
            );
        }
    }
}
