use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::ipc::client::socket_path;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, Request, Response, encode};

struct Env {
    cache: TempDir,
    root: TempDir,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        Self { cache, root }
    }

    fn cmd(&self, bin: &Path) -> Command {
        let mut command = Command::new(bin);
        command
            .env("DOCSBASE_CACHE_DIR", self.cache.path())
            .env("DOCSBASE_CONFIG_DIR", self.cache.path().join("config"))
            .current_dir(self.root.path());
        command
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = docsbase_memory::daemon::lifecycle::stop_daemon(self.cache.path());
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

/// Minimal daemon recording seen `CallTool` names.
fn spawn_fake_daemon(cache: &Path, seen: Arc<Mutex<Vec<String>>>) -> std::thread::JoinHandle<()> {
    let path = socket_path(cache);
    fs::create_dir_all(path.parent().expect("state")).expect("mkdir");
    let listener = UnixListener::bind(&path).expect("bind");
    let state = json!({
        "pid": std::process::id(),
        "socket": path,
        "build_id": docsbase_memory::ipc::protocol::build_id(),
        "schema_version": 1,
        "cache_root": cache,
    });
    fs::write(
        cache.join("state/daemon.json"),
        serde_json::to_vec(&state).expect("state json"),
    )
    .expect("write state");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = stream;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let request: Request = serde_json::from_str(line.trim()).expect("parse request");
                let response = match request {
                    Request::Hello { .. } => Response::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        build_id: docsbase_memory::ipc::protocol::build_id(),
                        schema_version: 1,
                    },
                    Request::RegisterSession { .. } => Response::ToolResult { value: Value::Null },
                    Request::CallTool { name, .. } => {
                        seen.lock().expect("lock").push(name.clone());
                        Response::ToolResult {
                            value: json!({ "routed_tool": name }),
                        }
                    }
                    Request::StopDaemon => break,
                };
                writer
                    .write_all(&encode(&response).expect("encode"))
                    .expect("write");
                writer.flush().expect("flush");
            }
        }
    })
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    lines: Vec<String>,
}

impl Mcp {
    fn start(env: &Env) -> Self {
        let mut child = env
            .cmd(&daemon_bin())
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
            lines: Vec::new(),
        }
    }

    fn request(&mut self, message: &Value) -> Value {
        writeln!(self.stdin, "{message}").expect("write");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).expect("read response");
        assert!(read > 0, "server closed stdout");
        self.lines.push(line.clone());
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
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const DOC: &str = "# Guide\n\ninstaller prose about widgets.\n";

#[test]
fn initialize_and_list_tools() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.cmd(&daemon_bin())
        .args(["index"])
        .output()
        .expect("index ok");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _daemon = spawn_fake_daemon(env.cache.path(), Arc::clone(&seen));

    let mut mcp = Mcp::start(&env);
    mcp.initialize();
    let response = mcp.request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = response["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "search_docs",
        "get_doc",
        "read_neighbors",
        "list_docs",
        "list_projects",
        "index_project",
        "sync_start",
        "sync_status",
        "status",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
}

#[test]
fn search_proxied_to_daemon() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.cmd(&daemon_bin())
        .args(["index"])
        .output()
        .expect("index ok");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _daemon = spawn_fake_daemon(env.cache.path(), Arc::clone(&seen));

    let mut mcp = Mcp::start(&env);
    mcp.initialize();
    let response = mcp.request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "search_docs", "arguments": { "query": "widgets" } }
    }));
    assert_eq!(
        response["result"]["structuredContent"]["routed_tool"],
        "search_docs"
    );
    assert_eq!(response["result"]["isError"], false);
    assert!(
        wait_until(Duration::from_secs(2), || seen
            .lock()
            .expect("lock")
            .iter()
            .any(|name| name == "search_docs")),
        "daemon must receive the proxied call"
    );
}

#[test]
fn stdout_has_no_logs() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.cmd(&daemon_bin())
        .args(["index"])
        .output()
        .expect("index ok");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _daemon = spawn_fake_daemon(env.cache.path(), Arc::clone(&seen));

    let mut mcp = Mcp::start(&env);
    mcp.initialize();
    let _ = mcp.request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "status", "arguments": {} }
    }));
    for line in &mcp.lines {
        let parsed: Value = serde_json::from_str(line).expect("stdout line must be JSON-RPC");
        assert_eq!(parsed["jsonrpc"], "2.0");
    }
    assert!(!mcp.lines.is_empty(), "protocol responses observed");
}

#[test]
fn unknown_tool_not_proxied() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _daemon = spawn_fake_daemon(env.cache.path(), Arc::clone(&seen));

    let mut mcp = Mcp::start(&env);
    mcp.initialize();
    let response = mcp.request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "delete_everything", "arguments": {} }
    }));
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("unknown tool"), "{text}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        seen.lock().expect("lock").is_empty(),
        "unknown tools must never reach the daemon (I8)"
    );
}

#[test]
fn unregistered_project_hint() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let mut daemon = env
        .cmd(&daemon_bin())
        .args(["serve", "--detached", "--grace-ms", "3000"])
        .spawn()
        .expect("spawn daemon");
    assert!(
        wait_until(Duration::from_secs(10), || socket_path(env.cache.path())
            .exists()),
        "socket must appear"
    );

    let mut mcp = Mcp::start(&env);
    mcp.initialize();
    let response = mcp.request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "search_docs", "arguments": { "query": "widgets" } }
    }));
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        text.contains("index_project") || text.contains("docsbase index"),
        "{text}"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
}
