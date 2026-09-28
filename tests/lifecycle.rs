use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use docsbase_memory::daemon::lifecycle::{ensure_daemon_with, stop_daemon};
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

    fn socket(&self) -> PathBuf {
        self.cache().join("state/daemon.sock")
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
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    })
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
    assert!(env.socket().exists(), "socket must exist");
    let mode = fs::metadata(env.socket()).expect("metadata").permissions();
    assert_eq!(mode.mode() & 0o777, 0o600, "socket must be 0600");
    assert!(env.cache().join("state/daemon.json").exists());

    env.stop();
    assert!(!env.socket().exists(), "socket removed on stop");
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

    assert!(!env.socket().exists(), "socket removed");
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
    let stale = env.socket();
    let listener = UnixListener::bind(&stale).expect("stale socket");
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
        wait_until(Duration::from_secs(10), || env.socket().exists()),
        "socket must appear"
    );

    let session = UnixStream::connect(env.socket()).expect("connect session");
    std::thread::sleep(Duration::from_millis(100));
    drop(session);

    assert!(
        wait_until(Duration::from_secs(5), || !env.socket().exists()),
        "daemon must exit after grace"
    );
    let status = child.wait().expect("wait child");
    assert!(status.success(), "graceful exit: {status:?}");
}
