#[path = "support/search_visibility.rs"]
mod search_visibility;

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::daemon::admission::Lease;
use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::index::tantivy_index::ReadIndex;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{
    PROTOCOL_VERSION, Request, Response, build_id, decode_response, encode,
};
use docsbase_memory::platform;
use docsbase_memory::store::migrations::SCHEMA_VERSION;
use docsbase_memory::store::repo::{self, DocState};

struct Env {
    cache: TempDir,
    root: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
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

    fn index(&self) {
        let output = self
            .cmd()
            .args(["index"])
            .arg(self.root())
            .output()
            .expect("run index");
        assert!(output.status.success(), "index failed: {output:?}");
    }

    fn start_daemon(&mut self) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "3000"])
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

    fn client(&self) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect(self.cache()) {
                return client;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not connectable");
    }

    fn write_state(&self, build: &str, schema: u32) {
        let state = json!({
            "pid": 999_999,
            "socket": platform::daemon_endpoint(self.cache()),
            "build_id": build,
            "schema_version": schema,
            "cache_root": self.cache(),
        });
        let state_dir = self.cache().join("state");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::write(
            state_dir.join("daemon.json"),
            serde_json::to_vec(&state).expect("json"),
        )
        .expect("write state");
    }

    fn conflicts(&self) -> Vec<Value> {
        let path = self.cache().join("logs/conflicts.ndjson");
        let text = fs::read_to_string(path).expect("conflict log");
        text.lines()
            .map(|line| serde_json::from_str(line).expect("conflict line"))
            .collect()
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

const DOC: &str = "# Guide\n\ninstaller prose about widgets.\n";

// A failure watchdog, not an expected processing time or a mandatory delay.
const WATCHER_WATCHDOG: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
struct IndexedState {
    generation: i64,
    docs: Vec<DocState>,
    chunks: Vec<String>,
}

fn indexed_state(conn: &rusqlite::Connection, project_id: i64) -> IndexedState {
    IndexedState {
        generation: repo::index_generation(conn, project_id).expect("generation"),
        docs: repo::doc_states(conn, project_id).expect("document state"),
        chunks: repo::chunk_texts(conn, project_id).expect("chunk texts"),
    }
}

fn assert_stale_state(conn: &rusqlite::Connection, project_id: i64, expected: &IndexedState) {
    let schema: u32 = conn
        .query_row(
            "SELECT schema_version FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .expect("schema stamp");
    assert_eq!(schema, 0, "watcher must not restamp a stale schema");
    assert_eq!(
        &indexed_state(conn, project_id),
        expected,
        "stale watcher must not change indexed data"
    );
}

fn wait_for_schema_refusals(
    conn: &rusqlite::Connection,
    project_id: i64,
    after_job: i64,
    changed: &[PathBuf],
    expected: &IndexedState,
) {
    let wanted: HashSet<PathBuf> = changed
        .iter()
        .map(|path| path.canonicalize().expect("canonical changed path"))
        .collect();
    let mut seen = HashSet::new();
    let mut failures = Vec::new();
    let observed = wait_until(WATCHER_WATCHDOG, || {
        assert_stale_state(conn, project_id, expected);
        let mut query = conn
            .prepare(
                "SELECT stats_json FROM sync_jobs
             WHERE project_id = ?1 AND id > ?2 AND state = 'error'
               AND finished_at IS NOT NULL ORDER BY id",
            )
            .expect("prepare failed-batch query");
        failures = query
            .query_map([project_id, after_job], |row| row.get::<_, String>(0))
            .expect("query failed batches")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed-batch rows");
        for raw in &failures {
            let stats: Value = serde_json::from_str(raw).expect("failed-batch stats");
            for warning in stats["warnings"].as_array().expect("batch paths") {
                let path = Path::new(warning["path"].as_str().expect("warning path"));
                if let Ok(path) = path.canonicalize()
                    && wanted.contains(&path)
                {
                    let error = stats["error"].as_str().expect("batch error");
                    assert!(
                        error.contains("full rebuild") && error.contains("docsbase index"),
                        "watcher refusal must explain the rebuild: {stats}"
                    );
                    seen.insert(path);
                }
            }
        }
        seen == wanted
    });
    assert!(
        observed,
        "watcher did not acknowledge refused changes: wanted={wanted:?}, seen={seen:?}, jobs={failures:?}"
    );
    assert_stale_state(conn, project_id, expected);
}

#[test]
fn mismatch_message_actionable() {
    let env = Env::new(&[("README.md", DOC)]);
    env.write_state("docsbase 0.0.0-old", 0);

    let output = env
        .cmd()
        .args(["serve", "--detached"])
        .output()
        .expect("run serve");
    assert!(!output.status.success(), "mismatched serve must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("docsbase install"),
        "message must name the install command: {stderr}"
    );
    assert!(
        stderr.contains("docsbase index"),
        "message must hint at the index rebuild: {stderr}"
    );
    let conflicts = env.conflicts();
    let last = conflicts.last().expect("conflict recorded");
    assert_eq!(last["kind"], "build_mismatch");
    assert_eq!(last["expected"], build_id());
    assert_eq!(last["actual"], "docsbase 0.0.0-old");
    assert_eq!(last["recorded_build_id"], "docsbase 0.0.0-old");
    assert_eq!(last["recorded_schema_version"], 0);

    // Same build, stale schema: the record must carry both schema values.
    let env_schema = Env::new(&[("README.md", DOC)]);
    env_schema.write_state(&build_id(), 0);
    let output = env_schema
        .cmd()
        .args(["serve", "--detached"])
        .output()
        .expect("run serve");
    assert!(!output.status.success(), "stale schema must refuse");
    let last = env_schema.conflicts();
    let last = last.last().expect("conflict recorded");
    assert_eq!(last["kind"], "schema_mismatch");
    assert_eq!(last["expected"], SCHEMA_VERSION.to_string());
    assert_eq!(last["actual"], "0");
    assert_eq!(last["recorded_schema_version"], 0);

    // A live daemon refuses a foreign build with an actionable message too.
    let mut env2 = Env::new(&[("README.md", DOC)]);
    env2.start_daemon();
    let mut stream =
        platform::connect_blocking(&platform::daemon_endpoint(env2.cache())).expect("connect");
    let request = Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        build_id: "docsbase 0.0.0-foreign".to_owned(),
        client: "admission-test".to_owned(),
    };
    stream
        .write_all(&encode(&request).expect("encode"))
        .expect("write");
    stream.flush().expect("flush");
    let mut line = Vec::new();
    BufReader::new(stream.try_clone().expect("clone"))
        .read_until(b'\n', &mut line)
        .expect("read");
    let response = decode_response(&line).expect("decode");
    let Response::Error { code, message, .. } = response else {
        panic!("unexpected hello response: {response:?}");
    };
    assert_eq!(code, -32010, "admission code: {message}");
    assert!(
        message.contains("docsbase install"),
        "hello mismatch must be actionable: {message}"
    );
    let conflicts = env2.conflicts();
    let last = conflicts.last().expect("hello mismatch logged");
    assert_eq!(last["kind"], "hello_build_mismatch");
    assert_eq!(last["actual"], "docsbase 0.0.0-foreign");
    assert_eq!(last["build_id"], "docsbase 0.0.0-foreign");
}

#[test]
fn conflict_log_fields() {
    let env = Env::new(&[("README.md", DOC)]);
    let cache = env.cache().to_path_buf();
    let _first = Lease::acquire(&cache, &build_id(), SCHEMA_VERSION).expect("first lease");
    let err = Lease::acquire(&cache, &build_id(), SCHEMA_VERSION).expect_err("lock is busy");
    assert!(
        err.to_string().contains("docsbase daemon stop"),
        "lock_busy must be actionable: {err}"
    );

    let conflicts = env.conflicts();
    let last = conflicts.last().expect("conflict recorded");
    assert_eq!(last["kind"], "lock_busy");
    assert_eq!(last["build_id"], build_id());
    assert_eq!(last["schema_version"], SCHEMA_VERSION);
    assert_eq!(last["cache_root"], json!(cache));
    assert_eq!(last["pid"], std::process::id());
    assert!(last["ts"].is_i64());
}

#[test]
fn rebuild_hint_on_schema_bump() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).expect("bind session");

    let conn = rusqlite::Connection::open(env.cache().join("registry.db")).expect("open db");
    conn.execute("UPDATE projects SET schema_version = 0", [])
        .expect("downgrade schema stamp");
    drop(conn);

    let err = client
        .call_tool("search_docs", json!({ "query": "widgets" }))
        .expect_err("stale schema must be refused");
    let message = err.to_string();
    assert!(
        message.contains("schema 0") && message.contains("docsbase index"),
        "rebuild hint missing: {message}"
    );

    client
        .call_tool(
            "index_project",
            json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect("rebuild via index_project");

    let conn = rusqlite::Connection::open(env.cache().join("registry.db")).expect("open db");
    let schema: u32 = conn
        .query_row("SELECT schema_version FROM projects", [], |row| row.get(0))
        .expect("schema stamp");
    assert_eq!(schema, SCHEMA_VERSION);

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let hits = bound
        .call_tool("search_docs", json!({ "query": "widgets" }))
        .expect("search after rebuild");
    assert_ne!(hits.as_array().expect("hits").len(), 0);
}

#[test]
fn stale_schema_survives_file_edit() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).expect("bind session");
    let conn = rusqlite::Connection::open(env.cache().join("registry.db")).expect("open db");
    let project_id: i64 = conn
        .query_row("SELECT id FROM projects", [], |row| row.get(0))
        .expect("fixture project");
    let initial = indexed_state(&conn, project_id);
    let hits = client
        .call_tool("search_docs", json!({ "query": "watcherready" }))
        .expect("initial search control");
    assert!(hits.as_array().expect("hits").is_empty());

    // Both committed data and search must show a real healthy watcher edit.
    write_file(
        env.root(),
        "README.md",
        b"# Guide\n\nwatcherready widgets.\n",
    );
    assert!(
        wait_until(WATCHER_WATCHDOG, || {
            let state = indexed_state(&conn, project_id);
            if state.generation <= initial.generation
                || !state
                    .chunks
                    .iter()
                    .any(|text| text.contains("watcherready"))
            {
                return false;
            }
            match client.call_tool("search_docs", json!({ "query": "watcherready" })) {
                Ok(hits) => !hits.as_array().expect("hits").is_empty(),
                Err(err) if search_visibility::temporarily_unavailable(&err) => false,
                Err(err) => panic!("healthy watcher search failed: {err}"),
            }
        }),
        "healthy watcher edit must reach committed data and search"
    );
    let before = indexed_state(&conn, project_id);
    assert_eq!(before.docs.len(), 1);
    let witness = env.root().join("stale-edit-witness.md");
    assert!(
        !witness.exists(),
        "witness cannot be an earlier pending path"
    );
    let after_job: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(id), 0) FROM sync_jobs WHERE project_id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .expect("job cursor");
    conn.execute(
        "UPDATE projects SET schema_version = 0 WHERE id = ?1",
        [project_id],
    )
    .expect("downgrade schema stamp");

    write_file(
        env.root(),
        "README.md",
        b"# Guide\n\ninstaller prose about gizmos.\n",
    );
    write_file(
        env.root(),
        "stale-edit-witness.md",
        b"# Witness\n\ngizmos after downgrade.\n",
    );
    // Canonical paths and new terminal jobs correlate consumer progress. The
    // backend may coalesce these paths into one batch or deliver them apart.
    wait_for_schema_refusals(
        &conn,
        project_id,
        after_job,
        &[env.root().join("README.md"), witness],
        &before,
    );
    let err = client
        .call_tool("search_docs", json!({ "query": "gizmos" }))
        .expect_err("stale schema must stay refused after confirmed watcher work");
    assert!(
        err.to_string().contains("docsbase index"),
        "rebuild hint missing: {err}"
    );
    // Open after the refusal: an earlier reader could hide an unwanted commit.
    let index = ReadIndex::open(&env.cache().join(format!("projects/{project_id}/tantivy")))
        .expect("fresh physical index");
    assert!(
        !index
            .search("watcherready", 10)
            .expect("healthy content")
            .is_empty()
    );
    assert!(
        index
            .search("gizmos", 10)
            .expect("refused content")
            .is_empty(),
        "stale edits must not appear in the physical index"
    );
}

#[test]
fn newer_db_schema_reports_install() {
    let env = Env::new(&[("README.md", DOC)]);
    let _db = docsbase_memory::store::Db::open(env.cache()).expect("create db");
    let conn = rusqlite::Connection::open(env.cache().join("registry.db")).expect("open db");
    conn.pragma_update(None, "user_version", 99)
        .expect("bump schema");
    drop(conn);

    let output = env.cmd().args(["status"]).output().expect("run status");
    assert!(!output.status.success(), "newer schema must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("docsbase install"),
        "newer schema needs the install hint: {stderr}"
    );
    let conflicts = env.conflicts();
    let last = conflicts.last().expect("conflict recorded");
    assert_eq!(last["kind"], "db_schema_mismatch");
    assert_eq!(last["recorded_schema_version"], 99);
}
