use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::{daemon_pid, stop_daemon};
use docsbase_memory::ipc::client::{Client, socket_path};
use docsbase_memory::ipc::protocol::{
    PROTOCOL_VERSION, Request, Response, build_id, decode_response, encode,
};

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
        self.start_daemon_with_grace(3000);
    }

    fn start_daemon_with_grace(&mut self, grace_ms: u64) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", &grace_ms.to_string()])
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || socket_path(self.cache())
                .exists()),
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

fn status(client: &mut Client) -> Value {
    client.call_tool("status", json!({})).expect("status")
}

fn session_count(admin: &mut Client, deadline: Duration) -> u64 {
    let until = Instant::now() + deadline;
    loop {
        let sessions = status(admin)["sessions"].as_u64().expect("sessions");
        if sessions == 0 {
            return 0;
        }
        assert!(Instant::now() < until, "sessions did not drop: {sessions}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper mirrors the MCP call shape"
)]
fn mcp_call(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    )
    .expect("write");
    stdin.flush().expect("flush");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read");
    serde_json::from_str(&line).expect("json-rpc line")
}

fn hello(stream: &mut UnixStream) {
    let request = Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        build_id: build_id(),
        client: "cleanup-test".to_owned(),
    };
    let response = exchange(stream, &request);
    assert!(matches!(response, Response::Hello { .. }), "{response:?}");
}

fn exchange(stream: &mut UnixStream, request: &Request) -> Response {
    stream
        .write_all(&encode(request).expect("encode"))
        .expect("write");
    stream.flush().expect("flush");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).expect("read");
    assert!(!line.is_empty(), "daemon closed the connection");
    decode_response(&line).expect("decode")
}

const DOC: &str = "# Guide\n\ninstaller prose about widgets.\n";

#[test]
fn kill9_frontend_frees_resources() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let mut admin = env.client();
    admin.handshake_registry().expect("hello");
    let baseline = status(&mut admin);
    let baseline_fds = baseline["fd_count"].as_u64().expect("fds");

    let mut frontend = env
        .cmd()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn frontend");
    // The frontend connects lazily: one MCP call registers the session.
    let mut stdin = frontend.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(frontend.stdout.take().expect("stdout"));
    mcp_call(
        &mut stdin,
        &mut stdout,
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "cleanup-test", "version": "0" }
        }),
    );
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .expect("notify");
    stdin.flush().expect("flush");
    let response = mcp_call(
        &mut stdin,
        &mut stdout,
        2,
        "tools/call",
        json!({
            "name": "status",
            "arguments": {}
        }),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert!(
        wait_until(Duration::from_secs(10), || {
            status(&mut admin)["sessions"].as_u64().unwrap_or(0) >= 1
        }),
        "frontend must register a session"
    );

    let start = Instant::now();
    let killed = Command::new("kill")
        .args(["-9", &frontend.id().to_string()])
        .status()
        .expect("kill -9");
    assert!(killed.success(), "kill must succeed");
    let _ = frontend.wait();

    session_count(&mut admin, Duration::from_secs(2));
    assert!(
        start.elapsed() <= Duration::from_secs(2),
        "cleanup took {:?}",
        start.elapsed()
    );
    let after = status(&mut admin);
    let after_fds = after["fd_count"].as_u64().expect("fds");
    assert!(
        after_fds <= baseline_fds + 1,
        "descriptors leaked: baseline {baseline_fds}, after {after_fds}"
    );
}

#[test]
fn thousand_cycles_no_fd_growth() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let mut admin = env.client();
    admin.handshake_registry().expect("hello");
    let baseline = status(&mut admin);
    let baseline_fds = baseline["fd_count"].as_u64().expect("fds");
    let baseline_threads = baseline["threads"].as_u64().expect("threads");

    let cwd = env.root().to_path_buf();
    for _ in 0..1000 {
        let mut client = env.client();
        client.handshake_registry().expect("hello");
        let response = client
            .call(Request::RegisterSession {
                pid: std::process::id(),
                cwd: cwd.clone(),
            })
            .expect("register");
        assert!(
            matches!(response, Response::ToolResult { .. }),
            "{response:?}"
        );
        drop(client);
    }

    session_count(&mut admin, Duration::from_secs(5));
    let after = status(&mut admin);
    let after_fds = after["fd_count"].as_u64().expect("fds");
    let after_threads = after["threads"].as_u64().expect("threads");
    assert!(
        after_fds <= baseline_fds + 2,
        "descriptors leaked: baseline {baseline_fds}, after {after_fds}"
    );
    assert!(
        after_threads <= baseline_threads + 8,
        "threads leaked: baseline {baseline_threads}, after {after_threads}"
    );
}

#[test]
fn dead_session_does_not_block_shutdown() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon_with_grace(1000);
    let mut admin = env.client();
    admin.handshake_registry().expect("hello");

    let mut zombie = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let live_pid = zombie.id();

    let mut raw = UnixStream::connect(socket_path(env.cache())).expect("raw connect");
    hello(&mut raw);
    let response = exchange(
        &mut raw,
        &Request::RegisterSession {
            pid: live_pid,
            cwd: env.root().to_path_buf(),
        },
    );
    assert!(
        matches!(response, Response::ToolResult { .. }),
        "{response:?}"
    );
    assert!(
        wait_until(Duration::from_secs(1), || {
            status(&mut admin)["sessions"].as_u64().unwrap_or(0) >= 1
        }),
        "registration must be visible before the pid dies"
    );

    let killed = Command::new("kill")
        .args(["-9", &live_pid.to_string()])
        .status()
        .expect("kill -9");
    assert!(killed.success(), "kill must succeed");
    let _ = zombie.wait();

    // The pid is dead but this socket stays open; the janitor must prune the
    // session and the grace timer must end the daemon anyway (C6).
    let start = Instant::now();
    session_count(&mut admin, Duration::from_secs(2));
    assert!(
        start.elapsed() <= Duration::from_secs(2),
        "janitor took {:?}",
        start.elapsed()
    );
    assert!(
        wait_until(Duration::from_secs(4), || daemon_pid(env.cache()).is_none()),
        "daemon must exit despite the lingering connection"
    );
    drop(raw);
}

#[test]
fn registration_survives_armed_grace() {
    let mut env = Env::new(&[("README.md", DOC)]);
    let big = env.root().join("big-project");
    for index in 0..800 {
        let mut body = format!("# File {index}\n\nintro widget prose number {index}.\n");
        for section in 0..40 {
            let _ = writeln!(
                body,
                "\n## Section {section}\n\ninstaller prose about widget-{index}-{section} and gadgets."
            );
        }
        write_file(&big, &format!("docs/file-{index}.md"), body.as_bytes());
    }
    let config_dir = env.cache().join("config");
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(config_dir.join("config.toml"), "auto_index = true\n").expect("global config");
    env.start_daemon_with_grace(300);

    // A session-less connection closing arms the grace timer.
    let mut idle_conn = UnixStream::connect(socket_path(env.cache())).expect("idle connect");
    hello(&mut idle_conn);
    drop(idle_conn);
    std::thread::sleep(Duration::from_millis(100));

    // Registration of the big project runs a full auto-index far longer than
    // the remaining grace; it must not be cut off.
    let mut raw = UnixStream::connect(socket_path(env.cache())).expect("raw connect");
    hello(&mut raw);
    let response = exchange(
        &mut raw,
        &Request::RegisterSession {
            pid: std::process::id(),
            cwd: big.clone(),
        },
    );
    let Response::ToolResult { value } = response else {
        panic!("registration was cut off: {response:?}");
    };
    assert_eq!(value["status"], "indexed", "{value}");
    drop(raw);
}
