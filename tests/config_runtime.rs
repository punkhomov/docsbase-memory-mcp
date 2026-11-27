#[path = "support/search_visibility.rs"]
mod search_visibility;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::error::Error;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{FRAME_OVERHEAD, MAX_FRAME_BYTES};
use docsbase_memory::platform;

#[path = "support/transport.rs"]
mod test_transport;

struct Env {
    transport: test_transport::TestTransport,
    cache: TempDir,
    root: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        fs::create_dir_all(cache.path().join("config")).expect("config dir");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        Self {
            transport: test_transport::TestTransport::new(160),
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

    fn global_config(&self) -> PathBuf {
        self.cache().join("config/config.toml")
    }

    fn project_config(&self) -> PathBuf {
        self.root().join(".docsbase.toml")
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command
            .env("DOCSBASE_CACHE_DIR", self.cache())
            .env("DOCSBASE_CONFIG_DIR", self.cache().join("config"))
            .env("DOCSBASE_DATA_DIR", self.cache().join("data"))
            .current_dir(self.root());
        command
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let out = self.cmd().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn start_daemon(&mut self) {
        self.start_daemon_with(self.cmd());
    }

    fn start_daemon_with(&mut self, mut command: Command) {
        let child = command
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || platform::exists(
                &platform::daemon_endpoint(self.cache())
            )),
            "socket must appear"
        );
    }

    fn restart_daemon(&mut self) {
        stop_daemon(self.cache()).expect("stop daemon");
        self.start_daemon();
    }

    fn client(&self) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect(&self.transport, self.cache()) {
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

fn doc(body: &str) -> String {
    let mut text = format!("# Title\n\n{body}\n");
    for section in 0..3 {
        let _ = writeln!(text, "\n## Section {section}\n\n{body} details.");
    }
    text
}

fn index(client: &mut Client, env: &Env) -> Value {
    client
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect("index_project")
}

fn search(client: &mut Client, query: &str) -> Vec<String> {
    search_before(client, query, Instant::now() + watcher_deadline())
}

fn search_before(client: &mut Client, query: &str, deadline: Instant) -> Vec<String> {
    let hits = search_response_before(
        deadline,
        || client.call_tool("search_docs", serde_json::json!({ "query": query })),
        Instant::now,
        || std::thread::sleep(Duration::from_millis(20)),
    );
    let mut paths: Vec<String> = hits
        .as_array()
        .expect("hits array")
        .iter()
        .map(|hit| hit["path"].as_str().expect("path").to_owned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn search_response_before(
    deadline: Instant,
    mut attempt: impl FnMut() -> Result<Value, Error>,
    mut now: impl FnMut() -> Instant,
    mut pause: impl FnMut(),
) -> Value {
    // Watcher/index publication may temporarily refuse an incoherent project.
    // Require an actual success: an error must never stand in for empty hits.
    // Every retry and the successful response share the caller's deadline.
    let mut last_project_error = None;
    while now() < deadline {
        match attempt() {
            Ok(hits) => {
                assert!(
                    now() < deadline,
                    "search_docs completed after watcher deadline: {last_project_error:?}"
                );
                return hits;
            }
            Err(error) if search_visibility::temporarily_unavailable(&error) => {
                last_project_error = Some(error);
            }
            Err(error) => panic!("search_docs: {error:?}"),
        }
        pause();
    }
    panic!("project did not become searchable before watcher deadline: {last_project_error:?}");
}

#[test]
fn direct_index_obeys_configured_sqlite_busy_timeout() {
    let env = Env::new(&[(
        "readme.md",
        "# Guide

Storage contention fixture.
",
    )]);
    fs::write(
        env.global_config(),
        "config_version = 2
[store]
write_busy_timeout = 0
wal_attempts = 1
",
    )
    .unwrap();
    assert_eq!(env.run_json(&["index"])["project_id"], 1);
    write_file(
        env.root(),
        "readme.md",
        b"Changed storage contention fixture.",
    );

    let registry = env.cache().join(docsbase_memory::store::DB_FILE);
    let conn = rusqlite::Connection::open(registry).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut child = env
        .cmd()
        .args(["index"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let exited = wait_until(Duration::from_secs(12), || {
        child.try_wait().unwrap().is_some()
    });
    if !exited {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        exited,
        "the direct writer waited despite its zero busy timeout"
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("database is locked"), "{error}");

    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(env.run_json(&["index"])["project_id"], 1);
}

#[test]
fn search_readiness_rejects_success_at_or_after_deadline() {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    for elapsed in [Duration::from_secs(1), Duration::from_secs(2)] {
        let start = Instant::now();
        let clock = Cell::new(start);
        let result = catch_unwind(AssertUnwindSafe(|| {
            search_response_before(
                start + Duration::from_secs(1),
                || {
                    clock.set(start + elapsed);
                    Ok(serde_json::json!([{"path": "aaa/a.md"}]))
                },
                || clock.get(),
                || panic!("a successful response must not retry"),
            )
        }));
        assert!(result.is_err(), "late hits must not satisfy a watcher poll");
    }
}

#[test]
fn search_readiness_stops_at_original_deadline() {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let deadline = Instant::now() + watcher_deadline();
    let clock = Cell::new(
        deadline
            .checked_sub(Duration::from_secs(1))
            .expect("one second before deadline"),
    );
    let attempts = Cell::new(0);
    let result = catch_unwind(AssertUnwindSafe(|| {
        search_response_before(
            deadline,
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Err(Error::Project {
                        message: "project has an incomplete index update".into(),
                        instruction: None,
                    })
                } else {
                    Ok(serde_json::json!([{"path": "aaa/a.md"}]))
                }
            },
            || clock.get(),
            || clock.set(deadline),
        )
    }));
    let failure = result.expect_err("readiness must not start another watcher budget");
    assert_eq!(attempts.get(), 1, "no attempt after the original deadline");
    assert!(
        failure
            .downcast_ref::<String>()
            .expect("timeout diagnostic")
            .contains("project has an incomplete index update"),
        "timeout must retain the last refusal"
    );
}

#[test]
fn search_readiness_does_not_attempt_after_deadline() {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let deadline = Instant::now();
    let attempts = Cell::new(0);
    let result = catch_unwind(AssertUnwindSafe(|| {
        search_response_before(
            deadline,
            || {
                attempts.set(attempts.get() + 1);
                Ok(serde_json::json!([]))
            },
            || deadline,
            || panic!("an expired poll must not sleep"),
        )
    }));
    assert!(result.is_err(), "an expired deadline cannot return hits");
    assert_eq!(attempts.get(), 0);
}

#[test]
fn search_readiness_requires_successful_empty_response() {
    use std::cell::Cell;

    let start = Instant::now();
    let clock = Cell::new(start);
    let attempts = Cell::new(0);
    let hits = search_response_before(
        start + watcher_deadline(),
        || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(Error::Project {
                    message: "project has an incomplete index update".into(),
                    instruction: None,
                })
            } else {
                Ok(serde_json::json!([]))
            }
        },
        || clock.get(),
        || clock.set(start + Duration::from_secs(1)),
    );
    assert_eq!(attempts.get(), 2, "refusal cannot become an empty success");
    assert_eq!(hits, serde_json::json!([]));
}

#[test]
fn search_readiness_rejects_other_errors_immediately() {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let errors = [
        Error::Admission {
            message: "fixture admission".into(),
        },
        Error::Protocol {
            message: "fixture protocol".into(),
        },
        Error::Project {
            message: "fixture unknown project error".into(),
            instruction: None,
        },
        Error::internal("fixture internal"),
        Error::transport("fixture transport"),
    ];
    for error in errors {
        let expected = format!("search_docs: {error:?}");
        let mut error = Some(error);
        let attempts = Cell::new(0);
        let start = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            search_response_before(
                start + watcher_deadline(),
                || {
                    attempts.set(attempts.get() + 1);
                    Err(error.take().expect("one attempt"))
                },
                || start,
                || panic!("fatal errors must not retry"),
            )
        }));
        let failure = result.expect_err("other errors must fail immediately");
        assert_eq!(attempts.get(), 1);
        assert_eq!(failure.downcast_ref::<String>(), Some(&expected));
    }
}

#[test]
fn search_refuses_pending_payload_and_recovers_after_completion() {
    let mut env = Env::new(&[("ready.md", &doc("readywidget"))]);
    fs::write(env.global_config(), "# v4 pending-document fixture\n").unwrap();
    env.start_daemon();
    let mut registry_client = env.client();
    registry_client
        .handshake_registry()
        .expect("registry handshake");
    index(&mut registry_client, &env);
    let mut bound = env.client();
    bound.handshake(env.root()).expect("bound session");
    let db = docsbase_memory::store::Db::open(env.cache()).expect("control writer");
    let projects = docsbase_memory::daemon::registry::list_projects(&db).expect("projects");
    let pending = docsbase_memory::store::repo::insert_pending_doc(
        db.connection(),
        projects[0].id,
        "pending.md",
        "pending.md",
    )
    .expect("crash marker");
    let error = bound
        .call_tool("search_docs", serde_json::json!({"query":"readywidget"}))
        .expect_err("incomplete project cannot produce a mixed payload");
    assert!(matches!(error, Error::Project { .. }), "{error:?}");
    assert!(
        error.to_string().contains("incomplete index update"),
        "{error}"
    );
    assert!(search_visibility::temporarily_unavailable(&error));
    db.connection()
        .execute("DELETE FROM docs WHERE id=?1", [pending])
        .expect("complete marker");
    assert_eq!(search(&mut bound, "readywidget"), vec!["ready.md"]);
}

#[test]
fn ignores_from_project_config() {
    let mut env = Env::new(&[
        ("keep.md", &doc("publicwidget")),
        ("secret/hide.md", &doc("secretwidget")),
    ]);
    fs::write(env.project_config(), "ignores = [\"secret/**\"]\n").expect("project config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind");
    assert_eq!(search(&mut bound, "publicwidget"), vec!["keep.md"]);
    assert!(
        search(&mut bound, "secretwidget").is_empty(),
        "project ignore must exclude secret/**"
    );
}

#[test]
fn project_config_applied_on_open() {
    let mut env = Env::new(&[
        ("aaa/a.md", &doc("alphawidget")),
        ("bbb/b.md", &doc("betawidget")),
    ]);
    fs::write(env.global_config(), "# Legacy v1 config reload\n").unwrap();
    fs::write(env.project_config(), "ignores = [\"aaa/**\"]\n").expect("project config v1");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);

    let mut first = env.client();
    first.handshake(env.root()).expect("first open");
    assert!(
        search(&mut first, "alphawidget").is_empty(),
        "v1 ignores aaa"
    );

    // Project config changes are only picked up when the project is opened
    // again (OQ-6): the open session keeps the configuration it opened with.
    fs::write(env.project_config(), "ignores = [\"bbb/**\"]\n").expect("project config v2");
    write_file(env.root(), "aaa/a.md", doc("alphawidget2").as_bytes());
    write_file(env.root(), "bbb/b.md", doc("betawidget2").as_bytes());
    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);
    assert!(
        search(&mut first, "alphawidget2").is_empty(),
        "mid-session config edit must not apply"
    );
    assert_eq!(
        search(&mut first, "betawidget2"),
        vec!["bbb/b.md"],
        "changed but still-indexed file is refreshed"
    );

    let mut second = env.client();
    second.handshake(env.root()).expect("reopen");
    index(&mut second, &env);
    assert_eq!(search(&mut second, "alphawidget2"), vec!["aaa/a.md"]);
    assert!(
        search(&mut second, "betawidget2").is_empty(),
        "reopened project must use v2 ignores"
    );
}

#[test]
fn global_change_requires_restart_hint() {
    let mut env = Env::new(&[
        ("keep.md", &doc("publicwidget")),
        ("ggg/g.md", &doc("gammawidget")),
    ]);
    fs::write(env.global_config(), "ignores = [\"ggg/**\"]\n").expect("global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let before = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(before["restart_required"], false, "status: {before}");
    index(&mut registry, &env);

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind");
    assert!(
        search(&mut bound, "gammawidget").is_empty(),
        "global v1 ignores ggg"
    );

    fs::write(env.global_config(), "# changed\n").expect("global config v2");
    let status = bound
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice string");
    assert!(
        notice.contains("daemon stop"),
        "notice must point at `docsbase daemon stop`: {notice}"
    );

    // The running daemon keeps the configuration it started with: the project
    // still ignores ggg until it is restarted.
    assert!(
        search(&mut bound, "gammawidget").is_empty(),
        "running daemon must keep the startup global config"
    );
}

#[test]
fn broken_global_config_blocks_open_until_restart() {
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    fs::write(env.global_config(), "not = [valid\n").expect("break global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice");
    assert!(
        notice.contains("invalid") && notice.contains("daemon stop"),
        "notice: {notice}"
    );

    let err = registry
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect_err("broken global config must block indexing");
    assert!(
        err.to_string().contains("daemon stop"),
        "unexpected error: {err}"
    );

    fs::remove_file(env.global_config()).expect("remove broken config");
    let still = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(
        still["restart_required"], true,
        "a fixed file still needs a restart: {still}"
    );

    env.restart_daemon();
    let mut fresh = env.client();
    fresh.handshake_registry().expect("registry hello");
    index(&mut fresh, &env);
    let status = fresh
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], false, "status: {status}");
}

/// Watcher batches are coarse-grained on Windows (`ReadDirectoryChangesW`
/// coalesces and only reports after the buffer fills or a short idle gap) and
/// shared CI runners run these debug builds ~2-3x slower than a dev host, so
/// the same batch can take 4-5x longer there. The bound only gates the
/// failure path: a passing run returns as soon as the entry is indexed.
fn watcher_deadline() -> Duration {
    Duration::from_secs(if cfg!(windows) { 60 } else { 20 })
}

#[test]
fn watcher_uses_reopened_config() {
    let mut env = Env::new(&[
        ("aaa/a.md", &doc("alphawidget")),
        ("bbb/b.md", &doc("betawidget")),
    ]);
    fs::write(env.global_config(), "# Legacy v1 watcher reload\n").unwrap();
    fs::write(env.project_config(), "ignores = [\"aaa/**\"]\n").expect("project config v1");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);
    let mut first = env.client();
    first.handshake(env.root()).expect("first open");

    // Run one watcher batch under v1 so the filter caches bbb as indexable;
    // the v2 swap must invalidate that cache too.
    write_file(env.root(), "bbb/seed.md", doc("seedwidget").as_bytes());
    let deadline = Instant::now() + watcher_deadline();
    let mut found = false;
    while Instant::now() < deadline {
        if !search_before(&mut first, "seedwidget", deadline).is_empty()
            && Instant::now() < deadline
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(found, "v1 watcher must index bbb/seed.md");

    fs::write(env.project_config(), "ignores = [\"bbb/**\"]\n").expect("project config v2");
    let mut second = env.client();
    second
        .handshake(env.root())
        .expect("reopen swaps watcher config");
    write_file(env.root(), "aaa/a.md", doc("alphawidget9").as_bytes());
    write_file(env.root(), "bbb/b.md", doc("betawidget9").as_bytes());

    let deadline = Instant::now() + watcher_deadline();
    let mut found = false;
    while Instant::now() < deadline {
        if search_before(&mut second, "alphawidget9", deadline) == vec!["aaa/a.md"]
            && Instant::now() < deadline
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(found, "watcher must index newly unignored aaa/**");
    assert!(
        search(&mut second, "betawidget9").is_empty(),
        "watcher must not index newly ignored bbb/**"
    );
}

#[test]
fn max_file_size_above_frame_cap_is_rejected() {
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    fs::write(env.global_config(), "max_file_size = 99999999\n").expect("global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice");
    assert!(
        notice.contains("max_file_size"),
        "notice must name the limit: {notice}"
    );

    let err = registry
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect_err("out-of-frame max_file_size must block indexing");
    assert!(
        err.to_string().contains("max_file_size"),
        "unexpected error: {err}"
    );
}

#[test]
fn max_file_size_at_frame_boundary_is_accepted() {
    let boundary = u64::try_from(MAX_FRAME_BYTES).expect("usize fits u64") - FRAME_OVERHEAD;
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    fs::write(env.global_config(), format!("max_file_size = {boundary}\n")).expect("global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(
        status["restart_required"], false,
        "exact boundary must be accepted: {status}"
    );
}

#[test]
fn oversized_global_config_is_rejected() {
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    let file = fs::File::create(env.global_config()).expect("create config");
    file.set_len(docsbase_memory::limits::MAX_STATE_BYTES + 1)
        .expect("grow config");
    drop(file);
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    let notice = status["notice"].as_str().expect("notice");
    assert!(notice.contains("over the"), "notice: {notice}");
}

#[test]
fn v2_config_drives_fresh_direct_index_and_daemon_search() {
    let mut env = Env::new(&[("guide.md", &doc("quasar telemetry calibration"))]);
    write_file(
        env.cache(),
        "config/config.toml",
        b"config_version=2\nauto_index=false\n[vector_cache]\nmax_bytes=8192\nmax_entries=74\nbusy_timeout=33\n",
    );
    write_file(
        env.root(),
        ".docsbase.toml",
        b"config_version=2\nmax_docs_per_project=50\n",
    );
    let report = env.run_json(&["config", "effective"]);
    let fields = report["fields"].as_array().unwrap();
    for (key, value) in [
        ("vector_cache.max_bytes", 8192),
        ("vector_cache.max_entries", 74),
        ("vector_cache.busy_timeout", 33),
    ] {
        assert!(fields.iter().any(|field| {
            field["key"] == key && field["value"] == value && field["source"]["kind"] == "global"
        }));
    }
    assert!(
        fields
            .iter()
            .any(|field| field["key"] == "max_docs_per_project" && field["value"] == 50)
    );
    let indexed = env.run_json(&["index"]);
    assert!(indexed["project_id"].is_i64());
    assert_eq!(env.run_json(&["status"])["schema_version"], 5);
    assert!(
        !env.cache().join("vectors.db").exists(),
        "lexical v2 index must leave the vector database absent"
    );
    assert_eq!(
        env.run_json(&["search", "quasar telemetry"])[0]["path"],
        "guide.md"
    );
    env.start_daemon();
    assert!(matches!(
        docsbase_memory::vector_cache::VectorCache::open(env.cache()),
        Err(Error::Admission { .. })
    ));
    assert_eq!(
        env.run_json(&["search", "quasar telemetry"])[0]["path"],
        "guide.md"
    );
    write_file(
        env.root(),
        "new.md",
        doc("pulsar orbital measurements").as_bytes(),
    );
    assert_eq!(env.run_json(&["sync"])["state"], "done");
    let another = tempfile::tempdir().unwrap();
    write_file(
        another.path(),
        "another.md",
        doc("radio observatory antenna").as_bytes(),
    );
    let from_daemon = env.run_json(&["sync", another.path().to_str().unwrap()]);
    assert_eq!(from_daemon["state"], "done");
    assert_ne!(from_daemon["project_id"], indexed["project_id"]);
    env.restart_daemon();
    assert!(
        !env.cache().join("vectors.db").exists(),
        "lexical v2 search and sync must leave the vector database absent"
    );
    assert!(
        env.run_json(&["search", "pulsar orbital"])
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "new.md")
    );
}

#[test]
fn v2_config_plan_compares_sources_with_the_published_generation() {
    let env = Env::new(&[("guide.md", "indexplanningwidget")]);
    let empty = env.run_json(&["config", "plan", "--check"]);
    assert_eq!(empty["actions"], serde_json::json!([]));
    assert!(!env.cache().join("registry.db").exists());
    env.run_json(&["index"]);

    let original = env.run_json(&["config", "plan", "--check"]);
    assert_eq!(original["actions"], serde_json::json!([]));
    assert_eq!(original["blockers"], serde_json::json!([]));
    assert_eq!(original["full_survey"], true);
    let registry = env.cache().join("registry.db");
    let encoded = fs::read(&registry).unwrap();
    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\nlong_chunk_penalty = 0.125\n",
    )
    .unwrap();
    let query_only = env.run_json(&["config", "plan", "--check"]);
    assert_eq!(query_only["actions"], serde_json::json!([]));
    assert_ne!(query_only["plan_id"], original["plan_id"]);

    fs::write(
        env.project_config(),
        "config_version = 2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let mut before = std::collections::BTreeMap::new();
    snapshot_config_plan_files(env.cache(), &mut before);
    snapshot_config_plan_files(env.root(), &mut before);
    let output = env
        .cmd()
        .args(["config", "plan", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let changed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(changed["actions"].as_array().unwrap().len(), 1);
    assert_eq!(changed["actions"][0]["effect"], "rebuild");
    assert_eq!(
        changed["actions"][0]["resource"]["value"]["component"],
        "project_index"
    );
    assert_eq!(fs::read(&registry).unwrap(), encoded);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!env.cache().join(format!("registry.db{suffix}")).exists());
    }
    let mut after = std::collections::BTreeMap::new();
    snapshot_config_plan_files(env.cache(), &mut after);
    snapshot_config_plan_files(env.root(), &mut after);
    assert_eq!(after, before);

    env.run_json(&["index"]);
    let applied = env.run_json(&["config", "plan", "--check"]);
    assert_eq!(applied["actions"], serde_json::json!([]));
    assert_ne!(applied["plan_id"], changed["plan_id"]);
}

#[test]
fn v2_live_config_plan_observes_current_runtime_without_loading_new_settings() {
    let mut env = Env::new(&[("guide.md", "liveconfigurationwidget")]);
    env.run_json(&["index"]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let baseline = env.run_json(&["config", "plan", "--live", "--check"]);
    assert_eq!(baseline["basis"], "live");
    assert_eq!(baseline["actions"], serde_json::json!([]));
    assert_eq!(baseline["ann"]["state"], "disabled");

    let mut mismatched = env.client();
    let response = mismatched
        .call(
            docsbase_memory::ipc::protocol::Request::InspectProjectConfig {
                protocol_version: docsbase_memory::ipc::protocol::PROTOCOL_VERSION,
                build_id: "mismatched-build".into(),
                path: env.root().to_owned(),
            },
        )
        .unwrap();
    assert!(
        matches!(
            response,
            docsbase_memory::ipc::protocol::Response::Error { .. }
        ),
        "{response:?}"
    );
    assert!(!env.cache().join("logs/conflicts.ndjson").exists());

    let indexed = fs::read(env.cache().join("registry.db")).unwrap();
    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\nlong_chunk_penalty = 0.125\n",
    )
    .unwrap();
    let output = env
        .cmd()
        .args(["config", "plan", "--live", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let changed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(changed["actions"].as_array().unwrap().len(), 1);
    assert_eq!(changed["actions"][0]["effect"], "reconfigure_runtime");
    assert_ne!(changed["plan_id"], baseline["plan_id"]);
    assert_eq!(env.run_json(&["config", "plan", "--live"]), changed);
    assert_eq!(fs::read(env.cache().join("registry.db")).unwrap(), indexed);
    assert_eq!(
        search(&mut client, "liveconfigurationwidget"),
        vec!["guide.md"]
    );

    fs::write(
        env.project_config(),
        "config_version = 2\n[ann.search]\nef_base = 64\nef_max = 4096\nfetch_multiplier = 4\n",
    )
    .unwrap();
    let explicit = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(explicit["actions"].as_array().unwrap().len(), 1);
    assert_eq!(
        explicit["actions"][0]["resource"]["value"],
        "project_runtime"
    );

    fs::write(
        env.global_config(),
        "config_version = 2\n[daemon]\npoll_interval_ms = 25\n",
    )
    .unwrap();
    let restart = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(restart["restart_required"], true);
    assert_eq!(restart["actions"][0]["resource"]["value"], "daemon_global");
    assert_restart_follows_replacement(&env);
}

fn assert_restart_follows_replacement(env: &Env) {
    fs::write(
        env.global_config(),
        "config_version = 2\n[index.chunk]\nmax_chars = 1200\n",
    )
    .unwrap();
    let replacement = env.run_json(&["config", "plan", "--live"]);
    let actions = replacement["actions"].as_array().unwrap();
    let index = &actions[0]["resource"];
    assert_eq!(index["value"]["component"], "project_index");
    assert_eq!(actions[1]["resource"]["value"], "daemon_global");
    assert_eq!(actions[1]["dependencies"], serde_json::json!([index]));
}

#[test]
fn v2_shared_watcher_toggle_takes_effect_on_restart_and_keeps_sync() {
    let mut env = Env::new(&[("guide.md", "initialwatcherwidget")]);
    fs::write(
        env.global_config(),
        "config_version = 2\nauto_index = true\n",
    )
    .unwrap();
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let status = |client: &mut Client| client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(status(&mut client)["watchers"], 1);

    fs::write(
        env.global_config(),
        "config_version = 2\nauto_index = true\n[watch]\nenabled = false\n",
    )
    .unwrap();
    let changed = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(changed["restart_required"], true, "{changed}");
    let actions = changed["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{changed}");
    assert_eq!(actions[0]["resource"]["value"], "daemon_global");
    assert_eq!(status(&mut client)["restart_required"], true);

    write_file(env.root(), "watched.md", b"automaticwatcherwidget");
    let deadline = Instant::now() + watcher_deadline();
    loop {
        if search_before(&mut client, "automaticwatcherwidget", deadline) == ["watched.md"] {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    drop(client);
    env.restart_daemon();
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let disabled = status(&mut client);
    assert_eq!(disabled["restart_required"], false);
    assert_eq!(disabled["watchers"], 0);
    assert_eq!(disabled["projects"][0]["watched"], false);
    let settled = env.run_json(&["config", "plan", "--live", "--check"]);
    assert_eq!(settled["actions"], serde_json::json!([]));

    let another = tempfile::tempdir().unwrap();
    write_file(another.path(), "fresh.md", b"freshwatcherwidget");
    let mut registered = env.client();
    registered.handshake(another.path()).unwrap();
    assert_eq!(search(&mut registered, "freshwatcherwidget"), ["fresh.md"]);
    assert_eq!(status(&mut client)["watchers"], 0);

    write_file(env.root(), "manual.md", b"manualwatcherwidget");
    assert_eq!(
        search(&mut client, "manualwatcherwidget"),
        Vec::<String>::new()
    );
    assert_eq!(env.run_json(&["sync"])["state"], "done");
    assert_eq!(search(&mut client, "manualwatcherwidget"), ["manual.md"]);
    assert_eq!(status(&mut client)["watchers"], 0);
}

#[test]
fn v2_live_plan_uses_effective_global_settings_for_restart() {
    let mut env = Env::new(&[("guide.md", "semanticglobalwidget")]);
    let write_global = |grace, note: &str| {
        fs::write(
            env.global_config(),
            format!("config_version = 2\n[daemon]\nidle_grace_ms = {grace}\n{note}\n"),
        )
        .unwrap();
    };
    write_global(1200, "# Startup file");
    env.run_json(&["index"]);
    env.start_daemon(); // --grace-ms=3000 overrides the file
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let baseline = env.run_json(&["config", "plan", "--live", "--check"]);
    fs::write(
        env.global_config(),
        "config_version = 2\n[daemon]\nidle_grace_ms = 1800\n# Updated file with an overridden setting\n",
    ).unwrap();
    let edited = env.run_json(&["config", "plan", "--live", "--check"]);
    assert_eq!(edited["restart_required"], false);
    assert_eq!(edited["actions"], serde_json::json!([]));
    assert_ne!(edited["plan_id"], baseline["plan_id"]);
    let status = |client: &mut Client| {
        client.call_tool("status", serde_json::json!({})).unwrap()["restart_required"].clone()
    };
    assert_eq!(status(&mut client), false);

    fs::write(
        env.global_config(),
        "config_version = 2\n[daemon]\nidle_grace_ms = 1800\npoll_interval_ms = 40\n",
    )
    .unwrap();
    let changed = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(changed["restart_required"], true);
    assert_eq!(changed["actions"][0]["resource"]["value"], "daemon_global");
    assert_eq!(status(&mut client), true);
    fs::write(
        env.global_config(),
        "config_version = 2\n[vector_cache]\nmax_entries = 1234\nbusy_timeout = 789\n",
    )
    .unwrap();
    let vector_change = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(vector_change["restart_required"], true);
    assert_ne!(vector_change["actions"], serde_json::json!([]));
    assert_eq!(status(&mut client), true);
    assert_eq!(
        search(&mut client, "semanticglobalwidget"),
        vec!["guide.md"]
    );
}

#[test]
fn v2_live_plan_reports_profile_restart_for_an_excluded_project() {
    let mut env = Env::new(&[("guide.md", "requestedprofilewidget")]);
    let fixed = TempDir::new().unwrap();
    fs::write(fixed.path().join("guide.md"), "fixedprofilewidget").unwrap();
    env.run_json(&["index"]);
    env.run_json(&["index", fixed.path().to_str().unwrap()]);
    env.daemon = Some(
        env.cmd()
            .args(["serve", "--detached"])
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(wait_until(Duration::from_secs(10), || platform::exists(
        &platform::daemon_endpoint(env.cache())
    )));
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let root = toml::Value::String(
        fixed
            .path()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
    );
    fs::write(env.global_config(), format!(
        "config_version = 2\n[daemon]\nprofile = {{ mode = 'website', project_root = {root} }}\n"
    )).unwrap();
    let changed = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(changed["restart_required"], true);
    assert_eq!(changed["actions"][0]["resource"]["value"], "daemon_global");
    assert_eq!(changed["blockers"][0]["code"], "invalid_settings");
    assert_eq!(
        client.call_tool("status", serde_json::json!({})).unwrap()["restart_required"],
        true
    );
}

#[test]
fn v2_live_config_applies_only_the_reviewed_project_runtime() {
    let fence = format!(
        "```text\nreviewedliveconfigurationwidget {}\n```\n",
        "text ".repeat(1_000)
    );
    let mut env = Env::new(&[("guide.md", &fence)]);
    env.run_json(&["index"]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    let score = |client: &mut Client| {
        client
            .call_tool(
                "search_docs",
                serde_json::json!({"query":"reviewedliveconfigurationwidget"}),
            )
            .unwrap()[0]["score"]
            .as_f64()
            .unwrap()
    };
    let initial_score = score(&mut client);
    let registry = env.cache().join("registry.db");
    let before = fs::read(&registry).unwrap();
    let set_penalty = |penalty| {
        fs::write(
            env.project_config(),
            format!("config_version = 2\n[lexical]\nlong_chunk_penalty={penalty}\n"),
        )
        .unwrap();
        env.run_json(&["config", "plan", "--live"])
    };
    let stale = set_penalty(0.125);
    let current = set_penalty(0.25);
    let refused = env
        .cmd()
        .args([
            "config",
            "apply",
            "--live",
            "--plan-id",
            stale["plan_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        refused.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(env.run_json(&["config", "plan", "--live"]), current);
    assert_eq!(current["blockers"], serde_json::json!([]));
    let accepted = env.run_json(&[
        "config",
        "apply",
        "--live",
        "--plan-id",
        current["plan_id"].as_str().unwrap(),
    ]);
    assert_eq!(accepted["applied"], true);
    assert_eq!(accepted["plan_id"], current["plan_id"]);
    let installed = env.run_json(&["config", "plan", "--live", "--check"]);
    assert_eq!(installed["actions"], serde_json::json!([]));
    let runtime = installed["expected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["resource"]["value"] == "project_runtime")
        .unwrap();
    assert_eq!(runtime["state"]["kind"], "known_identity");
    assert_eq!(
        runtime["state"]["value"],
        current["actions"][0]["target"]["value"]
    );
    assert_eq!(fs::read(registry).unwrap(), before);
    assert!(score(&mut client) < initial_score);
    assert_eq!(
        search(&mut client, "reviewedliveconfigurationwidget"),
        vec!["guide.md"]
    );

    fs::write(
        env.global_config(),
        "config_version = 2\n[daemon]\npoll_interval_ms = 25\n",
    )
    .unwrap();
    let restart = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(restart["restart_required"], true);
    let refused = env
        .cmd()
        .args([
            "config",
            "apply",
            "--live",
            "--plan-id",
            restart["plan_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(5));
    assert_eq!(env.run_json(&["config", "plan", "--live"]), restart);
}

#[test]
fn v2_live_config_apply_waits_for_the_reviewed_index_replacement() {
    let mut env = Env::new(&[("guide.md", "reviewedliveindexwidget")]);
    let initial = env.run_json(&["index"]);
    env.start_daemon();
    let project_id = initial["project_id"].as_i64().unwrap();
    let selected = || -> i64 {
        let registry = rusqlite::Connection::open_with_flags(
            env.cache().join(docsbase_memory::store::DB_FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        registry
            .query_row(
                "SELECT active_id FROM project_generation_head WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let original_generation = selected();
    let mut client = env.client();
    client.handshake(env.root()).unwrap();

    fs::write(
        env.project_config(),
        "config_version = 2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let reviewed = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(reviewed["blockers"], serde_json::json!([]));
    let replacement = reviewed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["resource"]["value"]["component"] == "project_index")
        .expect("reviewed project index action");
    assert_eq!(replacement["effect"], "rebuild");
    let mut old_client = env.client();
    old_client.handshake_registry().unwrap();
    let old_request = serde_json::from_value(serde_json::json!({
        "method": "ApplyProjectConfig", "params": {
            "path": env.root(), "plan_id": reviewed["plan_id"], "allow_remote": false,
        }
    }))
    .unwrap();
    let docsbase_memory::ipc::protocol::Response::ToolResult { value: refused } =
        old_client.call(old_request).unwrap()
    else {
        panic!("missing apply status for client without job acknowledgement support");
    };
    assert_eq!(refused["apply_status"], 5);
    assert_eq!(selected(), original_generation);

    let applied = env.run_json(&[
        "config",
        "apply",
        "--live",
        "--plan-id",
        reviewed["plan_id"].as_str().unwrap(),
    ]);
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["plan_id"], reviewed["plan_id"]);
    assert_eq!(applied["project_id"], project_id);
    assert_eq!(applied["stats"]["docs"], 1);
    assert_ne!(selected(), original_generation);

    let installed = env.run_json(&["config", "plan", "--live", "--check"]);
    assert_eq!(installed["actions"], serde_json::json!([]));
    assert_eq!(installed["blockers"], serde_json::json!([]));
    assert_eq!(
        search(&mut client, "reviewedliveindexwidget"),
        vec!["guide.md"]
    );
}

#[test]
#[cfg(feature = "hybrid-remote")]
fn v2_live_config_apply_requires_remote_grant() {
    let mut env = Env::new(&[("guide.md", "reviewedlivegrantwidget")]);
    env.run_json(&["index"]);
    let mut daemon = env.cmd();
    daemon.env_remove("OPENROUTER_API_KEY");
    env.start_daemon_with(daemon);
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    fs::write(
        env.project_config(),
        "config_version=2\n[hybrid]\nenabled=true\nranking='rrf'\n[hybrid.embedding]\nprovider='openrouter'\nmodel='fixture'\ndimensions=3\n",
    )
    .unwrap();
    let reviewed = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(reviewed["blockers"], serde_json::json!([]));
    assert_eq!(reviewed["required_grants"], serde_json::json!(["remote"]));
    let refused = env
        .cmd()
        .args([
            "config",
            "apply",
            "--live",
            "--plan-id",
            reviewed["plan_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(5), "{stderr}");
    assert!(refused.stdout.is_empty(), "{refused:?}");
    assert!(!stderr.contains("OPENROUTER_API_KEY"), "{stderr}");
    assert!(!env.cache().join("vectors.db").exists());
    assert_eq!(env.run_json(&["config", "plan", "--live"]), reviewed);
}

#[cfg(not(feature = "hybrid-remote"))]
#[test]
fn v2_live_config_plan_blocks_an_uncompiled_reranker() {
    let mut env = Env::new(&[("guide.md", "lexicalconfigfixture")]);
    env.run_json(&["index"]);
    env.start_daemon();
    fs::write(
        env.project_config(),
        "config_version=2\n[hybrid]\nenabled=true\nranking='lexical'\n[hybrid.reranker]\nprovider='openrouter'\nmodel='fixture/rerank'\n",
    ).unwrap();
    let plan = env.run_json(&["config", "plan", "--live"]);
    assert_eq!(plan["actions"][0]["effect"], "reconfigure_runtime");
    assert!(
        plan["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "executor_unavailable")
    );
}

#[test]
fn v2_live_config_plan_leaves_wal_metadata_to_recovery() {
    let mut env = Env::new(&[("guide.md", "configurationrecoverywidget")]);
    env.run_json(&["index"]);
    env.start_daemon();
    let metadata = env.cache().join("projects/1/generations/1/metadata.db");
    let db = rusqlite::Connection::open(&metadata).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.close().unwrap();
    let before = fs::read(&metadata).unwrap();
    assert_eq!(&before[18..20], &[2, 2]);
    let result = env
        .cmd()
        .args(["config", "plan", "--live", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("RecoveryRequired"));
    assert_eq!(fs::read(&metadata).unwrap(), before);
    for suffix in ["metadata.db-wal", "metadata.db-shm", "metadata.db-journal"] {
        assert!(!metadata.with_file_name(suffix).exists());
    }
}

#[test]
fn v2_live_config_plan_rejects_unreadable_published_tantivy() {
    let mut env = Env::new(&[("guide.md", "configurationtantivywidget")]);
    env.run_json(&["index"]);
    env.start_daemon();
    let metadata = env
        .cache()
        .join("projects/1/generations/1/tantivy/meta.json");
    let mut bytes = fs::read(&metadata).unwrap();
    bytes[0] = b'!';
    fs::write(&metadata, &bytes).unwrap();
    let result = env
        .cmd()
        .args(["config", "plan", "--live", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&metadata).unwrap(), bytes);
}

#[test]
fn v2_live_config_plan_without_daemon_does_not_start_one() {
    let env = Env::new(&[("guide.md", "noautostartwidget")]);
    let result = env
        .cmd()
        .args(["config", "plan", "--live"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!env.cache().join("registry.db").exists());
    assert!(!env.cache().join("state").exists());
}

fn snapshot_config_plan_files(
    root: &Path,
    files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            snapshot_config_plan_files(&path, files);
        } else {
            let data = fs::read(&path).unwrap();
            files.insert(path, data);
        }
    }
}

#[test]
fn v2_config_plan_binds_empty_cache_context_and_reports_invalid_effective_settings() {
    let env = Env::new(&[("guide.md", "configurationcontextwidget")]);
    let alternative = tempfile::tempdir().unwrap();
    let initial = env.run_json(&["config", "plan"]);
    let different = env
        .cmd()
        .env("DOCSBASE_CACHE_DIR", alternative.path())
        .args(["config", "plan"])
        .output()
        .unwrap();
    assert!(different.status.success());
    let other: Value = serde_json::from_slice(&different.stdout).unwrap();
    assert_ne!(initial["plan_id"], other["plan_id"]);

    fs::write(
        env.project_config(),
        "config_version = 2\nmax_file_size = 0\n",
    )
    .unwrap();
    let invalid = env
        .cmd()
        .args(["config", "plan", "--check"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(5));
    let blocked: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(blocked["actions"], serde_json::json!([]));
    assert_eq!(blocked["blockers"][0]["code"], "invalid_settings");
    assert!(!env.cache().join("registry.db").exists());
    assert!(!alternative.path().join("registry.db").exists());
}

#[test]
fn v2_config_plan_reports_vector_changes_without_resolving_provider_credentials() {
    let env = Env::new(&[("guide.md", "vectorplanningwidget")]);
    env.run_json(&["index"]);
    fs::write(env.project_config(),
        "config_version = 2\n[hybrid]\nenabled = true\nranking = 'rrf'\n[hybrid.embedding]\nprovider = 'openrouter'\nmodel = 'planning-test-model'\ndimensions = 3\n",
    ).unwrap();
    let output = env
        .cmd()
        .env_remove("OPENROUTER_API_KEY")
        .args(["config", "plan", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(if cfg!(feature = "hybrid-remote") {
            3
        } else {
            5
        }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["actions"].as_array().unwrap().len(), 2);
    assert_eq!(
        plan["actions"][0]["resource"]["value"]["component"],
        "document_vectors"
    );
    assert_eq!(
        plan["actions"][1]["resource"]["value"]["component"],
        "project_index"
    );
    assert_eq!(
        plan["actions"][1]["dependencies"][0],
        plan["actions"][0]["resource"]
    );
    assert_eq!(plan["required_grants"], serde_json::json!(["remote"]));
    if cfg!(feature = "hybrid-remote") {
        assert_eq!(plan["blockers"], serde_json::json!([]));
    } else {
        assert_eq!(plan["blockers"][0]["code"], "executor_unavailable");
    }
    assert!(!env.cache().join("vectors.db").exists());

    let root = toml::Value::String(env.root().canonicalize().unwrap().to_str().unwrap().into());
    fs::write(env.global_config(), format!(
        "config_version = 2\n[daemon]\nprofile = {{ mode = 'website', project_root = {root} }}\n"
    )).unwrap();
    let blocked = env
        .cmd()
        .args(["config", "plan", "--check"])
        .output()
        .unwrap();
    assert_eq!(blocked.status.code(), Some(5));
    let report: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(report["actions"], serde_json::json!([]));
    assert_eq!(report["blockers"][0]["code"], "invalid_settings");
}

#[test]
fn v2_config_plan_leaves_registry_recovery_to_the_owner() {
    let env = Env::new(&[("guide.md", "recoveryplanningwidget")]);
    env.run_json(&["index"]);
    let recovery = env.cache().join("registry.db-journal");
    fs::write(&recovery, b"unsettled fixture").unwrap();
    let output = env
        .cmd()
        .args(["config", "plan", "--check"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["observation_failure"], "recovery_required");
    assert_eq!(report["actions"], serde_json::json!([]));
    assert_eq!(fs::read(recovery).unwrap(), b"unsettled fixture");
}

#[test]
fn v2_config_apply_requires_the_current_plan_and_publishes_its_replacement() {
    let env = Env::new(&[("guide.md", "# Guide\n\nreviewedconfigwidget")]);
    env.run_json(&["index"]);
    let before = env.run_json(&["config", "plan"]);
    fs::write(
        env.project_config(),
        "config_version=2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let requested = env.run_json(&["config", "plan"]);
    let apply = |plan: &Value| {
        env.cmd()
            .args([
                "config",
                "apply",
                "--plan-id",
                plan["plan_id"].as_str().unwrap(),
            ])
            .output()
            .unwrap()
    };
    let stale = apply(&before);
    assert_eq!(
        stale.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert_eq!(env.run_json(&["config", "plan"]), requested);
    let maintenance = docsbase_memory::daemon::admission::Lease::acquire(
        env.cache(),
        &docsbase_memory::ipc::protocol::build_id(),
        5,
    )
    .unwrap();
    let busy = apply(&requested);
    assert_eq!(
        busy.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&busy.stderr)
    );
    drop(maintenance);
    assert_eq!(env.run_json(&["config", "plan"]), requested);
    let applied = apply(&requested);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let after = env.run_json(&["config", "plan", "--check"]);
    assert_eq!(after["actions"], serde_json::json!([]));
    assert_ne!(after["plan_id"], requested["plan_id"]);
    assert_eq!(apply(&requested).status.code(), Some(4));
    assert!(
        apply(&after).status.success(),
        "a current no-op is accepted"
    );
    assert_eq!(env.run_json(&["config", "plan"]), after);
    // A normal search may start the daemon, so exercise it after the
    // assertions that require the catalog to be stopped.
    assert_eq!(
        env.run_json(&["search", "reviewedconfigwidget"])[0]["path"],
        "guide.md"
    );
}

#[test]
#[cfg(feature = "hybrid-remote")]
fn v2_config_apply_refuses_remote_work_before_vector_or_provider_setup() {
    let env = Env::new(&[("guide.md", "grantconfigwidget")]);
    env.run_json(&["index"]);
    fs::write(
        env.project_config(),
        "config_version=2\n[hybrid]\nenabled=true\nranking='rrf'\n[hybrid.embedding]\nprovider='openrouter'\nmodel='fixture'\ndimensions=3\n",
    )
    .unwrap();
    let before = env.run_json(&["config", "plan"]);
    let mut before_files = std::collections::BTreeMap::new();
    snapshot_config_plan_files(env.cache(), &mut before_files);
    let output = env
        .cmd()
        .env_remove("OPENROUTER_API_KEY")
        .args([
            "config",
            "apply",
            "--plan-id",
            before["plan_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(5), "{stderr}");
    assert!(stderr.contains("--allow-remote"), "{stderr}");
    assert!(!env.cache().join("vectors.db").exists());
    let mut after_files = std::collections::BTreeMap::new();
    snapshot_config_plan_files(env.cache(), &mut after_files);
    assert_eq!(after_files, before_files);
    assert_eq!(env.run_json(&["config", "plan"]), before);
}

#[test]
fn v2_startup_policy_refuses_or_applies_a_required_local_replacement() {
    let mut env = Env::new(&[("guide.md", "startupreplacementwidget")]);
    env.run_json(&["index"]);
    fs::write(
        env.project_config(),
        "config_version=2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let plan = env.run_json(&["config", "plan"]);
    let serve = |options: &[&str]| {
        env.cmd()
            .args(["serve", "--grace-ms", "1"])
            .args(options)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    for (options, code) in [
        (&["--on-invalidation", "fail"][..], 3),
        (&["--detached"][..], 3),
        (&["--on-invalidation", "prompt"][..], 5),
    ] {
        let output = serve(options);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(env.run_json(&["config", "plan"]), plan);
        assert!(!env.cache().join("state").exists());
    }
    env.daemon = Some(
        env.cmd()
            .args(["serve", "--on-invalidation", "apply"])
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(wait_until(Duration::from_secs(10), || platform::exists(
        &platform::daemon_endpoint(env.cache())
    )));
    let mut client = env.client();
    client.handshake(env.root()).unwrap();
    assert_eq!(
        search(&mut client, "startupreplacementwidget"),
        vec!["guide.md"]
    );
    stop_daemon(env.cache()).unwrap();
    assert!(env.daemon.take().unwrap().wait().unwrap().success());
    assert_eq!(
        env.run_json(&["config", "plan", "--check"])["actions"],
        serde_json::json!([])
    );
}

#[test]
#[cfg(feature = "hybrid-remote")]
fn v2_startup_checks_all_project_grants_before_any_replacement() {
    let env = Env::new(&[("guide.md", "localstartupwidget")]);
    env.run_json(&["index"]);
    let remote = tempfile::tempdir().unwrap();
    fs::write(remote.path().join("guide.md"), "remotestartupwidget").unwrap();
    env.run_json(&["index", remote.path().to_str().unwrap()]);
    fs::write(
        env.project_config(),
        "config_version=2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    fs::write(
        remote.path().join(".docsbase.toml"),
        "config_version=2\n[hybrid]\nenabled=true\nranking='rrf'\n[hybrid.embedding]\nprovider='openrouter'\nmodel='fixture'\ndimensions=3\n",
    ).unwrap();
    let before = env.run_json(&["config", "plan"]);
    let output = env
        .cmd()
        .env_remove("OPENROUTER_API_KEY")
        .args(["serve", "--grace-ms", "1", "--on-invalidation", "apply"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow-remote"));
    assert_eq!(env.run_json(&["config", "plan"]), before);
    assert!(!env.cache().join("vectors.db").exists());
    assert!(!env.cache().join("state").exists());
}

#[test]
fn v2_startup_uses_the_cli_profile_override_for_planning_and_apply() {
    let env = Env::new(&[("guide.md", "fixedprofilestartupwidget")]);
    env.run_json(&["index"]);
    let second = tempfile::tempdir().unwrap();
    let other = second.path().join("manuals");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("guide.md"), "sharedprofilestartupwidget").unwrap();
    env.run_json(&["index", other.to_str().unwrap()]);
    let fixed = toml::Value::String(env.root().canonicalize().unwrap().to_str().unwrap().into());
    fs::write(
        env.global_config(),
        format!("config_version=2\n[daemon]\nprofile={{mode='website',project_root={fixed}}}\n"),
    )
    .unwrap();
    fs::write(
        other.join(".docsbase.toml"),
        "config_version=2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let detached = env
        .cmd()
        .args([
            "serve",
            "--detached",
            "--project",
            other.to_str().unwrap(),
            "--on-invalidation",
            "apply",
        ])
        .output()
        .unwrap();
    assert!(!detached.status.success());
    assert!(String::from_utf8_lossy(&detached.stderr).contains("--detached is inapplicable"));
    let renamed = second.path().join("Manuals");
    let intermediate = second.path().join("case-change");
    let selected = if cfg!(any(windows, target_os = "macos")) {
        fs::rename(&other, &intermediate).unwrap();
        fs::rename(&intermediate, &renamed).unwrap();
        if other.is_dir() {
            &renamed
        } else {
            fs::rename(&renamed, &other).unwrap();
            &other
        }
    } else {
        &other
    };
    let mut fixed = env
        .cmd()
        .args([
            "serve",
            "--project",
            selected.to_str().unwrap(),
            "--on-invalidation",
            "fail",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    if !wait_until(Duration::from_secs(10), || {
        fixed.try_wait().unwrap().is_some()
    }) {
        let _ = fixed.kill();
    }
    let fixed_override = fixed.wait_with_output().unwrap();
    assert_eq!(
        fixed_override.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&fixed_override.stderr)
    );
    if selected == &renamed {
        fs::rename(&renamed, &intermediate).unwrap();
        fs::rename(&intermediate, &other).unwrap();
    }
    let applied = env
        .cmd()
        .args([
            "serve",
            "--grace-ms",
            "1",
            "--profile",
            "shared",
            "--on-invalidation",
            "apply",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    fs::remove_file(env.global_config()).unwrap();
    let plan = env.run_json(&["config", "plan", other.to_str().unwrap(), "--check"]);
    assert_eq!(plan["actions"], serde_json::json!([]));
}

#[test]
fn v2_startup_recovers_an_interrupted_registry_before_considering_replacements() {
    let mut env = Env::new(&[("guide.md", "registryrecoverywidget")]);
    env.run_json(&["index"]);
    env.start_daemon();
    let job = {
        let conn = rusqlite::Connection::open(env.cache().join("registry.db")).unwrap();
        conn.execute(
            "INSERT INTO sync_jobs (project_id,state,started_at) VALUES (1,'queued',0)",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let mut daemon = env.daemon.take().unwrap();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let main = env.cache().join("registry.db");
    let wal = env.cache().join("registry.db-wal");
    let before = (fs::read(&main).unwrap(), fs::read(&wal).unwrap());
    let edited = env.root().join(".docsbase.toml");
    fs::write(
        &edited,
        "config_version=2\n[index.chunk]\nmax_chars=1200\noverlap_chars=100\n",
    )
    .unwrap();
    let explicit = env
        .cmd()
        .args(["serve", "--grace-ms", "1", "--on-invalidation", "fail"])
        .output()
        .unwrap();
    assert_eq!(explicit.status.code(), Some(5));
    assert_eq!((fs::read(&main).unwrap(), fs::read(&wal).unwrap()), before);
    let automatic = env
        .cmd()
        .args(["serve", "--grace-ms", "1"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        automatic.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&automatic.stderr)
    );
    assert!(!wal.exists());
    assert!(!env.cache().join("vectors.db").exists());
    fs::remove_file(edited).unwrap();
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().unwrap();
    let status = client
        .call_tool("sync_status", serde_json::json!({"job_id":job}))
        .unwrap();
    assert_eq!(status["state"], "error");
    assert_eq!(status["stats"]["orphaned"], true);
}

#[test]
#[cfg(target_os = "linux")]
fn v2_first_index_reports_non_unicode_filename_and_publishes_other_files() {
    use std::os::unix::ffi::OsStrExt as _;

    let env = Env::new(&[("guide.md", "validsourcefilenamewidget")]);
    let invalid = env
        .root()
        .join(std::ffi::OsStr::from_bytes(b"invalid-\xff.md"));
    fs::write(invalid, "existing content in an unsupported filename").unwrap();

    let indexed = env.run_json(&["index"]);
    assert_eq!(indexed["stats"]["docs"], 1);
    assert_eq!(indexed["stats"]["errors"], 1);
    assert!(
        indexed["stats"]["warnings"][0]["message"]
            .as_str()
            .unwrap()
            .contains("UTF-8")
    );
    assert_eq!(
        env.run_json(&["search", "validsourcefilenamewidget"])[0]["path"],
        "guide.md"
    );
}

#[test]
fn v2_schema_observation_accepts_a_fresh_closed_catalog() {
    use docsbase_memory::store::{Db, settings};

    let env = Env::new(&[("guide.md", "schemaplanningwidget")]);
    env.run_json(&["index"]);
    let policy = settings::standalone().unwrap();
    let facts = Db::inspect_readonly(env.cache(), &policy).unwrap();
    assert!(
        facts.artifacts[0]
            .identity
            .as_deref()
            .unwrap()
            .starts_with("sqlite-schema-v1:5:")
    );
    let assessment = settings::DbSpec::assess_schema(&facts).unwrap();
    assert!(assessment.blockers.is_empty(), "{assessment:?}");
    assert!(assessment.actions.is_empty(), "{assessment:?}");
}

#[test]
fn v2_stopped_index_reuses_unchanged_generation() {
    let env = Env::new(&[("guide.md", "unchangedcatalogwidget")]);
    let indexed = env.run_json(&["index"]);
    let id = indexed["project_id"].as_i64().unwrap();
    let selected = || -> i64 {
        let db =
            rusqlite::Connection::open(env.cache().join(docsbase_memory::store::DB_FILE)).unwrap();
        db.query_row(
            "SELECT active_id FROM project_generation_head WHERE project_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    };
    let generation = selected();

    let repeated = env.run_json(&["index"]);
    assert_eq!(repeated["stats"]["docs"], 0);
    assert_eq!(repeated["stats"]["skipped"], 1);
    assert_eq!(selected(), generation);

    let path = env.cache().join(format!(
        "projects/{id}/generations/{generation}/metadata.db"
    ));
    let metadata = rusqlite::Connection::open(path).unwrap();
    assert_eq!(metadata.execute("DELETE FROM chunks", []).unwrap(), 1);
    drop(metadata);
    let repaired = env.run_json(&["index"]);
    assert_eq!(repaired["stats"]["docs"], 1);
    let repaired_generation = selected();
    assert!(repaired_generation > generation);

    let tantivy = env.cache().join(format!(
        "projects/{id}/generations/{repaired_generation}/tantivy"
    ));
    let positions = fs::read_dir(&tantivy)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "pos"))
        .unwrap();
    let mut bytes = fs::read(&positions).unwrap();
    bytes[0] ^= 1;
    fs::write(&positions, bytes).unwrap();
    let managed = tantivy.join(".managed.json");
    let inventory = fs::read_to_string(&managed).unwrap();
    let positions_name = positions.file_name().unwrap().to_str().unwrap();
    let damaged_inventory =
        inventory.replace(positions_name, &positions_name.replace(".pos", ".pad"));
    assert_ne!(inventory, damaged_inventory);
    assert_eq!(inventory.len(), damaged_inventory.len());
    fs::write(managed, damaged_inventory).unwrap();

    let repaired_postings = env.run_json(&["index"]);
    assert_eq!(repaired_postings["stats"]["docs"], 1);
    assert!(selected() > repaired_generation);
    assert_eq!(
        env.run_json(&["search", "unchangedcatalogwidget"])[0]["path"],
        "guide.md"
    );
}

fn seed_v5_registry(env: &Env) {
    let mut db = docsbase_memory::store::Db::open(env.cache()).unwrap();
    seed_v5_catalog(&mut db);
}

fn seed_v5_catalog(db: &mut docsbase_memory::store::Db) -> [u8; 16] {
    let epoch = [17; 16];
    // Fresh runtime initialization is covered separately. This seeds only the
    // empty catalog, so ordinary CLI commands must perform all writes.
    let tx = db.begin_write().unwrap();
    tx.execute_batch(include_str!("../src/store/generation_catalog/schema.sql"))
        .unwrap();
    tx.execute(
        "INSERT INTO generation_store VALUES (1, 1, ?1)",
        [epoch.as_slice()],
    )
    .unwrap();
    tx.pragma_update(None, "user_version", 5).unwrap();
    tx.commit().unwrap();

    epoch
}

#[cfg(unix)]
fn seed_published_v5_pair(
    env: &Env,
) -> docsbase_memory::store::generation_build::PreparedLexicalGeneration {
    use docsbase_memory::config::Config;
    use docsbase_memory::store::generation_build::{
        LexicalBuildRequest, prepare_lexical_generation, publish_lexical_generation,
    };
    use docsbase_memory::store::registry_inspect::InspectionLimits;
    use docsbase_memory::store::{Db, settings};

    fs::write(env.global_config(), "config_version = 2\n").unwrap();
    let db_settings = settings::standalone().unwrap();
    let config = Config::load_from(&env.cache().join("config"), Some(env.root())).unwrap();
    let mut db = Db::open_with_settings(env.cache(), &db_settings).unwrap();
    let project = docsbase_memory::daemon::registry::ensure_project(&mut db, env.root()).unwrap();
    let epoch = seed_v5_catalog(&mut db);
    drop(db);

    let request = LexicalBuildRequest {
        project_id: project.id,
        epoch,
        base: None,
        plan_digest: [13; 32],
        reserved_disk_bytes: 16 * 1024 * 1024,
    };
    let limits = InspectionLimits {
        max_projects: 8,
        max_metadata_bytes: 65_536,
        max_sqlite_value_bytes: 1_048_576,
        sqlite_cache_kib: 64,
    };
    let prepared =
        prepare_lexical_generation(env.cache(), request, &db_settings, limits, &config).unwrap();
    publish_lexical_generation(
        env.cache(),
        request,
        prepared,
        &db_settings,
        limits,
        &config,
    )
    .unwrap();
    prepared
}

#[cfg(unix)]
fn assert_published_search_refuses(env: &Env, expected: &str) {
    let output = env
        .cmd()
        .args(["search", "needlefrompublishedpair"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "search accepted inconsistent inputs"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected), "{stderr}");
}

#[test]
#[cfg(unix)]
fn direct_search_reads_the_published_v5_pair() {
    use docsbase_memory::store::DB_FILE;

    let env = Env::new(&[("guide.md", "# Guide\n\nneedlefrompublishedpair\n")]);
    let prepared = seed_published_v5_pair(&env);
    let hits = env.run_json(&["search", "needlefrompublishedpair", "--snippet"]);
    assert_eq!(hits[0]["path"], "guide.md");
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("needlefrompublishedpair")
    );
    let registry = rusqlite::Connection::open(env.cache().join(DB_FILE)).unwrap();
    assert_eq!(
        registry
            .query_row("SELECT count(*) FROM docs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(registry);

    fs::write(
        env.project_config(),
        "config_version = 2\n[index.chunk]\nmax_chars = 1000\n",
    )
    .unwrap();
    assert_published_search_refuses(&env, "index recipe");
    fs::remove_file(env.project_config()).unwrap();
    let seal = env
        .cache()
        .join("projects")
        .join(prepared.project_id.to_string())
        .join("generations")
        .join(prepared.generation_id.to_string())
        .join("seal.json");
    let valid_seal: Value = serde_json::from_slice(&fs::read(&seal).unwrap()).unwrap();
    let registry = rusqlite::Connection::open(env.cache().join(DB_FILE)).unwrap();
    for (field, wrong) in [
        ("format", serde_json::json!(2)),
        (
            "generation_id",
            serde_json::json!(prepared.generation_id + 1),
        ),
        (
            "content_revision",
            serde_json::json!(prepared.content_revision + 1),
        ),
        ("index_recipe", serde_json::json!("wrong recipe")),
    ] {
        let mut edited = valid_seal.clone();
        edited[field] = wrong;
        let bytes = serde_json::to_vec(&edited).unwrap();
        fs::write(&seal, &bytes).unwrap();
        registry
            .execute(
                "UPDATE project_generations SET seal_digest = ?1 WHERE id = ?2",
                rusqlite::params![
                    blake3::hash(&bytes).as_bytes().as_slice(),
                    prepared.generation_id
                ],
            )
            .unwrap();
        assert_published_search_refuses(&env, "seal");
    }
    drop(registry);

    fs::write(seal, "foreign seal").unwrap();
    assert_published_search_refuses(&env, "seal");
}

#[test]
#[cfg(all(unix, feature = "hybrid-remote"))]
fn stopped_search_checks_the_v5_embedding_recipe_before_constructing_provider() {
    let env = Env::new(&[("guide.md", "# Guide\n\nneedlefrompublishedpair\n")]);
    let _published = seed_published_v5_pair(&env);
    fs::write(
        env.project_config(),
        "config_version = 2\n[hybrid]\nenabled = true\nranking = 'rrf'\n\
         [hybrid.embedding]\nprovider = 'openrouter'\nmodel = 'missing-test-model'\ndimensions = 3\n",
    ).unwrap();
    assert_published_search_refuses(&env, "published embedding recipe");
    assert!(!env.cache().join("vectors.db").exists());
}

#[test]
fn direct_index_publishes_and_replaces_a_lexical_v5_pair() {
    let env = Env::new(&[("guide.md", "# Guide\n\nfirstgenerationpulsar\n")]);
    fs::write(env.global_config(), "config_version = 2\n").unwrap();
    seed_v5_registry(&env);

    let initial = env.run_json(&["index"]);
    assert_eq!(initial["stats"]["docs"], 1);
    let status = env.run_json(&["status"]);
    assert_eq!(status["schema_version"], 5);
    let project = &status["projects"][0];
    assert_eq!(project["status"], "indexed");
    assert_eq!(project["docs"], 1);
    assert_eq!(project["chunks"], 1);
    assert_eq!(project["ann"]["state"], "unknown");
    let list = env.run_json(&["list"]);
    assert_eq!(list["docs"][0]["path"], "guide.md");
    assert_eq!(list["docs"][0]["chunks"], 1);
    let hits = env.run_json(&["search", "firstgenerationpulsar", "--snippet"]);
    assert_eq!(hits[0]["path"], "guide.md");
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("firstgenerationpulsar")
    );

    let first_dir = env
        .cache()
        .join("projects")
        .join(initial["project_id"].as_i64().unwrap().to_string())
        .join("generations/1");
    let mut reader = fd_lock::RwLock::new(fs::File::open(first_dir.join(".read.lock")).unwrap());
    let lease = reader.try_read().unwrap();

    let doc = env.root().join("guide.md");
    fs::write(doc, "# Guide\n\nsecondgenerationquasar\n").unwrap();
    let updated = env.run_json(&["index"]);
    assert_eq!(updated["project_id"], initial["project_id"]);
    assert_eq!(updated["stats"]["docs"], 1);
    assert_eq!(
        env.run_json(&["search", "firstgenerationpulsar"]),
        serde_json::json!([])
    );
    let hits = env.run_json(&["search", "secondgenerationquasar", "--snippet"]);
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("secondgenerationquasar")
    );

    let registry =
        rusqlite::Connection::open(env.cache().join(docsbase_memory::store::DB_FILE)).unwrap();
    let (active, retired): (i64, i64) = registry
        .query_row(
            "SELECT active_id, (SELECT count(*) FROM project_generations WHERE phase = 'retired')
         FROM project_generation_head",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(active > 1);
    assert_eq!(retired, 1);
    assert!(first_dir.exists());
    assert_eq!(
        registry
            .query_row("SELECT count(*) FROM docs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(registry);
    drop(lease);
    assert!(reader.try_write().is_ok());
    drop(reader);
    let repeated = env.run_json(&["index"]);
    assert_eq!(repeated["stats"]["docs"], 0);
    assert_eq!(repeated["stats"]["skipped"], 1);
    assert!(!first_dir.exists());
    let registry =
        rusqlite::Connection::open(env.cache().join(docsbase_memory::store::DB_FILE)).unwrap();
    assert_eq!(
        registry
            .query_row(
                "SELECT count(*) FROM project_generations WHERE phase != 'active'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
#[cfg(unix)]
fn live_registry_read_keeps_its_snapshot_across_direct_v5_index() {
    use docsbase_memory::store::DB_FILE;
    use rusqlite::{Connection, OpenFlags};

    let env = Env::new(&[("guide.md", "# Guide\n\nprevioushelix\n")]);
    fs::write(env.global_config(), "config_version = 2\n").unwrap();
    seed_v5_registry(&env);
    let initial = env.run_json(&["index"]);

    // A second process can hold its registry snapshot while a private
    // generation is prepared and published.
    let reader =
        Connection::open_with_flags(env.cache().join(DB_FILE), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let active = |conn: &Connection| {
        conn.query_row("SELECT active_id FROM project_generation_head", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    };
    let previous = active(&reader);
    fs::write(env.root().join("guide.md"), "# Guide\n\npublishedspiral\n").unwrap();
    assert_eq!(
        env.run_json(&["index"])["project_id"],
        initial["project_id"]
    );
    assert_eq!(
        active(&reader),
        previous,
        "open transaction sees its old head"
    );
    assert_eq!(
        env.run_json(&["search", "publishedspiral"])[0]["path"],
        "guide.md"
    );
    let current = Connection::open(env.cache().join(DB_FILE)).unwrap();
    assert!(active(&current) > previous);
}

#[test]
#[cfg(unix)]
fn failed_v5_index_keeps_the_active_pair_and_is_retryable() {
    let env = Env::new(&[("guide.md", "# Guide\n\nlastgoodpulsar\n")]);
    let default = "config_version = 2\n";
    fs::write(env.global_config(), default).unwrap();
    seed_v5_registry(&env);
    let indexed = env.run_json(&["index"]);
    assert_eq!(indexed["stats"]["docs"], 1);

    fs::write(
        env.root().join("guide.md"),
        "# Guide\n\nreplacementquasar\n",
    )
    .unwrap();
    fs::write(
        env.global_config(),
        "config_version = 2\n[index.writer]\nstaging_disk_bytes = 4096\n",
    )
    .unwrap();
    let limited = env.cmd().arg("index").output().unwrap();
    assert!(!limited.status.success());
    let hits = env.run_json(&["search", "lastgoodpulsar", "--snippet"]);
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("lastgoodpulsar")
    );

    fs::write(
        env.global_config(),
        "config_version = 2\n[search]\nmax_request_payload_bytes = 32768\n",
    )
    .unwrap();
    let limited = env.cmd().arg("index").output().unwrap();
    assert!(!limited.status.success());
    let stderr = String::from_utf8_lossy(&limited.stderr);
    assert!(stderr.contains("max_request_payload_bytes"), "{stderr}");

    fs::write(env.global_config(), default).unwrap();
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 1);
    assert_eq!(
        env.run_json(&["search", "replacementquasar"])[0]["path"],
        "guide.md"
    );
    let registry =
        rusqlite::Connection::open(env.cache().join(docsbase_memory::store::DB_FILE)).unwrap();
    let unfinished = registry.query_row(
        "SELECT count(*) FROM project_generations WHERE phase IN ('building', 'sealed', 'reclaiming')",
        [],
        |row| row.get::<_, i64>(0),
    ).unwrap();
    assert_eq!(unfinished, 0);
}

#[test]
fn writer_policy_change_preserves_existing_index_and_applies_to_connected_jobs() {
    let mut env = Env::new(&[("guide.md", &doc("writerconfigurationwidget"))]);
    fs::write(
        env.global_config(),
        "config_version = 2\n[index.writer]\nheap_bytes = 30000000\nworker_threads = 2\nmerge_threads = 1\n",
    )
    .unwrap();
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 1);
    let query = ["search", "writerconfigurationwidget", "--explain"];
    let initial = env.run_json(&query);
    let generation = initial["meta"]["index_generation"].as_i64().unwrap();

    fs::write(
        env.global_config(),
        "config_version = 2\n[index.writer]\nheap_bytes = 16000000\nmerge_threads = 2\n",
    )
    .unwrap();
    env.start_daemon();
    let repeated = env.run_json(&["index"]);
    assert_eq!(repeated["stats"]["docs"], 0);
    assert_eq!(repeated["stats"]["skipped"], 1);
    let after = env.run_json(&query);
    let hits_by_id = |result: &Value| {
        let mut hits = result["hits"].as_array().unwrap().clone();
        let scores: Vec<_> = hits.iter().map(|hit| hit["score"].clone()).collect();
        // Tantivy may change equal-score order when segments merge.
        hits.sort_by_key(|hit| hit["chunk_id"].as_u64().unwrap());
        (scores, hits)
    };
    assert_eq!(hits_by_id(&after), hits_by_id(&initial));
    assert_eq!(after["meta"]["index_generation"], generation);
}

#[test]
fn v2_cli_index_uses_the_live_owner_and_returns_index_counters() {
    let mut env = Env::new(&[
        ("guide.md", "connectedindexwidget"),
        ("guide/other.md", "connectedindexnewpage"),
    ]);
    fs::write(env.global_config(), "config_version = 2\n").unwrap();
    env.start_daemon();
    assert_eq!(env.run_json(&["status"])["schema_version"], 5);

    let first = env.cmd().arg("index").output().unwrap();
    assert!(first.status.success(), "{first:?}");
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["stats"]["docs"], 2);
    assert!(first["project_id"].is_i64());

    let nested = env.root().join("guide");
    let repeated = env
        .cmd()
        .current_dir(&nested)
        .args(["index", ".."])
        .output()
        .unwrap();
    assert!(repeated.status.success(), "{repeated:?}");
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated["project_id"], first["project_id"]);
    assert_eq!(repeated["stats"]["docs"], 0);
    assert_eq!(repeated["stats"]["skipped"], 2);
    assert!(repeated.get("job_id").is_none());

    let search = env
        .cmd()
        .args(["search", "connectedindexnewpage"])
        .output()
        .unwrap();
    assert!(search.status.success(), "{search:?}");
    let hits: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(hits[0]["path"], "guide/other.md");
}

#[test]
fn connected_v2_index_reloads_the_current_project_config() {
    let mut env = Env::new(&[("guide.md", "connectedindexwidget")]);
    fs::write(env.global_config(), "config_version = 2\n").unwrap();
    env.start_daemon();
    let initial = env.cmd().arg("index").output().unwrap();
    assert!(initial.status.success(), "{initial:?}");

    fs::write(env.project_config(), "[hybrid.cache]\nmax_bytes = 4096\n").unwrap();
    let mut registry = env.client();
    registry.handshake_registry().unwrap();
    assert_eq!(index(&mut registry, &env)["state"], "done");
    let rejected = env.cmd().arg("index").output().unwrap();
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("hybrid.cache.max_bytes"));
}

#[test]
fn missing_global_file_uses_v2_in_direct_cli_and_daemon() {
    let mut env = Env::new(&[("guide.md", "configurationwidget")]);
    let out = env.cmd().args(["config", "effective"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(view["fields"].as_array().unwrap().iter().any(|field| {
        field["key"] == "vector_cache.max_bytes" && field["source"]["kind"] == "defaults"
    }));

    let search_guide = |env: &Env| {
        let result = env
            .cmd()
            .args(["search", "configurationwidget"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let hits: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(hits[0]["path"], "guide.md");
    };
    let direct = env.cmd().arg("index").output().unwrap();
    assert!(direct.status.success(), "{direct:?}");
    assert!(!env.cache().join("vectors.db").exists());

    search_guide(&env);
    env.start_daemon();
    let another = tempfile::tempdir().unwrap();
    write_file(another.path(), "guide.md", b"connectedconfigurationwidget");
    let indexed = env.cmd().arg("index").arg(another.path()).output().unwrap();
    assert!(indexed.status.success(), "{indexed:?}");
    let indexed: Value = serde_json::from_slice(&indexed.stdout).unwrap();
    assert_eq!(indexed["stats"]["docs"], 1);
    let connected_search = env
        .cmd()
        .current_dir(another.path())
        .args(["search", "connectedconfigurationwidget"])
        .output()
        .unwrap();
    assert!(connected_search.status.success(), "{connected_search:?}");
    let hits: Value = serde_json::from_slice(&connected_search.stdout).unwrap();
    assert_eq!(hits[0]["path"], "guide.md");

    let out = env.cmd().arg("sync").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let job: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(job["state"], "done");
    search_guide(&env);
    assert!(!env.global_config().exists());
    assert!(!env.cache().join("vectors.db").exists());
    assert!(matches!(
        docsbase_memory::vector_cache::VectorCache::open(env.cache()),
        Err(Error::Admission { .. })
    ));
}

#[test]
fn changed_config_version_refuses_cli_operations_against_stale_daemon() {
    let mut env = Env::new(&[("guide.md", &doc("quasar telemetry calibration"))]);
    fs::write(env.global_config(), "# Existing unversioned installation").unwrap();
    let out = env.cmd().arg("index").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(env.run_json(&["status"])["schema_version"], 4);
    env.start_daemon();
    write_file(
        env.cache(),
        "config/config.toml",
        b"config_version=2
",
    );
    let expect_refusal = |env: &Env| {
        for args in [&["search", "quasar"][..], &["index"], &["list"], &["sync"]] {
            let out = env.cmd().args(args).output().unwrap();
            assert!(
                !out.status.success(),
                "expected stale daemon refusal for {args:?}"
            );
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("daemon is using a different config version")
            );
        }
    };
    expect_refusal(&env);
    env.restart_daemon();
    assert_eq!(env.run_json(&["status"])["schema_version"], 4);
    let out = env.cmd().args(["search", "quasar"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()[0]["path"],
        "guide.md"
    );
    fs::write(env.global_config(), "# Existing unversioned installation").unwrap();
    expect_refusal(&env);
}

#[test]
fn invalid_v2_global_fails_before_creating_daemon_state() {
    let oversized = format!(
        "config_version=2\n#{}",
        " ".repeat(usize::try_from(docsbase_memory::limits::MAX_STATE_BYTES).unwrap()),
    );
    for (source, error) in [
        (
            "config_version=2\n[vector_cache]\nmax_bytes=0\n",
            "vector_cache.max_bytes",
        ),
        (
            "# start here\nconfig_version=2\n[vector_cache",
            "invalid versioned configuration TOML",
        ),
        (
            "config_version=2
[store]
write_busy_timeout=2147483648
",
            "store.write_busy_timeout exceeds SQLite busy_timeout",
        ),
        (
            "config_version=2
[read_pool]
max_connections=0
",
            "read_pool.max_connections must be > 0",
        ),
        (
            "config_version=2\n[dense]\nquery_cache_max_entries=0\n",
            "query-cache entry and byte limits must be positive",
        ),
        (
            "config_version=2\n[index.writer]\nheap_bytes=14999999\n",
            "index.writer.heap_bytes",
        ),
        (&oversized, "over the trusted byte limit"),
    ] {
        let env = Env::new(&[]);
        write_file(env.cache(), "config/config.toml", source.as_bytes());
        let out = env
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "1"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(error));
        assert!(!env.cache().join("state").exists());
        assert!(!env.cache().join(docsbase_memory::store::DB_FILE).exists());
    }
}

#[test]
fn stopped_diagnostics_keep_standalone_store_policy_and_website_routing() {
    let env = Env::new(&[("guide.md", &doc("quasar telemetry calibration"))]);
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 1);

    fs::write(
        env.global_config(),
        "config_version = 2\n[store]\nwrite_busy_timeout = -1\n[read_pool]\nmax_connections = 0\n",
    )
    .unwrap();
    assert_eq!(
        env.run_json(&["status"])["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(env.run_json(&["list"])["docs"][0]["path"], "guide.md");

    let root = toml::Value::String(
        env.root()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    fs::write(
        env.global_config(),
        format!("config_version = 2\n[daemon]\nprofile = {{ mode = 'website', project_root = {root} }}\n[read_pool]\nmax_connections = 0\n"),
    )
    .unwrap();
    for command in ["status", "list"] {
        let output = env.cmd().arg(command).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("website daemon unavailable"));
    }
}

#[test]
fn v2_lexical_weights_change_real_search_without_reindexing() {
    let title_matches = "# lexicalweightprobe\n\nThis prose has no search term.\n\n".repeat(24);
    let mut env = Env::new(&[
        ("manual/title.md", &title_matches),
        (
            "manual/body.md",
            "# Example\n\nThe prose contains lexicalweightprobe.\n",
        ),
    ]);
    fs::write(
        env.global_config(),
        "config_version = 2\n[lexical]\ntitle_boost = 4.0\ntext_boost = 1.0\nheading_boost = 0.0\nidentifiers_boost = 0.0\n",
    )
    .unwrap();
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 2);
    let query = [
        "search",
        "lexicalweightprobe",
        "--explain",
        "--path-prefix",
        "manual/",
    ];
    let baseline = env.run_json(&query);
    assert_eq!(baseline["hits"][0]["path"], "manual/title.md");
    let generation = baseline["meta"]["index_generation"].as_i64().unwrap();
    let refill = env.run_json(&[
        "search",
        "lexicalweightprobe",
        "--explain",
        "--limit",
        "2",
        "--dedupe",
        "path",
        "--n-per-doc",
        "1",
    ]);
    assert!(refill["meta"]["refill"]["rounds"].as_u64().unwrap() > 1);
    assert_eq!(refill["hits"][0]["path"], "manual/title.md");
    assert_eq!(refill["hits"][0]["score"], baseline["hits"][0]["score"]);
    assert_eq!(refill["hits"][1]["path"], "manual/body.md");

    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\ntitle_boost = 0.0\n",
    )
    .unwrap();
    let direct = env.run_json(&query);
    assert_eq!(direct["hits"][0]["path"], "manual/body.md");
    assert_eq!(direct["meta"]["index_generation"], generation);

    env.start_daemon();
    let from_daemon = env.run_json(&query);
    assert_eq!(from_daemon["hits"], direct["hits"]);
    assert_eq!(from_daemon["meta"]["index_generation"], generation);

    let effective = env.run_json(&["config", "effective"]);
    let fields = effective["fields"].as_array().unwrap();
    let field = |key| fields.iter().find(|entry| entry["key"] == key).unwrap();
    assert_eq!(field("lexical.title_boost")["value"], 0.0);
    assert_eq!(field("lexical.title_boost")["source"]["kind"], "project");
    assert_eq!(field("lexical.text_boost")["value"], 1.0);
    assert_eq!(field("lexical.text_boost")["source"]["kind"], "global");

    fs::write(env.project_config(), "config_version = 2\n").unwrap();
    let reopened = env.run_json(&query);
    assert_eq!(reopened["hits"], baseline["hits"]);
    assert_eq!(reopened["meta"]["index_generation"], generation);

    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\ntitle_boost = 0.0\n",
    )
    .unwrap();
    let index = env.run_json(&["index"]);
    assert_eq!(index["stats"]["docs"], 0);
    assert_eq!(index["stats"]["skipped"], 2);
    assert_eq!(env.run_json(&query)["hits"], direct["hits"]);
}

#[test]
fn v2_retrieval_overscan_reaches_direct_and_daemon_cap_refill() {
    let dominant = "# Widgets\n\noverscanprobe overscanprobe overscanprobe\n\n".repeat(40);
    let mut env = Env::new(&[
        ("dominant.md", &dominant),
        ("answer.md", "# Answer\n\nSet overscanprobe here.\n"),
    ]);
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 2);
    let query = [
        "search",
        "overscanprobe",
        "--explain",
        "--limit",
        "2",
        "--dedupe",
        "path",
        "--n-per-doc",
        "1",
    ];
    let default = env.run_json(&query);
    assert_eq!(default["meta"]["window"], 128);
    assert_eq!(default["meta"]["refill"]["rounds"], 3);
    assert_eq!(default["meta"]["refill"]["stop_reason"], "satisfied");
    assert_eq!(default["hits"][0]["path"], "dominant.md");
    assert_eq!(default["hits"][1]["path"], "answer.md");

    fs::write(
        env.global_config(),
        "config_version = 2\n[retrieval]\npost_filter_overscan = 3\n",
    )
    .unwrap();
    let inherited = env.run_json(&query);
    assert_eq!(inherited["meta"]["window"], 54);

    fs::write(
        env.project_config(),
        "config_version = 2\n[retrieval]\npost_filter_overscan = 1\n",
    )
    .unwrap();
    let direct = env.run_json(&query);
    assert_eq!(direct["meta"]["window"], 64);
    assert_eq!(direct["meta"]["refill"]["rounds"], 6);
    assert_eq!(direct["meta"]["refill"]["stop_reason"], "satisfied");
    assert_eq!(direct["hits"], default["hits"]);
    assert_eq!(
        direct["meta"]["index_generation"],
        default["meta"]["index_generation"]
    );

    env.start_daemon();
    let daemon = env.run_json(&query);
    assert_eq!(daemon["hits"], direct["hits"]);
    assert_eq!(daemon["meta"]["window"], 64);
    assert_eq!(daemon["meta"]["refill"], direct["meta"]["refill"]);

    let effective = env.run_json(&["config", "effective"]);
    let field = effective["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "retrieval.post_filter_overscan")
        .unwrap();
    assert_eq!(field["value"], 1);
    assert_eq!(field["source"]["kind"], "project");
    assert_eq!(field["scope"], "project");
    assert_eq!(field["unit"], "multiplier");

    fs::write(
        env.project_config(),
        "config_version = 2\n[retrieval]\npost_filter_overscan = 1000\n",
    )
    .unwrap();
    let bounded = env.run_json(&query);
    assert_eq!(bounded["meta"]["window"], 1000);
    assert_eq!(bounded["meta"]["refill"]["rounds"], 1);
    assert_eq!(bounded["hits"], direct["hits"]);
    let uncapped = env.run_json(&[
        "search",
        "overscanprobe",
        "--explain",
        "--limit",
        "2",
        "--path-prefix",
        "answer",
    ]);
    assert_eq!(uncapped["meta"]["window"], 2);
    assert_eq!(uncapped["hits"][0]["path"], "answer.md");
}

#[test]
fn v2_lexical_collection_applies_a_query_only_penalty() {
    let long = format!(
        "# Example\n\n```text\nlexicalcollectionprobe lexicalcollectionprobe {}\n```\n",
        "x".repeat(3_000)
    );
    let mut env = Env::new(&[
        ("long.md", &long),
        (
            "short.md",
            "# Example\n\nlexicalcollectionprobe filler1 filler2.\n",
        ),
    ]);
    fs::write(
        env.global_config(),
        "config_version = 2\n[lexical]\nidentifiers_boost = 0.0\nlong_chunk_penalty = 1.0\ninitial_fetch_multiplier = 1\ninitial_fetch_additive = 0\nwidening_multiplier = 2\n",
    )
    .unwrap();
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 2);
    let query = [
        "search",
        "lexicalcollectionprobe",
        "--limit",
        "1",
        "--explain",
    ];
    let unpenalized = env.run_json(&query);
    assert_eq!(unpenalized["hits"][0]["path"], "long.md");
    let generation = &unpenalized["meta"]["index_generation"];

    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\nlong_chunk_penalty = 0.125\n",
    )
    .unwrap();
    let direct = env.run_json(&query);
    assert_eq!(direct["hits"][0]["path"], "short.md");
    assert_eq!(&direct["meta"]["index_generation"], generation);

    env.start_daemon();
    let daemon = env.run_json(&query);
    assert_eq!(daemon["hits"], direct["hits"]);
    assert_eq!(&daemon["meta"]["index_generation"], generation);

    let effective = env.run_json(&["config", "effective"]);
    let fields = effective["fields"].as_array().unwrap();
    let field = |key| fields.iter().find(|entry| entry["key"] == key).unwrap();
    assert_eq!(field("lexical.long_chunk_penalty")["value"], 0.125);
    assert_eq!(
        field("lexical.long_chunk_penalty")["source"]["kind"],
        "project"
    );
    assert_eq!(field("lexical.initial_fetch_multiplier")["value"], 1);
    assert_eq!(field("lexical.initial_fetch_additive")["value"], 0);
    assert_eq!(field("lexical.widening_multiplier")["value"], 2);
    assert_eq!(
        field("lexical.widening_multiplier")["source"]["kind"],
        "global"
    );

    fs::write(env.project_config(), "config_version = 2\n").unwrap();
    let reopened = env.run_json(&query);
    assert_eq!(reopened["hits"], unpenalized["hits"]);
    assert_eq!(&reopened["meta"]["index_generation"], generation);
}

#[test]
fn v2_lexical_weight_overflow_returns_a_query_error() {
    let env = Env::new(&[("needle.md", "# Guide\n\ninterestingweightword")]);
    for i in 0..20 {
        write_file(
            env.root(),
            &format!("unrelated{i}.md"),
            b"# Guide\n\nSome unrelated text.",
        );
    }
    fs::write(
        env.global_config(),
        "config_version = 2\n[lexical]\ntext_boost = 3.0e38\n",
    )
    .unwrap();
    assert_eq!(env.run_json(&["index"])["stats"]["docs"], 21);

    let out = env
        .cmd()
        .args(["search", "interestingweightword"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-finite lexical score"));

    fs::write(
        env.project_config(),
        "config_version = 2\n[lexical]\ntext_boost = 0.0\n",
    )
    .unwrap();
    for query in [
        "text:interestingweightword^400000000000000000000000000000000000000",
        "(text:interestingweightword^100000000000000000000)^100000000000000000000",
    ] {
        let out = env.cmd().args(["search", query]).output().unwrap();
        assert!(!out.status.success(), "{query}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("non-finite lexical boost"));
    }
}
