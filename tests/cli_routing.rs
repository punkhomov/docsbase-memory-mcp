use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, Request, Response, encode};
use docsbase_memory::platform;
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
    spawn_fake_daemon_with_capability(cache, true)
}

fn spawn_fake_daemon_with_capability(cache: &Path, capable: bool) -> thread::JoinHandle<()> {
    spawn_fake_daemon_with_reply(cache, capable, None)
}

fn spawn_fake_daemon_with_reply(
    cache: &Path,
    capable: bool,
    search_reply: Option<Response>,
) -> thread::JoinHandle<()> {
    let endpoint = platform::daemon_endpoint(cache);
    fs::create_dir_all(cache.join("state")).expect("mkdir state");
    let listener = platform::bind_blocking(&endpoint).expect("bind socket");

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
                        capabilities: if capable {
                            vec![
                                docsbase_memory::ipc::protocol::SEARCH_CONTEXT_CAPABILITY
                                    .to_owned(),
                            ]
                        } else {
                            Vec::new()
                        },
                    },
                    Request::RegisterSession { .. }
                    | Request::RegisterUnbound { .. }
                    | Request::BindReadOnly { .. } => Response::ToolResult {
                        value: serde_json::Value::Null,
                    },
                    Request::CallTool { name, .. } if name == "search_docs" && capable => {
                        Response::Error {
                            code: -32011,
                            message: "fake daemon expects scoped search".to_owned(),
                            instruction: None,
                        }
                    }
                    Request::CallTool { name, .. } => Response::ToolResult {
                        value: serde_json::json!({ "routed_tool": name }),
                    },
                    Request::Search { .. } if capable => {
                        search_reply
                            .clone()
                            .unwrap_or_else(|| Response::ToolResult {
                                value: serde_json::json!({"routed_tool":"search_docs"}),
                            })
                    }
                    Request::Search { .. } => Response::Error {
                        code: -32011,
                        message: "legacy fake daemon does not support scoped search".to_owned(),
                        instruction: None,
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

    let endpoint = platform::daemon_endpoint(env.cache.path());
    fs::create_dir_all(env.cache.path().join("state")).expect("mkdir state");
    let listener = platform::bind_blocking(&endpoint).expect("bind stale socket");
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

// Failure bounds for this fixture and its child, not product latency contracts.
const SYNC_PEER_WATCHDOG: Duration = Duration::from_secs(10);

#[cfg(unix)]
struct SyncListener(platform::Listener);

#[cfg(windows)]
struct SyncListener(tokio::net::windows::named_pipe::NamedPipeServer);

impl SyncListener {
    #[cfg(unix)]
    fn bind(endpoint: &platform::Endpoint) -> std::io::Result<Self> {
        platform::bind(endpoint).map(Self)
    }

    #[cfg(windows)]
    fn bind(endpoint: &platform::Endpoint) -> std::io::Result<Self> {
        // interprocess flush/drop can spawn blocking flush/linger workers.
        // Tokio's native pipe closes directly when the fixture is cancelled.
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(sync_pipe_path(endpoint)?)
            .map(Self)
    }

    #[cfg(unix)]
    async fn accept(self) -> std::io::Result<platform::Stream> {
        self.0.accept().await
    }

    #[cfg(windows)]
    async fn accept(self) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        self.0.connect().await?;
        Ok(self.0)
    }
}

#[cfg(windows)]
fn sync_pipe_path(endpoint: &platform::Endpoint) -> std::io::Result<String> {
    match endpoint {
        platform::Endpoint::Pipe(name) => Ok(format!(r"\\.\pipe\{name}")),
        platform::Endpoint::Unix(_) => Err(std::io::ErrorKind::InvalidInput.into()),
    }
}

#[cfg(unix)]
async fn sync_test_client(endpoint: &platform::Endpoint) -> std::io::Result<platform::Stream> {
    platform::connect_async(endpoint).await
}

#[cfg(windows)]
fn sync_test_client(
    endpoint: &platform::Endpoint,
) -> impl std::future::Future<Output = std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient>>
{
    std::future::ready(
        sync_pipe_path(endpoint)
            .and_then(|path| tokio::net::windows::named_pipe::ClientOptions::new().open(path)),
    )
}

fn sync_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sync fixture runtime")
}

#[derive(Debug, PartialEq, Eq)]
enum SyncPeerExit {
    Complete,
    Cancelled,
    Watchdog,
    Failed(std::io::ErrorKind),
}

#[derive(Debug)]
struct SyncTrace {
    requests: Vec<Request>,
    exit: SyncPeerExit,
}

struct SyncPeer {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<thread::JoinHandle<SyncTrace>>,
    accepted: std::sync::mpsc::Receiver<()>,
}

impl SyncPeer {
    fn new(cache: &Path) -> Self {
        Self::with_watchdog(cache, SYNC_PEER_WATCHDOG)
    }

    fn with_watchdog(cache: &Path, watchdog: Duration) -> Self {
        let runtime = sync_runtime();
        let endpoint = platform::daemon_endpoint(cache);
        fs::create_dir_all(cache.join("state")).expect("mkdir state");
        // Bind before starting the command. Both native backends expose async
        // accept and IO, so cancellation also covers Windows named pipes.
        let listener = {
            let _entered = runtime.enter();
            SyncListener::bind(&endpoint).expect("bind sync peer")
        };
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let (accepted, received) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            runtime.block_on(async move {
                let mut requests = Vec::new();
                let mut protocol_complete = false;
                let exit = tokio::select! {
                    biased;
                    result = async {
                        let mut stream = listener.accept().await?;
                        let _ = accepted.send(());
                        serve_sync_stream(&mut stream, &mut requests).await?;
                        protocol_complete = true;
                        // Raw Windows pipe flush does not acknowledge delivery.
                        // Keep its reply handle until command completion causes
                        // finish/cancellation, or the original watchdog fires.
                        std::future::pending::<()>().await;
                        drop(stream);
                        Ok::<(), std::io::Error>(())
                    } => match result {
                        Ok(()) => SyncPeerExit::Complete,
                        Err(error) => SyncPeerExit::Failed(error.kind()),
                    },
                    _ = &mut cancelled => SyncPeerExit::Cancelled,
                    () = tokio::time::sleep(watchdog) => SyncPeerExit::Watchdog,
                };
                let exit = if protocol_complete && exit == SyncPeerExit::Cancelled {
                    SyncPeerExit::Complete
                } else {
                    exit
                };
                SyncTrace { requests, exit }
            })
        });
        Self {
            cancel: Some(cancel),
            handle: Some(handle),
            accepted: received,
        }
    }

    fn finish(mut self) -> SyncTrace {
        let _ = self.cancel.take().expect("cancel sender").send(());
        self.join()
    }

    fn join(mut self) -> SyncTrace {
        self.handle
            .take()
            .expect("sync peer thread")
            .join()
            .expect("join sync peer")
    }
}

impl Drop for SyncPeer {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

async fn serve_sync_stream(
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
    requests: &mut Vec<Request>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut stream = tokio::io::BufReader::new(stream);
    for _ in 0..4 {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let request: Request = serde_json::from_str(line.trim())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        requests.push(request);
        let response = match requests.last().expect("recorded request") {
            Request::Hello { .. } => Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                build_id: "test-daemon".to_owned(),
                schema_version: 1,
                capabilities: Vec::new(),
            },
            Request::RegisterSession { .. } => Response::ToolResult {
                value: serde_json::Value::Null,
            },
            Request::CallTool { name, .. } if name == "sync_start" => Response::ToolResult {
                value: serde_json::json!({
                    "job_id": 7, "state": "running", "stats": null
                }),
            },
            Request::CallTool { name, .. } if name == "sync_status" => Response::ToolResult {
                value: serde_json::json!({
                    "job_id": 7, "state": "done", "stats": { "docs": 1 }
                }),
            },
            _ => return Err(std::io::ErrorKind::InvalidData.into()),
        };
        let bytes = encode(&response).map_err(std::io::Error::other)?;
        stream.get_mut().write_all(&bytes).await?;
        stream.get_mut().flush().await?;
    }
    Ok(())
}

fn with_sync_peer(
    cache: &Path,
    command: impl FnOnce() -> std::io::Result<std::process::Output>,
) -> (std::io::Result<std::process::Output>, SyncTrace) {
    let peer = SyncPeer::new(cache);
    let output = command();
    // This also runs for spawn errors; Drop performs the same cancellation and
    // join if command setup unwinds. No assertion precedes peer teardown.
    let trace = peer.finish();
    (output, trace)
}

fn sync_through_peer(env: &Env, target: Option<&Path>) -> (std::process::Output, SyncTrace) {
    let mut command = env.cmd();
    command.arg("sync").timeout(SYNC_PEER_WATCHDOG);
    if let Some(target) = target {
        command.arg(target);
    }
    let (output, trace) = with_sync_peer(env.cache.path(), || command.output());
    (output.expect("sync process"), trace)
}

fn assert_sync_target(env: &Env, target: Option<&Path>, expected: &Path) {
    let (output, trace) = sync_through_peer(env, target);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(trace.exit, SyncPeerExit::Complete, "{trace:?}");
    let requests = trace.requests;
    assert_eq!(
        requests.len(),
        4,
        "one bound connection must carry the complete handshake/start/status exchange"
    );
    assert!(matches!(&requests[0], Request::Hello { .. }));
    let Request::RegisterSession { cwd, .. } = &requests[1] else {
        panic!("expected bound session: {requests:?}");
    };
    assert_eq!(
        cwd.canonicalize().expect("recorded target"),
        expected.canonicalize().expect("selected target"),
        "sync must bind its selected target before starting the job"
    );
    assert!(matches!(&requests[2], Request::CallTool { name, args }
        if name == "sync_start" && args == &serde_json::json!({})));
    assert!(matches!(&requests[3], Request::CallTool { name, args }
        if name == "sync_status" && args == &serde_json::json!({"job_id": 7})));
    let json: serde_json::Value = serde_json::from_str(stdout_text(&output).trim()).expect("json");
    assert_eq!(json["job_id"], 7);
    assert_eq!(json["state"], "done");
}

#[test]
fn sync_peer_joins_when_command_finishes_without_connecting() {
    let env = Env::new(&[]);
    let (output, trace) = with_sync_peer(env.cache.path(), || {
        env.cmd()
            .arg("--version")
            .timeout(SYNC_PEER_WATCHDOG)
            .output()
    });
    assert!(output.expect("early exit").status.success());
    assert_eq!(trace.exit, SyncPeerExit::Cancelled);
    assert_eq!(trace.requests, Vec::<Request>::new());

    let env = Env::new(&[]);
    let missing = env.cache.path().join("missing-executable");
    let (output, trace) = with_sync_peer(env.cache.path(), || Command::new(&missing).output());
    assert_eq!(
        output.expect_err("spawn failure").kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(trace.exit, SyncPeerExit::Cancelled);
    assert_eq!(trace.requests, Vec::<Request>::new());
}

#[test]
fn sync_peer_cancels_native_clients_without_a_complete_frame() {
    for prefix in [b"".as_slice(), b"{\"method\":\"Hello\"".as_slice()] {
        let env = Env::new(&[]);
        let peer = SyncPeer::new(env.cache.path());
        sync_runtime().block_on(async {
            use tokio::io::AsyncWriteExt;
            let mut client = sync_test_client(&platform::daemon_endpoint(env.cache.path()))
                .await
                .expect("connect held client");
            client.write_all(prefix).await.expect("partial frame");
            peer.accepted
                .recv_timeout(SYNC_PEER_WATCHDOG)
                .expect("client accepted");
            let trace = peer.finish();
            // Keep the client alive through join: neither EOF nor pipe timeout
            // may be responsible for ending the peer's blocked read.
            drop(client);
            assert_eq!(trace.exit, SyncPeerExit::Cancelled, "{prefix:?}");
            assert_eq!(trace.requests, Vec::<Request>::new());
        });
    }
}

#[test]
fn sync_peer_watchdog_bounds_accept_without_cancellation() {
    let env = Env::new(&[]);
    let trace = SyncPeer::with_watchdog(env.cache.path(), Duration::ZERO).join();
    assert_eq!(trace.exit, SyncPeerExit::Watchdog);
    assert_eq!(trace.requests, Vec::<Request>::new());
}

#[test]
fn sync_via_daemon_polls_until_done() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    assert_sync_target(&env, None, env.root.path());
}

#[test]
fn sync_via_daemon_uses_explicit_absolute_target() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let target = TempDir::new().expect("selected project");
    write_file(target.path(), "docs/b.md", b"selected project body");
    assert_sync_target(&env, Some(target.path()), target.path());
}

#[test]
fn sync_via_daemon_uses_explicit_relative_target() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let target = env.root.path().join("selected-project");
    write_file(&target, "docs/b.md", b"selected project body");
    assert_sync_target(&env, Some(Path::new("selected-project")), &target);
}

#[test]
fn sync_rejects_missing_explicit_target_before_daemon_or_direct_work() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let target = env.root.path().join("missing-project");
    let endpoint = platform::daemon_endpoint(env.cache.path());
    fs::create_dir_all(env.cache.path().join("state")).expect("mkdir state");
    let listener = platform::bind_blocking(&endpoint).expect("bind socket");
    listener.set_nonblocking(true).expect("nonblocking accept");

    let output = env
        .cmd()
        .arg("sync")
        .arg(&target)
        .timeout(Duration::from_secs(10))
        .output()
        .expect("sync process");
    let connected = match listener.accept() {
        Ok(stream) => {
            drop(stream);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("inspect pending connection: {error}"),
    };
    assert!(
        !connected,
        "invalid target must fail before daemon connection"
    );
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing-project"),
        "{output:?}"
    );
    assert!(!env.cache.path().join("registry.db").exists());
    assert!(!env.cache.path().join("projects").exists());
}

#[test]
fn sync_without_daemon_indexes_explicit_target() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let target = TempDir::new().expect("selected project");
    write_file(target.path(), "docs/b.md", b"selected project body");
    env.cmd().arg("sync").arg(target.path()).assert().success();

    let output = env
        .cmd()
        .arg("status")
        .assert()
        .success()
        .get_output()
        .clone();
    let status: serde_json::Value = serde_json::from_str(&stdout_text(&output)).expect("status");
    let projects = status["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 1, "cwd project must not be registered");
    assert_eq!(
        Path::new(projects[0]["root"].as_str().expect("root"))
            .canonicalize()
            .expect("indexed root"),
        target.path().canonicalize().expect("selected root")
    );
    let output = env
        .cmd()
        .current_dir(target.path())
        .arg("list")
        .assert()
        .success()
        .get_output()
        .clone();
    let listed: serde_json::Value = serde_json::from_str(&stdout_text(&output)).expect("list");
    let docs = listed["docs"].as_array().expect("docs");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["path"], "docs/b.md");
}

#[test]
fn daemon_errors_surface() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let endpoint = platform::daemon_endpoint(env.cache.path());
    fs::create_dir_all(env.cache.path().join("state")).expect("mkdir state");
    let listener = platform::bind_blocking(&endpoint).expect("bind socket");

    let handle = thread::spawn(move || {
        let stream = listener.accept().expect("accept");
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

#[test]
fn search_refuses_legacy_daemon_without_direct_fallback() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.index(); // A successful direct result must not hide incompatibility.
    let _daemon = spawn_fake_daemon_with_capability(env.cache.path(), false);
    let output = env
        .cmd()
        .timeout(Duration::from_mins(5))
        .args(["search", "widgets"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("restart"), "{stderr}");
}

#[test]
fn connected_search_failure_does_not_fall_back_to_valid_local_index() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.index();
    let _daemon = spawn_fake_daemon_with_reply(
        env.cache.path(),
        true,
        Some(Response::Error {
            code: -32014,
            message: "backend rejected this search".to_owned(),
            instruction: None,
        }),
    );
    let output = env
        .cmd()
        .timeout(Duration::from_mins(5))
        .args(["search", "widgets"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("backend rejected this search"), "{stderr}");
}
