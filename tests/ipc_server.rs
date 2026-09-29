use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::error::Error;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, Request, Response, encode};
use docsbase_memory::platform::{self, BlockingStream};
use fd_lock::RwLock;
use tempfile::TempDir;

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

    fn socket(&self) -> PathBuf {
        self.cache().join("state/daemon.sock")
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command.env("DOCSBASE_CACHE_DIR", self.cache());
        command.env("DOCSBASE_CONFIG_DIR", self.cache().join("config"));
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
            wait_until(Duration::from_secs(10), || self.socket().exists()),
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
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn read_response(reader: &mut BufReader<BlockingStream>) -> Response {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).expect("read response");
    docsbase_memory::ipc::protocol::decode_response(&line).expect("decode response")
}

const DOC: &str = "---\ntitle: Guide\n---\n\n# Install\n\ninstaller prose about widgets.\n";

#[test]
fn hello_version_mismatch_rejected() {
    let mut env = Env::new(&[]);
    env.start_daemon();

    let stream =
        platform::connect_blocking(&platform::daemon_endpoint(env.cache())).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;
    let hello = Request::Hello {
        protocol_version: PROTOCOL_VERSION + 1,
        build_id: docsbase_memory::ipc::protocol::build_id(),
        client: "test".to_owned(),
    };
    writer
        .write_all(&encode(&hello).expect("encode"))
        .expect("write");

    match read_response(&mut reader) {
        Response::Error { code, .. } => assert_eq!(code, -32011),
        other => panic!("expected protocol error, got {other:?}"),
    }

    let mut rest = Vec::new();
    let read = reader.read_until(b'\n', &mut rest).expect("read eof");
    assert_eq!(read, 0, "server must close after a version mismatch");
}

#[test]
fn hello_build_mismatch_rejected() {
    let mut env = Env::new(&[]);
    env.start_daemon();

    let stream =
        platform::connect_blocking(&platform::daemon_endpoint(env.cache())).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;
    let hello = Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        build_id: "other-build".to_owned(),
        client: "test".to_owned(),
    };
    writer
        .write_all(&encode(&hello).expect("encode"))
        .expect("write");

    match read_response(&mut reader) {
        Response::Error { code, .. } => assert_eq!(code, -32010, "I1 admission"),
        other => panic!("expected admission error, got {other:?}"),
    }
}

#[test]
fn call_tool_requires_hello() {
    let mut env = Env::new(&[]);
    env.start_daemon();

    let stream =
        platform::connect_blocking(&platform::daemon_endpoint(env.cache())).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;
    let call = Request::CallTool {
        name: "status".to_owned(),
        args: serde_json::json!({}),
    };
    writer
        .write_all(&encode(&call).expect("encode"))
        .expect("write");

    match read_response(&mut reader) {
        Response::Error { code, .. } => assert_eq!(code, -32011),
        other => panic!("expected protocol error, got {other:?}"),
    }
}

#[test]
fn session_bound_to_project_by_cwd() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    env.start_daemon();

    let mut client = env.client();
    client
        .handshake(env.root())
        .expect("handshake binds project");
    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["projects"][0]["status"], "indexed");
    assert_eq!(status["projects"][0]["docs"], 1);
    assert_eq!(status["sessions"], 1);
    assert!(status["fd_count"].as_u64().expect("fd_count") > 0);
}

#[test]
fn unknown_tool_rejected() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    env.start_daemon();

    let mut client = env.client();
    client.handshake(env.root()).expect("handshake");
    match client.call(Request::CallTool {
        name: "delete_everything".to_owned(),
        args: serde_json::json!({}),
    }) {
        Ok(Response::Error { code, .. }) => assert_eq!(code, -32011, "allowlist (I8)"),
        other => panic!("expected protocol error, got {other:?}"),
    }
}

#[test]
fn eof_removes_session() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    env.start_daemon();

    let mut first = env.client();
    first.handshake(env.root()).expect("first handshake");
    let mut second = env.client();
    second.handshake(env.root()).expect("second handshake");

    let status = second
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["sessions"], 2);

    drop(first);
    let removed = wait_until(Duration::from_secs(5), || {
        second
            .call_tool("status", serde_json::json!({}))
            .is_ok_and(|value| value["sessions"] == 1)
    });
    assert!(removed, "EOF must remove the session (I2)");
}

#[test]
fn re_register_replaces_session() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    env.start_daemon();

    let mut client = env.client();
    client.handshake(env.root()).expect("first");
    client.handshake(env.root()).expect("second");
    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["sessions"], 1, "old session must be left (I2)");
}

#[test]
fn auto_index_from_subdirectory() {
    let mut env = Env::new(&[
        (".docsbase.toml", "auto_index = true\n"),
        ("docs/a.md", DOC),
    ]);
    fs::create_dir_all(env.root().join(".git")).expect("git dir");
    let sub = env.root().join("nested/deep");
    fs::create_dir_all(&sub).expect("subdir");
    env.start_daemon();

    let mut client = env.client();
    client.handshake(&sub).expect("auto-index from subdir");
    let hits = client
        .call_tool("search_docs", serde_json::json!({ "query": "widgets" }))
        .expect("search");
    assert!(
        hits.as_array().is_some_and(|rows| !rows.is_empty()),
        "{hits}"
    );
}

#[test]
fn lease_conflict_keeps_status() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    env.start_daemon();

    let lock_path = env.cache().join("projects/1/.writer.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("lock file");
    let mut lock = RwLock::new(file);
    let guard = lock.write().expect("hold lease");

    let mut client = env.client();
    client.handshake(env.root()).expect("handshake");
    let result = client.call_tool("index_project", serde_json::json!({}));
    assert!(result.is_err(), "lease conflict must fail: {result:?}");

    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(
        status["projects"][0]["status"], "indexed",
        "conflict must not flip status to error"
    );
    drop(guard);
}

#[test]
fn project_not_registered_message() {
    let mut env = Env::new(&[("docs/a.md", DOC)]);
    env.start_daemon();

    let mut client = env.client();
    let err = client.handshake(env.root()).expect_err("must refuse");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
    let text = err.to_string();
    assert!(
        text.contains("index_project"),
        "hint must survive the wire: {text}"
    );
}

#[test]
fn auto_index_registers_and_indexes() {
    let mut env = Env::new(&[
        (".docsbase.toml", "auto_index = true\n"),
        ("docs/a.md", DOC),
    ]);
    env.start_daemon();

    let mut client = env.client();
    client.handshake(env.root()).expect("auto-index handshake");

    let hits = client
        .call_tool("search_docs", serde_json::json!({ "query": "widgets" }))
        .expect("search");
    assert!(
        hits.as_array().is_some_and(|rows| !rows.is_empty()),
        "auto-indexed content searchable: {hits}"
    );

    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["projects"][0]["status"], "indexed");
}
