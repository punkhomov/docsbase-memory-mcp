//! Error surfacing: stable MCP codes, project instructions, and non-fatal
//! per-file warnings (FR-18, FR-19, NFR-8; design §9).

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::ipc::client::Client;

struct Env {
    cache: TempDir,
    root: TempDir,
    daemon: Child,
}

impl Env {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in files {
            write_file(root.path(), rel, body);
        }
        let daemon = Command::new(daemon_bin())
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .env("DOCSBASE_CACHE_DIR", cache.path())
            .env("DOCSBASE_CONFIG_DIR", cache.path().join("config"))
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        assert!(
            wait_until(Duration::from_secs(10), || cache
                .path()
                .join("state/daemon.sock")
                .exists()),
            "socket must appear"
        );
        Self {
            cache,
            root,
            daemon,
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

    fn client(&self) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect(self.cache()) {
                return client;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not connectable");
    }

    fn index(&self) {
        let output = self.cmd().arg("index").output().expect("run index");
        assert!(output.status.success(), "index failed: {output:?}");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = stop_daemon(self.cache());
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
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

/// Minimal stdio MCP client for the real frontend binary.
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
        let mut mcp = Self {
            child,
            stdin,
            stdout,
        };
        mcp.initialize();
        mcp
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

    fn call(&mut self, name: &str, args: &Value) -> Value {
        self.request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn error_code(response: &Value) -> i64 {
    assert_eq!(response["result"]["isError"], true, "{response}");
    response["result"]["structuredContent"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("missing structured code: {response}"))
}

#[test]
fn index_error_does_not_fail_job() {
    let env = Env::new(&[("good.md", b"# Good\n\ngoodwidget prose.\n")]);
    // Invalid UTF-8 must skip only this file (A4, FR-18).
    write_file(env.root(), "bad.md", &[0xff, 0xfe, 0x00, 0x01]);
    // Over max_file_size must warn, not abort (FR-19).
    let mut huge = b"# Huge\n\n".to_vec();
    huge.extend(std::iter::repeat_n(b'a', 1_200_000));
    write_file(env.root(), "huge.md", &huge);

    let mut registry = env.client();
    registry.handshake_registry().expect("hello");
    let job = registry
        .call_tool(
            "index_project",
            json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect("index_project");
    assert_eq!(
        job["state"], "done",
        "job must not fail on file errors: {job}"
    );
    let errors = job["stats"]["errors"].as_u64().expect("errors");
    assert!(errors >= 2, "both bad files counted: {job}");
    let warnings = job["stats"]["warnings"].as_array().expect("warnings");
    let paths: Vec<&str> = warnings
        .iter()
        .map(|warning| warning["path"].as_str().expect("path"))
        .collect();
    assert!(paths.contains(&"bad.md"), "warnings: {warnings:?}");
    assert!(paths.contains(&"huge.md"), "warnings: {warnings:?}");

    let mut bound = env.client();
    assert!(bound.handshake(env.root()).is_ok(), "bind after indexing");
    let hits = bound
        .call_tool("search_docs", json!({ "query": "goodwidget" }))
        .expect("search");
    assert_eq!(hits[0]["path"], "good.md", "healthy file stays searchable");

    let status = bound.call_tool("status", json!({})).expect("status");
    let warnings = status["projects"][0]["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("status warnings missing: {status}"));
    let paths: Vec<&str> = warnings
        .iter()
        .map(|warning| warning["path"].as_str().expect("path"))
        .collect();
    assert!(paths.contains(&"bad.md"), "status: {status}");
    assert!(paths.contains(&"huge.md"), "status: {status}");
}

#[test]
fn mcp_error_codes_stable() {
    let env = Env::new(&[("docs/a.md", b"# Guide\n\ninstaller prose.\n")]);
    env.index();

    let mut mcp = Mcp::start(&env);
    let query = mcp.call("search_docs", &json!({ "query": "   " }));
    assert_eq!(error_code(&query), -32014, "query error code: {query}");
    assert!(
        query["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("query"),
        "{query}"
    );

    let project = mcp.call("sync_status", &json!({ "job_id": 9999 }));
    assert_eq!(
        error_code(&project),
        -32012,
        "project error code: {project}"
    );

    let unknown = mcp.call("delete_everything", &json!({}));
    assert_eq!(
        error_code(&unknown),
        -32011,
        "unknown tool is a protocol error: {unknown}"
    );
}

#[test]
fn project_error_has_instruction() {
    let env = Env::new(&[("docs/a.md", b"# Guide\n\ninstaller prose.\n")]);
    // The project is deliberately not indexed: the frontend keeps the
    // daemon's instruction hint.
    let mut mcp = Mcp::start(&env);
    let response = mcp.call("search_docs", &json!({ "query": "installer" }));
    assert_eq!(error_code(&response), -32012, "{response}");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        text.contains("index_project") || text.contains("docsbase index"),
        "{text}"
    );
    let instruction = response["result"]["structuredContent"]["instruction"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instruction.contains("index_project") || instruction.contains("docsbase index"),
        "structured instruction missing: {response}"
    );
}
