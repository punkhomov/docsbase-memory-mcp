#![cfg(target_os = "linux")]
//! NFR-1 performance budgets in release mode: search p95 over 50k chunks and
//! a full index of 1000 markdown files (SC-7).
//!
//! Debug builds skip these tests (`--release` required): the budgets are
//! defined for optimized builds only.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::lifecycle::{daemon_pid, ensure_daemon_with, stop_daemon};
use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::job::run_full;
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::store::migrations;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use rusqlite::Connection;
use tempfile::TempDir;

const SEARCH_BUDGET: Duration = Duration::from_millis(200);
/// NFR-2: idle daemon RSS with one open project.
const RSS_BUDGET_KB: u64 = 150 * 1024;
const INDEX_BUDGET: Duration = Duration::from_secs(30);
/// B1: unchanged-corpus sync is a hash walk, not a re-index (T52).
const SYNC_NOOP_BUDGET: Duration = Duration::from_secs(3);
/// B2: 5% of files changed must re-index exactly those files (T52).
const SYNC_INCREMENTAL_BUDGET: Duration = Duration::from_secs(5);
/// B4: no-op sync over a 10k-doc project (T52; deferred tail from Task 32).
const SYNC_NOOP_10K_BUDGET: Duration = Duration::from_secs(15);
/// B4: RSS delta of the indexing process for a 10k-doc project (T52;
/// local release measurement: ~32 MiB).
const RSS_10K_BUDGET_KB: u64 = 128 * 1024;
const CHUNKS: usize = 50_000;
const FILES: usize = 1_000;
const FILES_10K: usize = 10_000;
const SAMPLES: usize = 60;

fn chunk(doc_id: i64, seq: u32, text: String) -> Chunk {
    Chunk {
        doc_id,
        seq,
        heading_path: vec![format!("Section {seq}")],
        kind: ChunkKind::Prose,
        line_start: 1,
        line_end: 10,
        text,
    }
}

fn synthetic_text(index: usize) -> String {
    let mut text = format!(
        "widget{index} search performance benchmark documentation chunk. \
         The identifier doc_{} tracks indexing throughput and latency budgets. ",
        index % 977
    );
    for word in [
        "alpha", "beta", "gamma", "delta", "timing", "query", "corpus",
    ] {
        text.push_str(word);
        text.push(' ');
    }
    text
}

fn build_index(dir: &Path) -> IndexHandle {
    let mut index = IndexHandle::open_or_create(dir).expect("index");
    index.mark_rebuilt().expect("mark rebuilt");
    let mut batch = Vec::with_capacity(5_000);
    for i in 0..CHUNKS {
        batch.push(chunk(
            i64::try_from(i).unwrap_or(i64::MAX),
            u32::try_from(i % 64).unwrap_or(0),
            synthetic_text(i),
        ));
        if batch.len() == 5_000 {
            index.add_chunks(&batch).expect("add");
            index.commit().expect("commit");
            batch.clear();
        }
    }
    if !batch.is_empty() {
        index.add_chunks(&batch).expect("add");
        index.commit().expect("commit");
    }
    index
}

fn percentile(samples: &mut [Duration], percent: usize) -> Duration {
    samples.sort();
    let last = samples.len().saturating_sub(1);
    let index = samples
        .len()
        .saturating_mul(percent)
        .div_ceil(100)
        .saturating_sub(1)
        .min(last);
    samples[index]
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budget requires --release")]
fn search_budget() {
    let dir = TempDir::new().expect("tempdir");
    let index = build_index(dir.path());
    let queries = [
        "search performance widget42",
        "doc_123 indexing throughput",
        "latency budgets alpha gamma",
        "widget49999 corpus timing",
    ];

    // Warm up the searcher and postings cache before sampling.
    for _ in 0..5 {
        let hits = index.search(queries[0], 10).expect("warmup");
        assert!(!hits.is_empty(), "corpus must be searchable");
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let query = queries[i % queries.len()];
        let started = Instant::now();
        let hits = index.search(query, 10).expect("search");
        samples.push(started.elapsed());
        assert!(!hits.is_empty(), "query {query:?} must match");
    }
    let p95 = percentile(&mut samples, 95);
    eprintln!("search p95 over {CHUNKS} chunks: {p95:?}");
    assert!(
        p95 <= SEARCH_BUDGET,
        "search p95 {p95:?} over {CHUNKS} chunks exceeds {SEARCH_BUDGET:?}"
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budget requires --release")]
fn index_budget() {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for i in 0..FILES {
        let rel = format!("docs/section-{:02}/file-{i}.md", i % 20);
        let path = root.path().join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        let mut body = format!("# Document {i}\n\nIntro prose about widget{i} and indexing.\n");
        for section in 0..8 {
            let _ = write!(
                body,
                "\n## Section {section}\n\nProse with latch{i}-{section} and \
                 performance notes for the search budget.\n\n```rust\nlet v = {i};\n```\n"
            );
        }
        fs::write(path, body).expect("write");
    }

    let mut db = Db::open(cache.path()).expect("db");
    let canonical_root = root.path().canonicalize().expect("canonical");
    let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
    conn.execute(
        "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
         VALUES (1, ?1, 'perf', 'not_indexed', ?2, 0)",
        rusqlite::params![canonical_root.to_string_lossy(), migrations::SCHEMA_VERSION],
    )
    .expect("insert project");
    drop(conn);

    let project = Project {
        id: 1,
        canonical_root,
        name: "perf".to_owned(),
        status: ProjectStatus::NotIndexed,
        schema_version: migrations::SCHEMA_VERSION,
        created_at: 0,
        last_indexed_at: None,
    };
    let mut index =
        IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).expect("index");

    let started = Instant::now();
    let stats = run_full(&mut db, &mut index, &project, &Config::default()).expect("run_full");
    let elapsed = started.elapsed();
    eprintln!("full index of {FILES} files: {elapsed:?}");

    assert_eq!(stats.docs, FILES, "every file indexed: {stats:?}");
    assert!(stats.warnings.is_empty(), "no file warnings: {stats:?}");
    assert!(
        elapsed <= INDEX_BUDGET,
        "indexing {FILES} files took {elapsed:?}, over {INDEX_BUDGET:?}"
    );
}

fn write_sync_corpus(root: &Path, files: usize) {
    for i in 0..files {
        let rel = format!("docs/dir{:02}/doc{i}.md", i % 20);
        write_file(
            root,
            &rel,
            format!("# Doc {i}\n\nprose widget{i} sync budget body.\n").as_bytes(),
        );
    }
}

fn sync_project(cache: &Path, root: &Path) -> (Db, Project, IndexHandle) {
    let mut db = Db::open(cache).expect("db");
    let canonical_root = root.canonicalize().expect("canonical");
    let conn = Connection::open(cache.join(DB_FILE)).expect("raw db");
    conn.execute(
        "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
         VALUES (1, ?1, 'perf', 'not_indexed', ?2, 0)",
        rusqlite::params![canonical_root.to_string_lossy(), migrations::SCHEMA_VERSION],
    )
    .expect("insert project");
    drop(conn);
    let project = Project {
        id: 1,
        canonical_root,
        name: "perf".to_owned(),
        status: ProjectStatus::NotIndexed,
        schema_version: migrations::SCHEMA_VERSION,
        created_at: 0,
        last_indexed_at: None,
    };
    let index = IndexHandle::open_or_create(&cache.join("projects/1/tantivy")).expect("index");
    (db, project, index)
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budget requires --release")]
fn sync_noop_and_incremental_budgets() {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    write_sync_corpus(root.path(), FILES);
    let (mut db, project, mut index) = sync_project(cache.path(), root.path());

    let full = run_full(&mut db, &mut index, &project, &Config::default()).expect("full index");
    assert_eq!(full.docs, FILES, "full index must cover the corpus");

    let started = Instant::now();
    let stats = run_full(&mut db, &mut index, &project, &Config::default()).expect("no-op sync");
    let noop = started.elapsed();
    eprintln!("sync no-op {FILES} files: {noop:?}");
    assert_eq!(stats.docs, 0, "no-op must re-index nothing");
    assert_eq!(stats.skipped, FILES, "no-op must skip everything");
    assert!(
        noop <= SYNC_NOOP_BUDGET,
        "no-op sync took {noop:?}, over {SYNC_NOOP_BUDGET:?}"
    );

    let changed = FILES / 20;
    for i in (0..FILES).step_by(20) {
        let rel = format!("docs/dir{:02}/doc{i}.md", i % 20);
        write_file(
            root.path(),
            &rel,
            format!("# Doc {i} v2\n\nchanged widget{i} body.\n").as_bytes(),
        );
    }

    let started = Instant::now();
    let stats =
        run_full(&mut db, &mut index, &project, &Config::default()).expect("incremental sync");
    let incremental = started.elapsed();
    eprintln!("sync changed {changed} of {FILES}: {incremental:?}");
    assert_eq!(stats.docs, changed, "only changed files may be re-indexed");
    assert_eq!(stats.skipped, FILES - changed, "the rest must be skipped");
    assert!(
        incremental <= SYNC_INCREMENTAL_BUDGET,
        "incremental sync took {incremental:?}, over {SYNC_INCREMENTAL_BUDGET:?}"
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budget requires --release")]
fn sync_noop_10k_budget_and_rss() {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    write_sync_corpus(root.path(), FILES_10K);
    let (mut db, project, mut index) = sync_project(cache.path(), root.path());
    let rss_before = read_vm_rss_kb(std::process::id());

    let started = Instant::now();
    let full = run_full(&mut db, &mut index, &project, &Config::default()).expect("full 10k index");
    eprintln!("full index {FILES_10K} files: {:?}", started.elapsed());
    assert_eq!(full.docs, FILES_10K);

    let started = Instant::now();
    let stats =
        run_full(&mut db, &mut index, &project, &Config::default()).expect("no-op 10k sync");
    let noop = started.elapsed();
    let rss_delta = read_vm_rss_kb(std::process::id()).saturating_sub(rss_before);
    eprintln!("sync no-op {FILES_10K} files: {noop:?}, rss delta {rss_delta} KiB");
    assert_eq!(stats.skipped, FILES_10K);
    assert!(
        noop <= SYNC_NOOP_10K_BUDGET,
        "10k no-op sync took {noop:?}, over {SYNC_NOOP_10K_BUDGET:?}"
    );
    assert!(
        rss_delta <= RSS_10K_BUDGET_KB,
        "10k project rss delta {rss_delta} KiB over budget {RSS_10K_BUDGET_KB} KiB"
    );
}

fn daemon_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin!("docsbase").to_path_buf()
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn read_vm_rss_kb(pid: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("process status");
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("VmRSS in /proc status")
}

#[cfg(unix)]
#[test]
#[cfg_attr(debug_assertions, ignore = "perf budget requires --release")]
fn rss_budget() {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    for i in 0..50 {
        write_file(
            root.path(),
            &format!("docs/file-{i}.md"),
            format!("# Doc {i}\n\nwidget prose {i} for the RSS budget.\n").as_bytes(),
        );
    }
    ensure_daemon_with(cache.path(), Some(&daemon_bin())).expect("start daemon");

    let output = Command::new(daemon_bin())
        .arg("index")
        .env("DOCSBASE_CACHE_DIR", cache.path())
        .env("DOCSBASE_CONFIG_DIR", cache.path().join("config"))
        .current_dir(root.path())
        .stdin(Stdio::null())
        .output()
        .expect("run index");
    assert!(output.status.success(), "index failed: {output:?}");

    // Bind a session so the daemon holds the project open, then measure.
    let mut client = Client::connect(cache.path()).expect("connect");
    client.handshake(root.path()).expect("bind session");
    let pid = daemon_pid(cache.path()).expect("daemon pid");
    let rss_kb = read_vm_rss_kb(pid);
    eprintln!("daemon idle RSS with one project: {rss_kb} kB");
    stop_daemon(cache.path()).expect("stop daemon");

    assert!(
        rss_kb <= RSS_BUDGET_KB,
        "idle RSS {rss_kb} kB exceeds the {RSS_BUDGET_KB} kB budget (NFR-2)"
    );
}
