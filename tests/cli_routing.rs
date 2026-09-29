use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use docsbase_memory::ipc::client::socket_path;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, Request, Response, encode};
use tempfile::TempDir;

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

    fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("docsbase");
        cmd.env("DOCSBASE_CACHE_DIR", self.cache.path());
        cmd.env("DOCSBASE_CONFIG_DIR", self.cache.path().join("config"));
        cmd.current_dir(self.root.path());
        cmd
    }

    fn index(&self) {
        self.cmd().args(["index", "."]).assert().success();
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn stdout_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Minimal daemon: answers the handshake and echoes the tool name back.
fn spawn_fake_daemon(cache: &Path) -> thread::JoinHandle<()> {
    let path = socket_path(cache);
    fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir state");
    let listener = UnixListener::bind(&path).expect("bind socket");
    listener.set_nonblocking(false).expect("blocking listener");

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut writer = stream;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let request: Request =
                    serde_json::from_str(line.trim()).expect("parse request line");
                let response = match request {
                    Request::Hello { .. } => Response::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        build_id: "test-daemon".to_owned(),
                        schema_version: 1,
                    },
                    Request::RegisterSession { .. } | Request::RegisterUnbound { .. } => {
                        Response::ToolResult {
                            value: serde_json::Value::Null,
                        }
                    }
                    Request::CallTool { name, .. } => Response::ToolResult {
                        value: serde_json::json!({ "routed_tool": name }),
                    },
                    Request::StopDaemon => break,
                };
                writer
                    .write_all(&encode(&response).expect("encode"))
                    .expect("write response");
                writer.flush().expect("flush");
            }
        }
    })
}

const DOC: &str = "---\ntitle: Guide\n---\n\n# Install\n\ninstaller prose about widgets.\n";

#[test]
fn routes_through_daemon_when_alive() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let _daemon = spawn_fake_daemon(env.cache.path());

    let output = env
        .cmd()
        .args(["search", "widgets"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(
        stdout_text(&output).trim(),
        "{\n  \"routed_tool\": \"search_docs\"\n}"
    );

    let output = env
        .cmd()
        .arg("status")
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(
        stdout_text(&output).trim(),
        "{\n  \"routed_tool\": \"status\"\n}"
    );
}

#[test]
fn falls_back_when_dead() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.index();

    let path = socket_path(env.cache.path());
    fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir state");
    let listener = UnixListener::bind(&path).expect("bind stale socket");
    drop(listener);
    thread::sleep(Duration::from_millis(20));

    let output = env
        .cmd()
        .args(["search", "widgets"])
        .assert()
        .success()
        .get_output()
        .clone();
    let rows: serde_json::Value = serde_json::from_str(stdout_text(&output).trim()).expect("json");
    let rows = rows.as_array().expect("direct-mode array");
    assert_eq!(rows[0]["path"], "docs/a.md", "must use the local snapshot");
}

#[test]
fn daemon_errors_surface() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let path = socket_path(env.cache.path());
    fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir state");
    let listener = UnixListener::bind(&path).expect("bind socket");

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).expect("hello");
        let response = Response::Error {
            code: -32012,
            message: "no project registered for /tmp/x".to_owned(),
            instruction: Some("call index_project first".to_owned()),
        };
        writer
            .write_all(&encode(&response).expect("encode"))
            .expect("write");
        writer.flush().expect("flush");
    });

    let output = env
        .cmd()
        .args(["search", "widgets"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("no project registered"), "{stderr}");
    let _ = handle.join();
}
