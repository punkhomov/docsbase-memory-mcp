use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use docsbase_memory::daemon::admission::Lease;
use docsbase_memory::daemon::lifecycle::{ensure_daemon_with, stop_daemon};
use docsbase_memory::error::Error;
use docsbase_memory::ipc::protocol::build_id;
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

    fn conflicts(&self) -> String {
        fs::read_to_string(self.cache().join("logs/conflicts.ndjson")).unwrap_or_default()
    }

    fn write_state(&self, build: &str, schema: u32) {
        let state = serde_json::json!({
            "pid": std::process::id(),
            "socket": self.socket(),
            "build_id": build,
            "schema_version": schema,
            "cache_root": self.cache(),
        });
        fs::create_dir_all(self.cache().join("state")).expect("state dir");
        fs::write(
            self.cache().join("state/daemon.json"),
            serde_json::to_vec(&state).expect("json"),
        )
        .expect("write state");
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
fn build_mismatch_refuses_and_logs() {
    let env = Env::new();
    env.write_state("docsbase 0.0.0-old", 1);

    let err = Lease::acquire(env.cache(), &build_id(), 1).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert_eq!(err.mcp_code(), -32010);

    let log = env.conflicts();
    assert!(log.contains("build_mismatch"), "{log}");
    assert!(log.contains("docsbase 0.0.0-old"), "{log}");
}

#[test]
fn schema_mismatch_refuses() {
    let env = Env::new();
    env.write_state(&build_id(), 99);

    let err = Lease::acquire(env.cache(), &build_id(), 1).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert!(
        env.conflicts().contains("schema_mismatch"),
        "{}",
        env.conflicts()
    );
}

#[test]
fn second_daemon_refused() {
    let env = Env::new();
    ensure_daemon_with(env.cache(), Some(&daemon_bin())).expect("start");

    let err = Lease::acquire(env.cache(), &build_id(), 1).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert!(env.conflicts().contains("lock_busy"), "{}", env.conflicts());

    stop_daemon(env.cache()).expect("stop");
}

#[test]
fn lock_recovered_after_kill() {
    let env = Env::new();
    let mut child = env
        .cli()
        .args(["serve", "--detached", "--grace-ms", "1000"])
        .spawn()
        .expect("spawn daemon");
    assert!(
        wait_until(Duration::from_secs(10), || env.socket().exists()),
        "socket must appear"
    );

    child.kill().expect("kill -9");
    child.wait().expect("reap");

    let lease = Lease::acquire(env.cache(), &build_id(), 1).expect("recover lock");
    drop(lease);

    stop_daemon(env.cache()).expect("stop is a no-op for a dead daemon");
}
