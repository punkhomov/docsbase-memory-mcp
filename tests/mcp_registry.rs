use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, build_id};
use docsbase_memory::platform;

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

    fn start_daemon(&mut self) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .spawn()
            .expect("spawn daemon");
        if let Some(mut previous) = self.daemon.replace(child) {
            let _ = previous.wait();
        }
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
            if let Some(client) = Client::connect(self.cache()) {
                return client;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not connectable");
    }

    fn index_project(client: &mut Client, path: &Path) -> Value {
        client
            .call_tool(
                "index_project",
                json!({ "path": path.to_str().expect("utf-8 path") }),
            )
            .expect("index_project")
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
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn wait_job_done(client: &mut Client, job_id: i64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let value = client
            .call_tool("sync_status", json!({ "job_id": job_id }))
            .expect("sync_status");
        let state = value["state"].as_str().expect("state");
        if state == "done" || state == "error" {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} did not finish: {value}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

const DOC: &str = "---\ntitle: Guide\n---\n\n# Install\n\ninstaller prose about widgets.\n";

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(env: &Env) -> Self {
        let mut child = env
            .cmd()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, message: &Value) -> Value {
        writeln!(self.stdin, "{message}").expect("write");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).expect("read response");
        assert!(read > 0, "server closed stdout");
        serde_json::from_str(&line).expect("json-rpc line")
    }

    fn notify(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn initialize(&mut self) {
        let response = self.request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }));
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            "docsbase-memory-mcp"
        );
        self.notify(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "test helper mirrors the MCP call shape"
    )]
    fn call_tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.request(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn corpus(files: usize, sections: usize) -> Vec<(String, String)> {
    (0..files)
        .map(|file| {
            let mut body = String::from("# Big document\n\nintro prose about widgets\n");
            for section in 0..sections {
                let _ = writeln!(
                    body,
                    "\n## Section {section}\n\ninstaller prose with widget-{section} and gadget guidance."
                );
            }
            (format!("docs/file-{file}.md"), body)
        })
        .collect()
}

#[test]
fn index_project_registers_and_indexes() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");

    let value = Env::index_project(&mut client, env.root());
    assert!(value["job_id"].as_i64().is_some(), "job id: {value}");
    assert_eq!(value["state"], "done", "job state: {value}");
    assert!(
        value["stats"]["docs"].as_u64().unwrap_or(0) >= 1,
        "stats: {value}"
    );

    let projects = client
        .call_tool("list_projects", json!({}))
        .expect("list_projects");
    let projects = projects.as_array().expect("projects array");
    assert_eq!(projects.len(), 1, "projects: {projects:?}");
    assert_eq!(projects[0]["status"], "indexed");

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let hits = bound
        .call_tool("search_docs", json!({ "query": "widgets" }))
        .expect("search_docs");
    assert!(
        !hits.as_array().expect("hits array").is_empty(),
        "hits: {hits}"
    );
}

#[test]
fn list_projects_statuses() {
    let mut env = Env::new(&[("README.md", DOC), ("docs/guide.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");

    let canonical = fs::canonicalize(env.root()).expect("canonical root");
    let projects = client
        .call_tool("list_projects", json!({}))
        .expect("list_projects");
    assert_eq!(projects.as_array().expect("array").len(), 0);

    Env::index_project(&mut client, env.root());
    let projects = client
        .call_tool("list_projects", json!({}))
        .expect("list_projects");
    let project = &projects.as_array().expect("array")[0];
    assert_eq!(project["status"], "indexed");
    assert_eq!(project["root"], json!(canonical));
    assert_eq!(project["docs"], 2, "project: {project}");
    assert!(
        project["chunks"].as_u64().unwrap_or(0) >= 2,
        "project: {project}"
    );
    assert!(project["last_indexed_at"].is_i64(), "project: {project}");
}

#[test]
fn status_reports_sessions_watcher_versions() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");

    let empty = client.call_tool("status", json!({})).expect("status");
    assert_eq!(empty["sessions"], 0);
    assert_eq!(empty["watchers"], 0);
    assert!(empty["hint"].is_string(), "empty status: {empty}");

    Env::index_project(&mut client, env.root());
    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let status = bound.call_tool("status", json!({})).expect("status");
    assert_eq!(status["build_id"], build_id());
    assert_eq!(status["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(status["schema_version"], 1);
    assert!(
        status["sessions"].as_u64().unwrap_or(0) >= 1,
        "status: {status}"
    );
    assert!(status["fd_count"].is_u64(), "status: {status}");
    assert_eq!(
        status["watchers"], 1,
        "indexed project is watched: {status}"
    );
    let projects = status["projects"].as_array().expect("projects");
    assert_eq!(projects[0]["status"], "indexed");
    assert_eq!(
        projects[0]["watched"], true,
        "indexed project must show a live watcher: {status}"
    );
}

#[test]
fn huge_search_limit_is_clamped() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    // `u64::MAX` used to reach tantivy's allocating collector and abort the
    // shared daemon (final review C1).
    let hits = bound
        .call_tool(
            "search_docs",
            json!({ "query": "widgets", "limit": u64::MAX }),
        )
        .expect("huge limit must be clamped, not fatal");
    assert!(
        hits.as_array().expect("hits").len() <= 1_000,
        "clamped result set"
    );
    let status = bound.call_tool("status", json!({})).expect("daemon alive");
    assert_eq!(status["projects"][0]["docs"], 1, "status: {status}");
}

#[test]
fn search_rejects_bad_limit_and_scope() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());
    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");

    let err = bound
        .call_tool("search_docs", json!({ "query": "widgets", "limit": "ten" }))
        .expect_err("bad limit type must fail");
    assert!(err.to_string().contains("positive integer"), "{err}");

    let err = bound
        .call_tool(
            "search_docs",
            json!({ "query": "widgets", "scope": "shared" }),
        )
        .expect_err("v1 supports only the project scope");
    assert!(err.to_string().contains("scope"), "{err}");

    // Zero means "no hits" rather than an error (documented).
    let empty = bound
        .call_tool("search_docs", json!({ "query": "widgets", "limit": 0 }))
        .expect("zero limit is an empty result");
    assert_eq!(empty.as_array().expect("array").len(), 0, "zero limit");
}

#[test]
fn sync_job_runs_and_reports() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    let indexed = Env::index_project(&mut client, env.root());
    let project_id = indexed["project_id"].as_i64().expect("project id");

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let started = bound
        .call_tool("sync_start", json!({}))
        .expect("sync_start");
    let job_id = started["job_id"].as_i64().expect("job id");
    assert_eq!(started["project_id"], project_id);

    let finished = wait_job_done(&mut bound, job_id);
    assert_eq!(finished["state"], "done", "job: {finished}");
    assert_eq!(finished["project_id"], project_id);
    assert!(finished["started_at"].is_i64());
    assert!(finished["finished_at"].is_i64());
    assert!(finished["stats"].is_object(), "job: {finished}");
    assert!(
        finished["stats"]["skipped"].as_u64().unwrap_or(0) >= 1,
        "unchanged corpus must be skipped: {finished}"
    );
}

#[test]
fn second_sync_start_returns_current() {
    let owned = corpus(40, 200);
    let refs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_str()))
        .collect();
    let mut env = Env::new(&refs);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let first = bound
        .call_tool("sync_start", json!({}))
        .expect("first sync_start");
    let second = bound
        .call_tool("sync_start", json!({}))
        .expect("second sync_start");
    assert_eq!(
        first["job_id"], second["job_id"],
        "second sync_start must return the active job: {first} vs {second}"
    );
    assert!(
        matches!(second["state"].as_str(), Some("queued" | "running")),
        "second call returns the active job: {second}"
    );
    wait_job_done(&mut bound, first["job_id"].as_i64().expect("job id"));
}

#[test]
fn sync_job_persists_across_restart() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let started = bound
        .call_tool("sync_start", json!({}))
        .expect("sync_start");
    let job_id = started["job_id"].as_i64().expect("job id");
    let finished = wait_job_done(&mut bound, job_id);
    assert_eq!(finished["state"], "done");
    drop(bound);

    env.restart_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    let persisted = client
        .call_tool("sync_status", json!({ "job_id": job_id }))
        .expect("sync_status");
    assert_eq!(persisted["state"], "done", "persisted: {persisted}");
    assert_eq!(persisted["job_id"], job_id);
    assert!(persisted["stats"].is_object(), "persisted: {persisted}");
}

#[test]
fn orphaned_job_marked_error_on_restart() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());
    stop_daemon(env.cache()).expect("stop daemon");

    let conn = rusqlite::Connection::open(env.cache().join("registry.db")).expect("open db");
    let project_id: i64 = conn
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .expect("project id");
    conn.execute(
        "INSERT INTO sync_jobs (project_id, state, started_at) VALUES (?1, 'queued', 0)",
        [project_id],
    )
    .expect("insert queued job");
    let job_id = conn.last_insert_rowid();
    drop(conn);

    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    let job = client
        .call_tool("sync_status", json!({ "job_id": job_id }))
        .expect("sync_status");
    assert_eq!(job["state"], "error", "orphan: {job}");
    assert_eq!(job["stats"]["orphaned"], true, "orphan: {job}");
}

#[test]
fn malformed_config_does_not_stick_queued() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    fs::write(env.root().join(".docsbase.toml"), "not = [valid").expect("break config");

    // Open sessions keep the config they opened with (OQ-6), so the running
    // session is unaffected until the project is reopened.
    let started = bound
        .call_tool("sync_start", json!({}))
        .expect("cached open-time config");
    let finished = wait_job_done(&mut bound, started["job_id"].as_i64().expect("job id"));
    assert_eq!(finished["state"], "done", "job: {finished}");

    // Reopening the project surfaces the parse error before any job exists.
    let mut reopened = env.client();
    let err = reopened
        .handshake(env.root())
        .expect_err("broken config must fail project open");
    assert!(
        matches!(err, docsbase_memory::error::Error::Internal { .. }),
        "unexpected error: {err}"
    );

    fs::remove_file(env.root().join(".docsbase.toml")).expect("fix config");
    let mut fixed = env.client();
    fixed.handshake(env.root()).expect("reopen after fix");
    let started = fixed
        .call_tool("sync_start", json!({}))
        .expect("sync_start after fix");
    assert!(
        matches!(started["state"].as_str(), Some("queued" | "running")),
        "fresh job expected: {started}"
    );
    let finished = wait_job_done(&mut fixed, started["job_id"].as_i64().expect("job id"));
    assert_eq!(finished["state"], "done", "job: {finished}");
}

#[test]
fn unbound_frontend_indexes_then_searches() {
    let env = Env::new(&[("README.md", DOC)]);
    let mut mcp = Mcp::start(&env);
    mcp.initialize();

    let indexed = mcp.call_tool(2, "index_project", json!({}));
    assert_eq!(
        indexed["result"]["isError"], false,
        "index_project: {indexed}"
    );
    assert_eq!(indexed["result"]["structuredContent"]["state"], "done");

    let search = mcp.call_tool(3, "search_docs", json!({ "query": "widgets" }));
    assert_eq!(search["result"]["isError"], false, "search: {search}");
    let hits = search["result"]["structuredContent"]
        .as_array()
        .expect("hits array");
    assert!(!hits.is_empty(), "hits: {search}");
}

#[test]
fn watcher_refreshes_search() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind session");
    let status = bound.call_tool("status", json!({})).expect("status");
    assert_eq!(
        status["watchers"], 1,
        "indexed project is watched: {status}"
    );

    write_file(
        env.root(),
        "README.md",
        b"# Guide\n\ninstaller prose about gizmos.\n",
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let hits = bound
            .call_tool("search_docs", json!({ "query": "gizmos" }))
            .expect("search_docs");
        if !hits.as_array().expect("hits").is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not refresh search: {hits}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn sync_missing_root_gives_hint() {
    // Delete before the daemon exists: Windows cannot remove a directory
    // while the watcher holds its handle (os error 32), and the daemon must
    // still skip watching a missing root and fail syncs fast.
    let mut env = Env::new(&[("README.md", DOC)]);
    let output = env
        .cmd()
        .arg("index")
        .arg(env.root())
        .output()
        .expect("run index");
    assert!(output.status.success(), "index failed: {output:?}");
    std::fs::remove_dir_all(env.root()).expect("remove project root");
    env.start_daemon();

    let mut client = env.client();
    client.handshake_registry().expect("hello");

    let status = client.call_tool("status", json!({})).expect("status");
    assert_eq!(
        status["projects"][0]["root_state"], "missing",
        "status: {status}"
    );
    let project_id = status["projects"][0]["id"].as_i64().expect("project id");
    let err = client
        .call_tool("sync_start", json!({ "project_id": project_id }))
        .expect_err("missing root must fail fast");
    let text = err.to_string();
    assert!(
        text.contains("no longer exists"),
        "unexpected error: {text}"
    );
    assert!(
        text.contains("remove the project from the registry"),
        "hint missing: {text}"
    );
    // The registry entry and its data survive the failed sync.
    let projects = client
        .call_tool("list_projects", json!({}))
        .expect("list_projects");
    assert_eq!(projects.as_array().expect("array").len(), 1);
}

#[cfg(unix)]
#[test]
fn watcher_stops_when_root_disappears_and_restarts() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.start_daemon();
    let mut client = env.client();
    client.handshake_registry().expect("hello");
    Env::index_project(&mut client, env.root());

    let status = client.call_tool("status", json!({})).expect("status");
    assert_eq!(status["projects"][0]["watched"], true, "status: {status}");
    let project_id = status["projects"][0]["id"].as_i64().expect("id");

    std::fs::remove_dir_all(env.root()).expect("remove root");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stopped = false;
    while Instant::now() < deadline {
        let status = client.call_tool("status", json!({})).expect("status");
        if status["projects"][0]["watched"] == false {
            stopped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(stopped, "watcher must stop once the root is missing");

    // Recreating the root and syncing closes the loop: the watcher restarts.
    std::fs::create_dir_all(env.root()).expect("recreate root");
    write_file(env.root(), "NEW.md", DOC.as_bytes());
    client
        .call_tool("sync_start", json!({ "project_id": project_id }))
        .expect("sync_start");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut restarted = false;
    while Instant::now() < deadline {
        let status = client.call_tool("status", json!({})).expect("status");
        if status["projects"][0]["watched"] == true {
            restarted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(restarted, "watcher must restart after the root returns");
}

#[test]
fn watcher_restarts_after_missing_root_returns() {
    let mut env = Env::new(&[("README.md", DOC)]);
    let output = env
        .cmd()
        .arg("index")
        .arg(env.root())
        .output()
        .expect("run index");
    assert!(output.status.success(), "index failed: {output:?}");
    std::fs::remove_dir_all(env.root()).expect("remove project root");
    env.start_daemon();

    let mut client = env.client();
    client.handshake_registry().expect("hello");
    let status = client.call_tool("status", json!({})).expect("status");
    assert_eq!(status["projects"][0]["root_state"], "missing");
    assert_eq!(
        status["projects"][0]["watched"], false,
        "missing root must not be watched: {status}"
    );
    let project_id = status["projects"][0]["id"].as_i64().expect("id");

    std::fs::create_dir_all(env.root()).expect("recreate root");
    write_file(env.root(), "NEW.md", DOC.as_bytes());
    client
        .call_tool("sync_start", json!({ "project_id": project_id }))
        .expect("sync_start");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut restarted = false;
    while Instant::now() < deadline {
        let status = client.call_tool("status", json!({})).expect("status");
        if status["projects"][0]["watched"] == true {
            restarted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(restarted, "watcher must start once the root returns");
}
