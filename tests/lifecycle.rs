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
}
