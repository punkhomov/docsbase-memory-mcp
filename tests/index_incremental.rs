use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::config::Config;
use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::job::{JobStats, run_full, run_incremental_with};
use docsbase_memory::index::tantivy_index::{Hit, IndexHandle};
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use rusqlite::Connection;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    root: TempDir,
    db: Db,
    index: IndexHandle,
    project: Project,
    config: Config,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        let db = Db::open(cache.path()).expect("db");
        let canonical_root = root.path().canonicalize().expect("canonical root");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (1, ?1, 'test', 'not_indexed', 1, 0)",
            [canonical_root.to_string_lossy().as_ref()],
        )
        .expect("insert project");
        drop(conn);

        let index =
            IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).expect("index");
        let project = Project {
            id: 1,
            canonical_root,
            name: "test".to_owned(),
            status: ProjectStatus::NotIndexed,
            schema_version: 1,
            created_at: 0,
            last_indexed_at: None,
        };
        Self {
            cache,
            root,
            db,
            index,
            project,
            config: Config::default(),
        }
    }

    fn full(&mut self) -> JobStats {
        run_full(&mut self.db, &mut self.index, &self.project, &self.config).expect("full")
    }

    fn incremental(&mut self, changed: &[PathBuf]) -> JobStats {
        run_incremental_with(
            &mut self.db,
            &mut self.index,
            &self.project,
            changed,
            &self.config,
        )
        .expect("incremental")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn search(&self, query: &str) -> Vec<Hit> {
        self.index.search(query, 10).expect("search")
    }

    fn count(&self, sql: &str) -> i64 {
        let conn = Connection::open(self.cache.path().join(DB_FILE)).expect("raw db");
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

const PLAIN: &str = "# Plain\n\nSome prose about widgets and gadgets.\n";

fn alpha() -> &'static str {
    "# Alpha\n\nalpha content here.\n"
}

fn beta() -> &'static str {
    "# Story\n\nbeta content here.\n"
}

#[test]
fn changed_file_updates_chunks() {
    let mut env = Env::new(&[("a.md", alpha()), ("b.md", PLAIN)]);
    env.full();
    write_file(env.root.path(), "a.md", beta().as_bytes());

    let changed = [env.path("a.md")];
    let stats = env.incremental(&changed);
    assert_eq!(stats.docs, 1);
    assert_eq!(stats.skipped, 0);
    assert_eq!(stats.removed, 0);
    assert!(stats.chunks > 0);
    assert!(env.search("alpha").is_empty(), "old text gone");
    assert!(!env.search("beta").is_empty(), "new text indexed");
    assert!(!env.search("widgets").is_empty(), "untouched doc intact");
}

#[test]
fn unchanged_skipped() {
    let mut env = Env::new(&[("a.md", alpha())]);
    env.full();

    let stats = env.incremental(&[env.path("a.md")]);
    assert_eq!(
        stats,
        JobStats {
            docs: 0,
            chunks: 0,
            skipped: 1,
            removed: 0,
            errors: 0,
        }
    );
}

#[test]
fn deleted_file_purged() {
    let mut env = Env::new(&[("a.md", alpha()), ("b.md", PLAIN)]);
    env.full();
    let gone = env.path("b.md");
    fs::remove_file(&gone).expect("remove");

    let stats = env.incremental(&[gone]);
    assert_eq!(stats.removed, 1);
    assert!(env.search("widgets").is_empty(), "purged from tantivy");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
}

#[test]
fn rename_counts_as_delete_plus_add() {
    let mut env = Env::new(&[("a.md", alpha())]);
    env.full();
    let old = env.path("a.md");
    let new = env.path("c.md");
    fs::rename(&old, &new).expect("rename");

    let stats = env.incremental(&[old, new]);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.docs, 1);
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
    assert!(!env.search("alpha").is_empty(), "content survives rename");
}

#[test]
fn outside_root_warns() {
    let mut env = Env::new(&[("a.md", alpha())]);
    env.full();

    let stray = env.cache.path().join("stray.md");
    let stats = env.incremental(&[stray]);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.docs, 0);
    assert_eq!(stats.removed, 0);
}

#[test]
fn crash_marker_converges_on_rerun() {
    let mut env = Env::new(&[("a.md", alpha())]);
    env.full();
    write_file(env.root.path(), "a.md", beta().as_bytes());
    let changed = [env.path("a.md")];
    env.incremental(&changed);
    assert!(!env.search("beta").is_empty(), "beta indexed before crash");

    // Simulate a crash after the tantivy commit: marker set, stale chunks
    // committed for the same doc id, no final SQLite write.
    let conn = Connection::open(env.cache.path().join(DB_FILE)).expect("raw db");
    conn.execute("UPDATE docs SET content_hash = ''", [])
        .expect("marker");
    drop(conn);
    env.index.delete_doc(1);
    env.index
        .add_chunks(&[Chunk {
            doc_id: 1,
            seq: 0,
            heading_path: Vec::new(),
            kind: ChunkKind::Prose,
            line_start: 1,
            line_end: 1,
            text: "gamma stale content".to_owned(),
        }])
        .expect("stale");
    env.index.commit().expect("commit stale");

    let stats = env.incremental(&changed);
    assert_eq!(stats.docs, 1, "empty hash forces reprocess");
    assert!(env.search("gamma").is_empty(), "stale chunks removed");
    assert!(!env.search("beta").is_empty(), "current content kept");
}

#[test]
fn removed_dir_purges_subtree() {
    let mut env = Env::new(&[
        ("dir/a.md", alpha()),
        ("dir/b.md", PLAIN),
        ("keep.md", PLAIN),
    ]);
    env.full();
    fs::remove_dir_all(env.path("dir")).expect("remove dir");

    let stats = env.incremental(&[env.path("dir")]);
    assert_eq!(stats.removed, 2, "whole subtree purged");
    assert_eq!(env.search("alpha").len(), 0);
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
}

#[test]
fn rename_at_capacity_keeps_document() {
    let mut env = Env::new(&[("a.md", alpha()), ("b.md", PLAIN)]);
    env.config.max_docs_per_project = 2;
    env.full();

    let old = env.path("a.md");
    let new = env.path("c.md");
    fs::rename(&old, &new).expect("rename");
    let stats = env.incremental(&[old, new]);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.docs, 1);
    assert_eq!(stats.errors, 0, "rename must not hit the document budget");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 2);
}

#[test]
fn batch_budget_counts_removals_before_additions() {
    let mut env = Env::new(&[
        ("a.md", alpha()),
        ("b.md", PLAIN),
        ("c.md", "# C\n\nc content.\n"),
        ("d.md", "# D\n\nd content.\n"),
    ]);
    env.config.max_docs_per_project = 4;
    env.full();

    let gone = env.path("a.md");
    fs::remove_file(&gone).expect("remove");
    write_file(env.root.path(), "e.md", b"# E\n\ne content.\n");
    write_file(env.root.path(), "f.md", b"# F\n\nf content.\n");
    let stats = env.incremental(&[gone, env.path("e.md"), env.path("f.md")]);

    assert_eq!(stats.removed, 1);
    assert_eq!(stats.docs, 1, "removal frees exactly one slot");
    assert_eq!(stats.errors, 1, "second addition is over budget");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 4);
}
