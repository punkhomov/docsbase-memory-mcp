use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use docsbase_memory::daemon::lifecycle::{MAX_CONNECTIONS, ensure_daemon_with, stop_daemon};
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{PROTOCOL_VERSION, Request, encode};
use docsbase_memory::platform;
#[cfg(unix)]
use docsbase_memory::platform::process::process_alive;
use docsbase_memory::store::DB_FILE;
use docsbase_memory::store::MAX_SYNC_JOBS;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            cache: TempDir::new().expect("cache"),
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn endpoint_up(&self) -> bool {
        platform::exists(&platform::daemon_endpoint(self.cache()))
    }

    fn start(&self) -> u32 {
        ensure_daemon_with(self.cache(), Some(&daemon_bin())).expect("start daemon");
        read_pid(self.cache())
    }

    fn stop(&self) {
        stop_daemon(self.cache()).expect("stop daemon");
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command.env("DOCSBASE_CACHE_DIR", self.cache());
        command
    }
}

fn daemon_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin!("docsbase").to_path_buf()
}

fn read_pid(cache: &Path) -> u32 {
    let bytes = fs::read(cache.join("state/daemon.json")).expect("daemon.json");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    u32::try_from(json["pid"].as_u64().expect("pid")).expect("pid fits")
}

fn pid_alive(pid: u32) -> bool {
    docsbase_memory::platform::process::process_alive(pid)
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

#[test]
fn first_start_creates_daemon() {
    let env = Env::new();
    let pid = env.start();

    assert!(pid_alive(pid), "daemon pid must be alive");
    let log = env.cache().join("logs/daemon.log");
    fs::metadata(&log).expect("NFR-8: detached daemon writes logs/daemon.log");
    assert!(env.endpoint_up(), "endpoint must exist");
    #[cfg(unix)]
    {
        let metadata = fs::metadata(&log).expect("log metadata");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "daemon log must be owner-only"
        );
        let socket = platform::daemon_endpoint(env.cache());
        let mode = fs::metadata(socket.as_path())
            .expect("metadata")
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600, "socket must be 0600");
    }
    assert!(env.cache().join("state/daemon.json").exists());

    env.stop();
    assert!(!env.endpoint_up(), "endpoint removed on stop");
    assert!(!pid_alive(pid), "process exited");
}

#[test]
fn second_start_reuses() {
    let env = Env::new();
    let first = env.start();
    let second = env.start();
    assert_eq!(first, second, "no second daemon");
    env.stop();
}

#[test]
fn stop_command_terminates() {
    let env = Env::new();
    let pid = env.start();

    env.cli()
        .args(["daemon", "stop"])
        .output()
        .map(|output| assert!(output.status.success(), "stop must succeed"))
        .expect("run stop");

    assert!(!env.endpoint_up(), "endpoint removed");
    assert!(wait_until(Duration::from_secs(2), || !pid_alive(pid)));
}

#[test]
fn stale_state_recovered() {
    let env = Env::new();
    env.start();
    env.stop();

    fs::create_dir_all(env.cache().join("state")).expect("state dir");
    fs::write(
        env.cache().join("state/daemon.json"),
        br#"{"pid":999999,"socket":"/nonexistent","build_id":"old","schema_version":1,"cache_root":"/tmp"}"#,
    )
    .expect("stale state");
    let listener =
        platform::bind_blocking(&platform::daemon_endpoint(env.cache())).expect("stale socket");
    drop(listener);

    let pid = env.start();
    assert_ne!(pid, 999_999);
    assert!(pid_alive(pid));
    env.stop();
}

#[test]
fn grace_shutdown_after_last_session() {
    let env = Env::new();
    let mut child = env
        .cli()
        .args(["serve", "--detached", "--grace-ms", "300"])
        .spawn()
        .expect("spawn serve");

    assert!(
        wait_until(Duration::from_secs(10), || env.endpoint_up()),
        "socket must appear"
    );

    let session = platform::connect_blocking(&platform::daemon_endpoint(env.cache()))
        .expect("connect session");
    std::thread::sleep(Duration::from_millis(100));
    drop(session);

    assert!(
        wait_until(Duration::from_secs(5), || !env.endpoint_up()),
        "daemon must exit after grace"
    );
    let status = child.wait().expect("wait child");
    assert!(status.success(), "graceful exit: {status:?}");
}

#[test]
fn connection_flood_is_refused_and_daemon_survives() {
    let env = Env::new();
    env.start();

    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        held.push(
            platform::connect_blocking(&platform::daemon_endpoint(env.cache()))
                .expect("connect within cap"),
        );
    }
    // Give the accept loop time to consume every slot before the extra one.
    std::thread::sleep(Duration::from_millis(500));

    let extra = platform::connect_blocking(&platform::daemon_endpoint(env.cache()))
        .expect("connect handshake");
    extra
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut extra = extra;
    let hello = encode(&Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        build_id: "flood".to_owned(),
        client: "flood".to_owned(),
    })
    .expect("encode");
    let _ = extra.write_all(&hello);
    let mut line = String::new();
    match BufReader::new(extra).read_line(&mut line) {
        Ok(0) | Err(_) => {}
        Ok(_) => panic!("over-cap connection must be shed, got: {line}"),
    }

    // Freeing one slot restores service.
    drop(held.pop());
    std::thread::sleep(Duration::from_millis(100));
    let mut client = Client::connect(env.cache()).expect("client under cap");
    client.handshake_registry().expect("hello under cap");
    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert!(status["projects"].is_array());
    drop(held);
    env.stop();
}

#[test]
fn sync_jobs_retention_via_enqueue() {
    let env = Env::new();
    let project_dir = TempDir::new().expect("project");
    let project = project_dir.path();
    fs::write(project.join("a.md"), "# A\n").expect("doc");
    let output = env
        .cli()
        .args(["index"])
        .arg(project)
        .output()
        .expect("index");
    assert!(output.status.success(), "index failed: {output:?}");
    env.start();

    let mut client = Client::connect(env.cache()).expect("connect");
    client.handshake(project).expect("bind session");
    for _ in 0..(MAX_SYNC_JOBS + 5) {
        let job = client
            .call_tool("sync_start", serde_json::json!({}))
            .expect("sync_start");
        let job_id = job["job_id"].as_i64().expect("job id");
        loop {
            let status = client
                .call_tool("sync_status", serde_json::json!({ "job_id": job_id }))
                .expect("sync_status");
            let state = status["state"].as_str().unwrap_or_default();
            if state == "done" || state == "error" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    drop(client);

    let conn = rusqlite::Connection::open(env.cache().join(DB_FILE)).expect("db");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_jobs", [], |row| row.get(0))
        .expect("count");
    assert!(
        usize::try_from(count).unwrap_or(usize::MAX) <= MAX_SYNC_JOBS,
        "retention must bound sync_jobs, got {count}"
    );
    env.stop();
}

#[cfg(unix)]
#[test]
fn stop_escalates_with_sigterm_when_wedged() {
    let env = Env::new();
    let project_dir = TempDir::new().expect("project");
    let project = project_dir.path().to_path_buf();
    let body = "a".repeat(6_999_000);
    fs::write(project.join("big.md"), format!("# Big\n\n{body}\n")).expect("doc");
    fs::write(
        project.join(".docsbase.toml"),
        "max_file_size = 7_000_000\n",
    )
    .expect("config");
    let output = env
        .cli()
        .args(["index"])
        .arg(&project)
        .output()
        .expect("index");
    assert!(output.status.success(), "index failed: {output:?}");
    env.start();
    let pid = read_pid(env.cache());

    // Raw client: complete the handshake, request the big document and then
    // stop reading so the daemon's response write blocks.
    let stream =
        platform::connect_blocking(&platform::daemon_endpoint(env.cache())).expect("connect");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    writer
        .write_all(
            &encode(&Request::Hello {
                protocol_version: PROTOCOL_VERSION,
                build_id: docsbase_memory::ipc::protocol::build_id(),
                client: "wedge".to_owned(),
            })
            .expect("encode hello"),
        )
        .expect("hello");
    let mut line = String::new();
    reader.read_line(&mut line).expect("hello reply");
    writer
        .write_all(
            &encode(&Request::RegisterSession {
                pid: std::process::id(),
                cwd: project.clone(),
            })
            .expect("encode register"),
        )
        .expect("register");
    line.clear();
    reader.read_line(&mut line).expect("register reply");
    writer
        .write_all(
            &encode(&Request::CallTool {
                name: "get_doc".to_owned(),
                args: serde_json::json!({ "path": "big.md" }),
            })
            .expect("encode call"),
        )
        .expect("call");
    std::thread::sleep(Duration::from_millis(700));

    let mut stopper = Client::connect(env.cache()).expect("stopper");
    stopper.handshake_registry().expect("hello");
    let stop = std::thread::spawn(move || {
        let _ = stopper.call(Request::StopDaemon);
    });
    // The graceful drain elapses; the daemon is now waiting for the wedged
    // request. A signal must abort that wait instead of being swallowed.
    std::thread::sleep(Duration::from_secs(6));
    if !process_alive(pid) {
        // This host auto-tuned the socket buffers enough to absorb the whole
        // response, so the write never blocks and the daemon stops cleanly.
        // Typical CI runners keep the default 208 KiB buffers and do wedge.
        eprintln!("skipping: socket buffers absorbed the response; no wedge on this host");
        let _ = stop.join();
        return;
    }

    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut dead = false;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            dead = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(dead, "SIGTERM must abort the wedged drain");
    let _ = stop.join();
    drop(writer);
    drop(reader);
}

#[test]
fn daemon_log_rotates_at_spawn() {
    let env = Env::new();
    let logs = env.cache().join("logs");
    fs::create_dir_all(&logs).expect("logs dir");
    let log = logs.join("daemon.log");
    let file = fs::File::create(&log).expect("create log");
    file.set_len(docsbase_memory::limits::MAX_LOG_BYTES + 1)
        .expect("grow log");
    drop(file);

    env.start();
    assert!(
        logs.join("daemon.log.old").is_file(),
        "rotated copy expected"
    );
    let current = fs::metadata(&log).expect("current log");
    assert!(
        current.len() <= docsbase_memory::limits::MAX_LOG_BYTES,
        "fresh log must be small, got {}",
        current.len()
    );
    env.stop();
}

#[test]
fn oversized_daemon_state_recovers() {
    let env = Env::new();
    let state = env.cache().join("state");
    fs::create_dir_all(&state).expect("state dir");
    let file = fs::File::create(state.join("daemon.json")).expect("create state");
    file.set_len(docsbase_memory::limits::MAX_STATE_BYTES + 1)
        .expect("grow state");
    drop(file);

    // Oversized state is corrupt input: it is discarded (recovery) instead of
    // being loaded into memory.
    env.start();
    let rewritten = fs::metadata(state.join("daemon.json")).expect("rewritten state");
    assert!(
        rewritten.len() <= docsbase_memory::limits::MAX_STATE_BYTES,
        "state must be rewritten small, got {}",
        rewritten.len()
    );
    env.stop();
}
