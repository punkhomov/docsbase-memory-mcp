use std::fs;
use std::path::Path;

use docsbase_memory::config::Config;
use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::job::{JobStats, run_full};
use docsbase_memory::index::tantivy_index::{Hit, IndexHandle, REBUILD_MARKER};
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
        }
    }

    fn run(&mut self) -> JobStats {
        self.run_with(&Config::default())
    }

    fn run_with(&mut self, config: &Config) -> JobStats {
        run_full(&mut self.db, &mut self.index, &self.project, config).expect("run_full")
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

const GUIDE: &str = "---\ntitle: Setup Guide\ntags: setup, guide\n---\n\n# Install\n\nRun the installer quickly.\n\n## Example\n\n```rust\nlet x = 1;\n```\n";
const PLAIN: &str = "# Plain\n\nSome prose about widgets and gadgets.\n";

fn legacy_schema_index(dir: &Path) {
    use tantivy::Index;
    use tantivy::schema::{
        IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
    };
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let numeric = NumericOptions::default().set_stored().set_indexed();
    let mut builder = Schema::builder();
    builder.add_u64_field("chunk_id", numeric.clone());
    builder.add_i64_field("doc_id", numeric);
    builder.add_text_field("text", text.clone());
    builder.add_text_field("title", text.clone());
    builder.add_text_field("heading_path", text.clone());
    builder.add_text_field("identifiers", text);
    Index::create_in_dir(dir, builder.build()).expect("create legacy index");
}

#[test]
fn legacy_schema_rebuild_repopulates_docs() {
    let mut env = Env::new(&[
        ("a.md", "# A\n\nalphawidget prose\n"),
        ("b.md", "# B\n\nbetawidget prose\n"),
    ]);
    env.run();
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 2);
    assert!(!env.search("alphawidget").is_empty(), "initial index");

    // Simulate an index written by an older build (no `text_len` field).
    let path = env.cache.path().join("projects/1/tantivy");
    std::fs::remove_dir_all(&path).expect("remove index");
    std::fs::create_dir_all(&path).expect("mkdir");
    legacy_schema_index(&path);

    let handle = IndexHandle::open_or_create(&path).expect("reopen");
    assert!(handle.was_recreated());
    // A crash before the rebuild must leave the durable marker behind.
    drop(handle);
    let handle = IndexHandle::open_or_create(&path).expect("reopen after crash");
    assert!(
        handle.was_recreated(),
        "rebuild requirement must survive a crash"
    );
    env.index = handle;
    env.run();
    assert!(
        !path.join(REBUILD_MARKER).exists(),
        "marker cleared after a successful rebuild"
    );

    assert_eq!(
        env.count("SELECT COUNT(*) FROM docs"),
        2,
        "doc ids must be recreated once"
    );
    assert!(
        !env.search("alphawidget").is_empty(),
        "upgraded index must be repopulated"
    );
    assert!(!env.search("betawidget").is_empty(), "second doc rebuilt");
}

#[test]
fn fresh_corpus() {
    let mut env = Env::new(&[("a/guide.md", GUIDE), ("b/plain.md", PLAIN)]);
    let stats = env.run();

    assert_eq!(
        stats,
        JobStats {
            docs: 2,
            chunks: stats.chunks,
            skipped: 0,
            removed: 0,
            errors: 0,
            warnings: Vec::new(),
        }
    );
    assert!(stats.chunks > 0, "chunks indexed");
    assert!(!env.search("installer").is_empty(), "prose searchable");
    assert!(!env.search("widgets").is_empty(), "second doc searchable");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 2);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM chunks"),
        i64::try_from(stats.chunks).expect("chunks fit")
    );

    let conn = Connection::open(env.cache.path().join(DB_FILE)).expect("raw db");
    let (title, frontmatter): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT title, frontmatter_json FROM docs WHERE rel_path = 'a/guide.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("doc row");
    assert_eq!(title.as_deref(), Some("Setup Guide"));
    assert!(
        frontmatter
            .as_ref()
            .is_some_and(|json| json.contains("setup")),
        "{frontmatter:?}"
    );

    let (kind, lang): (String, Option<String>) = conn
        .query_row(
            "SELECT kind, lang FROM chunks WHERE doc_id = 1 AND kind = 'code'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("code chunk");
    assert_eq!(kind, "code");
    assert_eq!(lang.as_deref(), Some("rust"));

    let (first_line, code_line): (i64, i64) = conn
        .query_row(
            "SELECT MIN(line_start), (SELECT line_start FROM chunks WHERE doc_id = 1 AND kind = 'code')
             FROM chunks WHERE doc_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("line rows");
    assert_eq!(first_line, 6, "frontmatter shifts cached file lines");
    assert_eq!(code_line, 10, "code chunk points at its real file line");
}

#[test]
fn crash_between_commits_converges() {
    let mut env = Env::new(&[("a.md", "# Title\n\nalpha content\n")]);
    env.run();

    // Simulate a crashed run: the pending marker and the tantivy commit made
    // it to disk, the final SQLite commit did not.
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
            line_end: 3,
            text: "beta content".to_owned(),
        }])
        .expect("add stale");
    env.index.commit().expect("commit stale");

    // The file was reverted before the rerun (e.g. `git checkout`).
    write_file(env.root.path(), "a.md", b"# Title\n\nalpha content\n");

    let stats = env.run();
    assert_eq!(stats.docs, 1, "marker forces reprocess");
    assert_eq!(stats.skipped, 0);
    assert!(
        env.search("beta").is_empty(),
        "stale committed chunks removed"
    );
    assert!(
        !env.search("alpha").is_empty(),
        "reverted content reindexed"
    );

    let hash: String = Connection::open(env.cache.path().join(DB_FILE))
        .expect("raw db")
        .query_row("SELECT content_hash FROM docs WHERE id = 1", [], |row| {
            row.get(0)
        })
        .expect("hash");
    assert!(!hash.is_empty(), "marker cleared after commit");
}

#[test]
fn crash_after_removal_purge_recovers_restored_file() {
    let mut env = Env::new(&[("a.md", "# Title\n\nalpha content\n")]);
    env.run();

    // Removal purge committed in tantivy, SQLite tombstone set, then the file
    // was restored byte-identical before the rerun.
    let conn = Connection::open(env.cache.path().join(DB_FILE)).expect("raw db");
    conn.execute("UPDATE docs SET content_hash = ''", [])
        .expect("tombstone");
    drop(conn);
    env.index.delete_doc(1);
    env.index.commit().expect("purge");

    let stats = env.run();
    assert_eq!(stats.docs, 1, "tombstone forces reindex");
    assert!(!env.search("alpha").is_empty(), "restored file searchable");
}

#[test]
fn doc_budget_frees_slots_in_same_run() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    let config = Config {
        max_docs_per_project: 1,
        ..Config::default()
    };
    env.run_with(&config);

    fs::remove_file(env.root.path().join("a.md")).expect("remove");
    write_file(
        env.root.path(),
        "c.md",
        b"# Fresh\n\nCompletely different subject.\n",
    );
    let stats = env.run_with(&config);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.docs, 1, "freed slot reused in the same run");
    assert_eq!(stats.errors, 0);
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
}

#[test]
fn skip_identical() {
    let mut env = Env::new(&[("a/guide.md", GUIDE), ("b/plain.md", PLAIN)]);
    env.run();
    let stats = env.run();

    assert_eq!(
        stats,
        JobStats {
            docs: 0,
            chunks: 0,
            skipped: 2,
            removed: 0,
            errors: 0,
            warnings: Vec::new(),
        }
    );
    assert!(!env.search("installer").is_empty(), "index intact");
}

#[test]
fn remove_deleted() {
    let mut env = Env::new(&[("a/guide.md", GUIDE), ("b/plain.md", PLAIN)]);
    env.run();
    fs::remove_file(env.root.path().join("b/plain.md")).expect("remove");

    let stats = env.run();
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.docs, 0);
    assert!(env.search("widgets").is_empty(), "purged from tantivy");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
    let stale = env.count("SELECT COUNT(*) FROM chunks WHERE doc_id = 2");
    assert_eq!(stale, 0, "chunks cascade-deleted");
}

#[test]
fn corrupt_file_nonfatal() {
    let mut env = Env::new(&[("good.md", PLAIN)]);
    write_file(env.root.path(), "bad.md", &[0xff, 0xfe, 0x00, 0x80]);

    let stats = env.run();
    assert_eq!(stats.errors, 1, "corrupt file counted");
    assert_eq!(stats.docs, 1);
    assert!(!env.search("widgets").is_empty(), "good file still indexed");
}

#[test]
fn changed_file_reindexes() {
    let mut env = Env::new(&[("a/guide.md", GUIDE), ("b/plain.md", PLAIN)]);
    let first = env.run();
    write_file(
        env.root.path(),
        "a/guide.md",
        b"# Install\n\nBrand new verbiage here.\n",
    );

    let stats = env.run();
    assert_eq!(stats.docs, 1);
    assert_eq!(stats.skipped, 1);
    assert!(env.search("installer").is_empty(), "old chunks gone");
    assert!(!env.search("verbiage").is_empty(), "new chunks searchable");
    let chunks = env.count("SELECT COUNT(*) FROM chunks WHERE doc_id = 1");
    assert!(
        i64::try_from(first.chunks).expect("fit") > chunks,
        "no stale rows: {first:?} vs {chunks}"
    );
}

#[test]
fn oversized_file_nonfatal() {
    let mut env = Env::new(&[("big.md", PLAIN)]);
    let config = Config {
        max_file_size: 8,
        ..Config::default()
    };

    let stats = env.run_with(&config);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.docs, 0);
    assert!(
        env.search("widgets").is_empty(),
        "oversized file not indexed"
    );
}

#[test]
fn doc_budget_warns() {
    let mut env = Env::new(&[("a.md", PLAIN), ("b.md", PLAIN)]);
    let config = Config {
        max_docs_per_project: 1,
        ..Config::default()
    };

    let stats = env.run_with(&config);
    assert_eq!(stats.docs, 1);
    assert_eq!(stats.errors, 1, "over-budget file warned");
    assert_eq!(env.count("SELECT COUNT(*) FROM docs"), 1);
}
