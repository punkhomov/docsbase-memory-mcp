use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use docsbase_memory::config::Config;
use docsbase_memory::index::job::{JobStats, run_full, run_incremental_with};
use docsbase_memory::index::tantivy_index::{Hit, IndexHandle};
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use docsbase_memory::watch::{WatcherGuard, spawn_watcher};
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

    fn paths(&self) -> Vec<String> {
        let conn = Connection::open(self.cache.path().join(DB_FILE)).expect("raw db");
        let mut stmt = conn
            .prepare("SELECT rel_path FROM docs ORDER BY rel_path")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query");
        rows.map(|row| row.expect("row")).collect()
    }

    fn watch(&self) -> (WatcherGuard, Receiver<Vec<PathBuf>>) {
        self.watch_with(self.cache.path())
    }

    fn watch_with(&self, cache: &Path) -> (WatcherGuard, Receiver<Vec<PathBuf>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let guard = spawn_watcher(&self.project, cache, &self.config, tx).expect("spawn watcher");
        (guard, rx)
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn recv_batch(rx: &Receiver<Vec<PathBuf>>, timeout: Duration) -> Vec<PathBuf> {
    rx.recv_timeout(timeout).expect("watcher batch")
}

fn recv_none(rx: &Receiver<Vec<PathBuf>>, timeout: Duration) {
    match rx.recv_timeout(timeout) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("unexpected batch: {other:?}"),
    }
}

const PLAIN: &str = "# Plain\n\nSome prose about widgets and gadgets.\n";

#[test]
fn edit_becomes_searchable_within_2s() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    env.full();
    let (_guard, rx) = env.watch();

    let start = Instant::now();
    write_file(
        env.root.path(),
        "a.md",
        b"# Plain\n\nSome prose about gizmos and gadgets.\n",
    );
    let batch = recv_batch(&rx, Duration::from_secs(5));
    env.incremental(&batch);

    let hits = env.search("gizmos");
    assert!(!hits.is_empty(), "edit must be searchable");
    assert!(
        start.elapsed() < Duration::from_millis(2_000),
        "freshness took {:?}",
        start.elapsed()
    );
}

#[test]
fn burst_one_job() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    env.full();
    let (_guard, rx) = env.watch();

    let mut expected = Vec::new();
    for index in 0..20 {
        let rel = format!("docs/burst-{index}.md");
        write_file(
            env.root.path(),
            &rel,
            format!("# Burst {index}\n\nburst prose number {index}.\n").as_bytes(),
        );
        expected.push(env.path(&rel));
    }
    let batch = recv_batch(&rx, Duration::from_secs(5));
    assert_eq!(
        batch.len(),
        expected.len(),
        "one coalesced batch: {batch:?}"
    );
    for path in &expected {
        assert!(batch.contains(path), "missing {path:?} in {batch:?}");
    }
    recv_none(&rx, Duration::from_millis(500));

    let stats = env.incremental(&batch);
    assert_eq!(stats.docs, 20, "single job covers the burst");
    assert_eq!(stats.errors, 0);
}

#[test]
fn delete_purges() {
    let mut env = Env::new(&[("a.md", PLAIN), ("b.md", "# Beta\n\nbeta content.\n")]);
    env.full();
    let (_guard, rx) = env.watch();

    let gone = env.path("b.md");
    fs::remove_file(&gone).expect("remove");
    let batch = recv_batch(&rx, Duration::from_secs(5));
    assert!(batch.contains(&gone), "batch: {batch:?}");
    let stats = env.incremental(&batch);
    assert_eq!(stats.removed, 1);
    assert!(env.search("beta").is_empty(), "purged from tantivy");
    assert_eq!(env.paths(), vec!["a.md".to_owned()]);
}

#[test]
fn rename_delete_plus_add() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    env.full();
    let (_guard, rx) = env.watch();

    fs::rename(env.path("a.md"), env.path("renamed.md")).expect("rename");
    let batch = recv_batch(&rx, Duration::from_secs(5));
    env.incremental(&batch);

    assert_eq!(env.paths(), vec!["renamed.md".to_owned()]);
    assert!(!env.search("widgets").is_empty(), "content survives rename");
}

#[test]
fn index_dir_ignored() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    env.full();
    let cache = env.root.path().join("cache-inside-root");
    let (_guard, rx) = env.watch_with(&cache);

    write_file(
        &cache,
        "projects/1/tantivy/evil.md",
        b"# Evil\n\nindex noise must not index.\n",
    );
    write_file(
        env.root.path(),
        "target/built.md",
        b"# Built\n\nbuild output must not index.\n",
    );
    write_file(
        env.root.path(),
        ".hidden/secret.md",
        b"# Hidden\n\nhidden files must not index.\n",
    );
    write_file(
        env.root.path(),
        "node_modules/pkg/readme.md",
        b"# Pkg\n\nvendored files must not index.\n",
    );
    write_file(
        env.root.path(),
        "a.md",
        b"# Plain\n\nSome prose about gadgets and doodads.\n",
    );

    let batch = recv_batch(&rx, Duration::from_secs(5));
    assert_eq!(
        batch.iter().filter(|path| path.ends_with("a.md")).count(),
        1,
        "real edit missing: {batch:?}"
    );
    assert_eq!(batch.len(), 1, "only the editable file may pass: {batch:?}");
    recv_none(&rx, Duration::from_millis(500));
}

#[test]
fn dir_rename_purges_subtree() {
    let mut env = Env::new(&[
        ("dir/a.md", PLAIN),
        ("dir/b.md", "# Beta\n\nbeta content.\n"),
    ]);
    env.full();
    let (_guard, rx) = env.watch();

    fs::rename(env.path("dir"), env.path("renamed")).expect("rename dir");
    let batch = recv_batch(&rx, Duration::from_secs(5));
    env.incremental(&batch);

    assert_eq!(
        env.paths(),
        vec!["renamed/a.md".to_owned(), "renamed/b.md".to_owned()]
    );
    assert!(!env.search("widgets").is_empty(), "content survives rename");
    assert_ne!(env.search("beta").len(), 0);
}

#[test]
fn symlinks_not_followed() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    let external = TempDir::new().expect("external");
    write_file(
        external.path(),
        "secret.md",
        b"# Secret\n\nsecretsauce must stay hidden.\n",
    );
    env.full();
    let (_guard, rx) = env.watch();

    // Symlinks appear after the watch started, so their events are real.
    std::os::unix::fs::symlink(external.path().join("secret.md"), env.path("link.md"))
        .expect("symlink file");
    std::os::unix::fs::symlink(external.path(), env.path("linkdir")).expect("symlink dir");
    write_file(
        env.root.path(),
        "a.md",
        b"# Plain\n\nSome prose about gadgets and doodads.\n",
    );

    let batch = recv_batch(&rx, Duration::from_secs(5));
    assert!(
        batch
            .iter()
            .all(|path| !path.to_string_lossy().contains("link")),
        "symlink paths leaked: {batch:?}"
    );
    assert!(
        batch.iter().any(|path| path.ends_with("a.md")),
        "real edit missing: {batch:?}"
    );
    recv_none(&rx, Duration::from_millis(500));

    write_file(
        env.root.path(),
        "link.md",
        b"# Secret\n\nsecretsauce changed through link.\n",
    );
    recv_none(&rx, Duration::from_millis(2_600));
    assert_eq!(
        env.search("secretsauce").len(),
        0,
        "watcher must not index through symlinks"
    );
}

#[test]
fn new_files_in_watched_dir_after_first_window() {
    let mut env = Env::new(&[("a.md", PLAIN)]);
    env.full();
    let (_guard, rx) = env.watch();

    write_file(env.root.path(), "b.md", b"# B\n\nbcontent first.\n");
    let first = recv_batch(&rx, Duration::from_secs(5));
    env.incremental(&first);
    assert_ne!(env.search("bcontent").len(), 0);

    std::thread::sleep(Duration::from_millis(300));
    write_file(env.root.path(), "c.md", b"# C\n\nccontent second.\n");
    let second = recv_batch(&rx, Duration::from_secs(5));
    env.incremental(&second);
    assert_ne!(
        env.search("ccontent").len(),
        0,
        "files created in an already seen directory must be admitted"
    );
}
