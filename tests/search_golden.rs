//! Golden retrieval tests for SC-1…SC-4 on the benchmark corpus
//! (`tests/fixtures/bench/`, FR-19/FR-20; insta snapshots).

use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::tools;
use docsbase_memory::index::job::run_full;
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bench")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture");
        }
    }
}

struct Bench {
    cache: TempDir,
    _root: TempDir,
    db: Db,
    _index: IndexHandle,
    project: Project,
}

impl Bench {
    fn new() -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        copy_dir(&fixture_root(), root.path());
        let mut db = Db::open(cache.path()).expect("db");
        let canonical_root = root.path().canonicalize().expect("canonical root");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (1, ?1, 'bench', 'not_indexed', 1, 0)",
            [canonical_root.to_string_lossy().as_ref()],
        )
        .expect("insert project");
        drop(conn);
        let mut index =
            IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).expect("index");
        let mut project = Project {
            id: 1,
            canonical_root,
            name: "bench".to_owned(),
            status: ProjectStatus::NotIndexed,
            schema_version: docsbase_memory::store::migrations::SCHEMA_VERSION,
            created_at: 0,
            last_indexed_at: None,
        };
        let stats = run_full(&mut db, &mut index, &project, &Config::default()).expect("run_full");
        assert!(stats.docs >= 8, "corpus indexed: {stats:?}");
        project.status = ProjectStatus::Indexed;
        Self {
            cache,
            _root: root,
            db,
            _index: index,
            project,
        }
    }
    /// Top-`limit` citations in rank order (the shipped `search_docs`
    /// payload, FR-20).
    fn search(&self, query: &str, limit: usize) -> Vec<serde_json::Value> {
        let value = tools::search_docs(
            &self.db,
            self.cache.path(),
            &self.project,
            &json!({ "query": query, "limit": limit }),
        )
        .expect("search_docs");
        value.as_array().expect("citation array").clone()
    }

    fn paths(&self, query: &str, limit: usize) -> Vec<String> {
        self.search(query, limit)
            .iter()
            .map(|row| row["path"].as_str().expect("path").to_owned())
            .collect()
    }
}

/// The stable part of each citation in rank order. BM25 scores are
/// engine-internal and drift across tantivy upgrades (see the 0.25→0.26
/// migration), while SC-1…SC-4 judge *which* chunks rank where — so scores
/// are never snapshotted.
fn citations(hits: &[serde_json::Value]) -> Vec<serde_json::Value> {
    hits.iter()
        .map(|hit| {
            json!({
                "chunk_id": hit["chunk_id"],
                "path": hit["path"],
                "heading_path": hit["heading_path"],
                "lines": hit["lines"],
            })
        })
        .collect()
}

/// SC-1: exact identifier → relevant document within top-3.
#[test]
fn exact_identifier_top3() {
    let bench = Bench::new();
    let hits = bench.search("assessment_plan_id", 3);
    let top = bench.paths("assessment_plan_id", 3);
    assert!(
        top.iter().any(|p| p == "backend/assessment-plan-api.md"),
        "expected assessment-plan-api.md in top-3, got {top:?}"
    );
    insta::assert_json_snapshot!(citations(&hits));
}

/// SC-2: exact error message → relevant chunk in top-1.
#[test]
fn error_message_top1() {
    let bench = Bench::new();
    let hits = bench.search(
        "Body thickness and Sheet Metal component rule thickness are different",
        3,
    );
    assert_eq!(
        hits[0]["path"].as_str().expect("path"),
        "cad/sheet-metal-rules.md",
        "top-1 path"
    );
    insta::assert_json_snapshot!(citations(&hits));
}

/// SC-3: natural-language question → relevant chunk in top-3.
#[test]
fn semantic_top3() {
    let bench = Bench::new();
    let hits = bench.search("How should authentication tokens be refreshed?", 3);
    let top = bench.paths("How should authentication tokens be refreshed?", 3);
    assert!(
        top.iter().any(|p| p == "auth/token-refresh.md"),
        "expected token-refresh.md in top-3, got {top:?}"
    );
    insta::assert_json_snapshot!(citations(&hits));
}

/// SC-4: code/API wording → relevant chunk in top-3.
#[test]
fn code_api_top3() {
    let bench = Bench::new();
    let hits = bench.search("defineStore setup store syntax", 3);
    let top = bench.paths("defineStore setup store syntax", 3);
    assert!(
        top.iter().any(|p| p == "frontend/pinia-setup-stores.md"),
        "expected pinia-setup-stores.md in top-3, got {top:?}"
    );
    insta::assert_json_snapshot!(citations(&hits));
}
