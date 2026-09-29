//! Concurrency soak (SC-5, NFR-5): three agents and a watcher against one
//! daemon, checking for corruption, lost updates, fd/thread growth and
//! unresolved lease churn.
//!
//! Ignored by default: nightly runs it for an hour (`SOAK_SECS=3600`), a
//! short local run uses e.g. `SOAK_SECS=10 cargo test --test soak -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::{daemon_pid, stop_daemon};
use docsbase_memory::ipc::client::Client;
use docsbase_memory::store::DB_FILE;

const DEFAULT_SOAK_SECS: u64 = 3_600;
const AGENTS: usize = 3;
const FRESHNESS_BUDGET: Duration = Duration::from_secs(5);
/// Files each agent rewrites in rotation; a bounded corpus keeps a full
/// index well inside the freshness budget for the whole hour (NFR-1 allows
/// 30 s per 1000 files, so unbounded growth would starve the watcher).
const NOTE_SLOTS: usize = 32;

struct Env {
    cache: TempDir,
    root: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new() -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        write_file(root.path(), "watch.md", b"# Watch\n\ninitial soakwidget.\n");
        Self {
            cache,
            root,
            daemon: None,
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command
            .env("DOCSBASE_CACHE_DIR", self.cache())
            .env("DOCSBASE_CONFIG_DIR", self.cache().join("config"))
            .current_dir(self.root());
        command
    }

    fn start_daemon(&mut self) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || self
                .cache()
                .join("state/daemon.sock")
                .exists()),
            "socket must appear"
        );
    }

    fn index(&self) {
        let output = self.cmd().arg("index").output().expect("run index");
        assert!(output.status.success(), "index failed: {output:?}");
    }

    fn client(&self) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect(self.cache()) {
                return client;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not connectable");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = stop_daemon(self.cache());
        if let Some(child) = self.daemon.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn is_lease_conflict(err: &docsbase_memory::error::Error) -> bool {
    matches!(
        err,
        docsbase_memory::error::Error::Project { message, .. }
            if message.starts_with("another writer holds")
    )
}

/// Indexes with retry: full jobs fail fast while a watcher batch holds the
/// project lease.
fn index_with_retry(client: &mut Client, root: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.call_tool(
            "index_project",
            json!({ "path": root.to_str().expect("utf8") }),
        ) {
            Ok(job) => {
                assert!(
                    matches!(job["state"].as_str(), Some("done" | "running" | "queued")),
                    "unexpected job state: {job}"
                );
                return;
            }
            Err(err) if is_lease_conflict(&err) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => panic!("index_project failed: {err}"),
        }
    }
}

/// Search results, or `None` while another agent's full index flips the
/// project to `indexing` (read commands refuse transiently by design).
fn search_paths(client: &mut Client, query: &str) -> Option<Vec<String>> {
    match client.call_tool("search_docs", json!({ "query": query })) {
        Ok(hits) => Some(
            hits.as_array()
                .expect("hits")
                .iter()
                .filter_map(|hit| hit["path"].as_str().map(str::to_owned))
                .collect(),
        ),
        Err(docsbase_memory::error::Error::Project { message, .. })
            if message.contains("(status:") =>
        {
            None
        }
        Err(err) => panic!("search_docs failed: {err}"),
    }
}

fn wait_for_marker(client: &mut Client, marker: &str, path: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if search_paths(client, marker).is_some_and(|hits| hits.iter().any(|hit| hit == path)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn run_agent(agent: usize, cache: &Path, root: &Path, deadline: Instant) -> usize {
    let mut client = Client::connect(cache).expect("connect");
    client.handshake(root).expect("bind agent session");
    let mut written = 0_usize;
    while Instant::now() < deadline {
        let slot = written % NOTE_SLOTS;
        let rel = format!("agent{agent}/note-{slot}.md");
        let marker = format!("soakmarker{agent}x{written}");
        write_file(
            root,
            &rel,
            format!("# Note {slot} revision {written}\n\n{marker} soakwidget prose.\n").as_bytes(),
        );
        index_with_retry(&mut client, root);
        assert!(
            wait_for_marker(&mut client, &marker, &rel, Duration::from_secs(20)),
            "agent {agent}: {rel} never became searchable"
        );
        written += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    written
}

fn run_soak(soak: Duration) {
    let mut env = Env::new();
    env.start_daemon();
    env.index();

    let baseline = {
        let mut client = env.client();
        client.handshake_registry().expect("hello");
        let status = client.call_tool("status", json!({})).expect("status");
        (
            status["fd_count"].as_u64().expect("fds"),
            status["threads"].as_u64().expect("threads"),
        )
    };

    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let deadline = Instant::now() + soak;

    let agents: Vec<_> = (0..AGENTS)
        .map(|agent| {
            let cache = env.cache().to_path_buf();
            let root = env.root().to_path_buf();
            std::thread::spawn(move || run_agent(agent, &cache, &root, deadline))
        })
        .collect();

    // Watcher freshness: keep editing one file and require it searchable
    // within the NFR-4 budget.
    let mut watcher_client = env.client();
    watcher_client.handshake(env.root()).expect("bind watcher");
    let mut revision = 0_usize;
    while Instant::now() < deadline {
        revision += 1;
        let marker = format!("watchmark{revision}");
        write_file(
            env.root(),
            "watch.md",
            format!("# Watch\n\n{marker} soakwidget.\n").as_bytes(),
        );
        if !wait_for_marker(&mut watcher_client, &marker, "watch.md", FRESHNESS_BUDGET) {
            failures
                .lock()
                .expect("lock")
                .push(format!("watcher revision {revision} missed the 5s budget"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let written: Vec<usize> = agents
        .into_iter()
        .map(|handle| handle.join().expect("agent join"))
        .collect();
    assert!(
        written.iter().all(|count| *count > 0),
        "every agent must do work: {written:?}"
    );
    assert!(
        failures.lock().expect("lock").is_empty(),
        "watcher failures: {:?}",
        failures.lock().expect("lock")
    );
    drop(watcher_client);

    final_consistency(&env, &written);
    watcher_only_freshness(&env);
    settle_growth(&env, baseline);
    verify_restart(&mut env, &written);
}

/// After all writers stop: every markdown file is registered and the last
/// markers of every agent are searchable.
fn final_consistency(env: &Env, written: &[usize]) {
    let mut client = env.client();
    client.handshake(env.root()).expect("final bind");
    index_with_retry(&mut client, env.root());
    let mut indexed = 0_usize;
    let mut cursor: Option<String> = None;
    loop {
        let args = match &cursor {
            Some(cursor) => json!({ "limit": 500, "cursor": cursor }),
            None => json!({ "limit": 500 }),
        };
        let listed = client.call_tool("list_docs", args).expect("list");
        indexed += listed["docs"].as_array().expect("docs").len();
        cursor = listed["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        indexed,
        count_markdown(env.root()),
        "docs rows must match markdown files"
    );
    for (agent, count) in written.iter().enumerate() {
        let revision = count - 1;
        assert!(
            wait_for_marker(
                &mut client,
                &format!("soakmarker{agent}x{revision}"),
                &format!("agent{agent}/note-{}.md", revision % NOTE_SLOTS),
                Duration::from_secs(20)
            ),
            "agent {agent}: final marker missing"
        );
    }
}

/// Watcher-only freshness once every agent stopped: an edit must become
/// searchable without any full index (NFR-4).
fn watcher_only_freshness(env: &Env) {
    let mut client = env.client();
    client.handshake(env.root()).expect("bind watcher check");
    let marker = "watcheronlymarker";
    write_file(
        env.root(),
        "watch.md",
        format!("# Watch\n\n{marker} soakwidget.\n").as_bytes(),
    );
    assert!(
        wait_for_marker(&mut client, marker, "watch.md", Duration::from_secs(10)),
        "watcher must index an edit without any full index"
    );
}

/// Session cleanup is asynchronous (NFR-9: reaped within ~2 s), so let the
/// daemon settle after every client is gone before checking for growth.
fn settle_growth(env: &Env, baseline: (u64, u64)) {
    let mut admin = env.client();
    admin.handshake_registry().expect("hello");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let status = admin.call_tool("status", json!({})).expect("status");
        let fds = status["fd_count"].as_u64().expect("fds");
        let threads = status["threads"].as_u64().expect("threads");
        if fds <= baseline.0 + 8 && threads <= baseline.1 + 16 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = admin.call_tool("status", json!({})).expect("status");
    panic!(
        "daemon did not settle: baseline {baseline:?}, final {} fds / {} threads",
        status["fd_count"], status["threads"]
    );
}

/// Crash-safety: integrity check on the stopped registry, then a restart that
/// still answers from the same index.
fn verify_restart(env: &mut Env, written: &[usize]) {
    stop_daemon(env.cache()).expect("stop daemon");
    assert!(
        !env.cache().join("logs/conflicts.ndjson").exists(),
        "same-build soak must not record admission conflicts (SC-5)"
    );
    let conn = rusqlite::Connection::open(env.cache().join(DB_FILE)).expect("registry db");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity");
    assert_eq!(integrity, "ok", "registry must not be corrupted");
    drop(conn);

    env.start_daemon();
    let mut restarted = env.client();
    restarted.handshake(env.root()).expect("bind after restart");
    let agent = AGENTS - 1;
    let revision = written[agent] - 1;
    assert!(
        wait_for_marker(
            &mut restarted,
            &format!("soakmarker{agent}x{revision}"),
            &format!("agent{agent}/note-{}.md", revision % NOTE_SLOTS),
            Duration::from_secs(20)
        ),
        "data must survive a daemon restart"
    );
    let pid = daemon_pid(env.cache()).expect("pid");
    assert!(pid > 0, "restarted daemon has a pid");
}

fn count_markdown(root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                count += 1;
            }
        }
    }
    count
}

#[test]
#[ignore = "soak runs nightly (SOAK_SECS=3600) or locally with SOAK_SECS=<seconds>"]
fn three_agents_one_daemon_no_corruption() {
    let soak_secs = std::env::var("SOAK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SOAK_SECS);
    run_soak(Duration::from_secs(soak_secs));
}
