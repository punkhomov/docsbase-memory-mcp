use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use docsbase_memory::daemon::admission::Lease;
use docsbase_memory::daemon::lifecycle::{ensure_daemon_with, stop_daemon};
use docsbase_memory::error::Error;
use docsbase_memory::ipc::protocol::build_id;
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

    fn socket(&self) -> PathBuf {
        platform::daemon_endpoint(self.cache())
            .as_path()
            .to_path_buf()
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
    let schema = docsbase_memory::store::migrations::SCHEMA_VERSION;
    env.write_state("docsbase 0.0.0-old", schema);

    let err = Lease::acquire(env.cache(), &build_id(), schema).expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert_eq!(err.mcp_code(), -32010);

    let lines = conflict_lines(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["kind"], "build_mismatch");
    assert_eq!(lines[0]["actual"], "docsbase 0.0.0-old");

    #[cfg(unix)]
    {
        let mode = fs::metadata(env.cache().join("logs"))
            .expect("logs metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "conflict log dir must be private");
    }
}

#[test]
fn schema_mismatch_refuses() {
    let env = Env::new();
    env.write_state(&build_id(), 99);

    let err = Lease::acquire(
        env.cache(),
        &build_id(),
        docsbase_memory::store::migrations::SCHEMA_VERSION,
    )
    .expect_err("must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert!(
        env.conflicts().contains("schema_mismatch"),
        "{}",
        env.conflicts()
    );
}

#[test]
fn root_mismatch_refused_and_logs() {
    let env = Env::new();
    let other = TempDir::new().expect("other cache root");
    let state = serde_json::json!({
        "pid": std::process::id(),
        "socket": env.socket(),
        "build_id": build_id(),
        "schema_version": docsbase_memory::store::migrations::SCHEMA_VERSION,
        "cache_root": other.path(),
    });
    fs::create_dir_all(env.cache().join("state")).expect("state dir");
    fs::write(
        env.cache().join("state/daemon.json"),
        serde_json::to_vec(&state).expect("json"),
    )
    .expect("write state");

    let err = Lease::acquire(
        env.cache(),
        &build_id(),
        docsbase_memory::store::migrations::SCHEMA_VERSION,
    )
    .expect_err("root mismatch must refuse");
    assert!(matches!(err, Error::Admission { .. }), "{err:?}");
    assert!(err.to_string().contains("root"), "{err}");

    let lines = conflict_lines(&env);
    assert_eq!(
        lines.last().expect("conflict recorded")["kind"],
        "root_mismatch"
    );
}

#[test]
fn second_daemon_refused() {
    let env = Env::new();
    ensure_daemon_with(env.cache(), Some(&daemon_bin())).expect("start");

    let err = Lease::acquire(
        env.cache(),
        &build_id(),
        docsbase_memory::store::migrations::SCHEMA_VERSION,
    )
    .expect_err("must refuse");
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
        wait_until(Duration::from_secs(10), || {
            platform::exists(&platform::daemon_endpoint(env.cache()))
        }),
        "socket must appear"
    );

    child.kill().expect("kill -9");
    child.wait().expect("reap");

    let lease = Lease::acquire(
        env.cache(),
        &build_id(),
        docsbase_memory::store::migrations::SCHEMA_VERSION,
    )
    .expect("recover lock");
    drop(lease);

    ensure_daemon_with(env.cache(), Some(&daemon_bin())).expect("fresh daemon starts");
    stop_daemon(env.cache()).expect("stop fresh daemon");
}

fn conflict_lines(env: &Env) -> Vec<serde_json::Value> {
    env.conflicts()
        .lines()
        .map(|line| serde_json::from_str(line).expect("ndjson line"))
        .collect()
}

#[test]
fn lease_holds_lock_until_drop() {
    let env = Env::new();
    let schema = docsbase_memory::store::migrations::SCHEMA_VERSION;
    let lease = Lease::acquire(env.cache(), &build_id(), schema).expect("acquire lease");

    let output = env.cli().arg("serve").output().expect("competing daemon");
    assert!(!output.status.success(), "second process must be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("another daemon holds"),
        "{output:?}"
    );

    drop(lease);
    ensure_daemon_with(env.cache(), Some(&daemon_bin())).expect("start after lease drop");
    stop_daemon(env.cache()).expect("stop daemon");
}

/// Count only descriptors for this test's lock, excluding files opened by
/// concurrently running tests and the temporary /proc directory descriptor.
#[cfg(target_os = "linux")]
fn admission_lock_fds(env: &Env) -> usize {
    let path = env
        .cache()
        .canonicalize()
        .expect("canonical cache")
        .join("state/admission.lock");
    fs::read_dir("/proc/self/fd")
        .expect("process descriptors")
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .filter(|target| *target == path)
        .count()
}

#[cfg(target_os = "linux")]
#[test]
fn repeated_acquire_and_drop_closes_lock_file() {
    let env = Env::new();
    let schema = docsbase_memory::store::migrations::SCHEMA_VERSION;
    for _ in 0..32 {
        let lease = Lease::acquire(env.cache(), &build_id(), schema).expect("acquire");
        assert_eq!(admission_lock_fds(&env), 1, "live lease owns one file");
        drop(lease);
        assert_eq!(admission_lock_fds(&env), 0, "drop must close the file");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn busy_acquire_attempts_close_lock_file() {
    let env = Env::new();
    let schema = docsbase_memory::store::migrations::SCHEMA_VERSION;
    let lease = Lease::acquire(env.cache(), &build_id(), schema).expect("incumbent");
    for _ in 0..32 {
        let err = Lease::acquire(env.cache(), &build_id(), schema).expect_err("busy");
        assert!(docsbase_memory::daemon::admission::is_lock_busy(&err));
        assert_eq!(
            admission_lock_fds(&env),
            1,
            "only incumbent may keep a file"
        );
    }
    drop(lease);
    assert_eq!(admission_lock_fds(&env), 0, "incumbent must close its file");
}

#[cfg(target_os = "linux")]
#[test]
fn admission_check_errors_close_lock_file() {
    let schema = docsbase_memory::store::migrations::SCHEMA_VERSION;
    for failure in ["build", "schema", "root", "parse", "read"] {
        let env = Env::new();
        env.write_state(&build_id(), schema);
        let path = env.cache().join("state/daemon.json");
        match failure {
            "parse" => fs::write(&path, "not json").expect("corrupt state"),
            "read" => {
                fs::remove_file(&path).expect("remove state");
                fs::create_dir(&path).expect("unreadable state directory");
            }
            _ => {
                let mut state: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).expect("read state")).expect("json");
                match failure {
                    "build" => state["build_id"] = "old build".into(),
                    "schema" => state["schema_version"] = (schema + 1).into(),
                    "root" => {
                        state["cache_root"] = env.cache().join("other").to_string_lossy().into();
                    }
                    _ => unreachable!(),
                }
                fs::write(&path, serde_json::to_vec(&state).expect("encode")).expect("state");
            }
        }
        for _ in 0..16 {
            let err = Lease::acquire(env.cache(), &build_id(), schema).expect_err(failure);
            assert!(
                match failure {
                    "parse" | "read" => matches!(err, Error::Internal { .. }),
                    _ => matches!(err, Error::Admission { .. }),
                },
                "{failure}: unexpected error: {err}"
            );
            assert!(
                !docsbase_memory::daemon::admission::is_lock_busy(&err),
                "{failure}: failed validation must release the lock: {err}"
            );
            assert_eq!(admission_lock_fds(&env), 0, "{failure}: file must close");
        }
        if failure == "read" {
            fs::remove_dir(&path).expect("remove state directory");
        } else {
            fs::remove_file(&path).expect("remove state");
        }
        drop(Lease::acquire(env.cache(), &build_id(), schema).expect("recover after error"));
        assert_eq!(
            admission_lock_fds(&env),
            0,
            "{failure}: recovered file closes"
        );
    }
}
