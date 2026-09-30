use std::fmt::Write as _;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::{daemon_pid, stop_daemon};
use docsbase_memory::ipc::client::Client;
use docsbase_memory::platform;

struct Env {
    cache: TempDir,
    root: TempDir,
    data: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        let data = TempDir::new().expect("data");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        Self {
            cache,
            root,
            data,
            daemon: None,
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn data(&self) -> &Path {
        self.data.path()
    }

    fn binary(&self) -> PathBuf {
        self.data()
            .join("bin")
            .join(format!("docsbase{}", std::env::consts::EXE_SUFFIX))
    }

    fn manifest(&self) -> PathBuf {
        self.data().join("install.json")
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command
            .env("DOCSBASE_CACHE_DIR", self.cache())
            .env("DOCSBASE_CONFIG_DIR", self.cache().join("config"))
            .env("DOCSBASE_DATA_DIR", self.data())
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

    fn install(&self) -> Value {
        let output = self.cmd().arg("install").output().expect("run install");
        assert!(
            output.status.success(),
            "install failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = fs::read(self.manifest()).expect("manifest");
        serde_json::from_slice(&bytes).expect("manifest json")
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

    fn start_daemon(&mut self) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || {
                platform::exists(&platform::daemon_endpoint(self.cache()))
                    && self.cache().join("state/daemon.json").exists()
            }),
            "socket and daemon state must appear"
        );
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

/// Windows canonicalize yields verbatim `\\?\` paths; compare like values.
fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn process_alive(pid: u32) -> bool {
    docsbase_memory::platform::process::process_alive(pid)
}

fn daemon_state_pid(cache: &Path) -> u32 {
    let bytes = fs::read(cache.join("state/daemon.json")).expect("daemon.json");
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    u32::try_from(json["pid"].as_u64().expect("pid")).expect("pid fits")
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

const DOC: &str = "# Guide\n\ninstaller prose about widgets.\n";

#[test]
fn install_owned_artifacts() {
    let env = Env::new(&[("README.md", DOC)]);
    let manifest = env.install();

    let binary = env.binary();
    assert!(binary.is_file(), "binary must be installed");
    #[cfg(unix)]
    {
        let perms = fs::metadata(&binary).expect("metadata").permissions();
        assert_eq!(perms.mode() & 0o777, 0o755, "binary must be executable");
    }
    assert_eq!(manifest["binary"], serde_json::json!(canon(&binary)));
    assert_eq!(
        manifest["cache_root"],
        serde_json::json!(canon(env.cache()))
    );
    assert_eq!(manifest["schema_version"], 1);
    assert!(
        manifest["build_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("docsbase")
    );
    assert!(env.manifest().is_file());

    // Idempotent re-install.
    env.install();
    assert!(binary.is_file());
}

#[test]
fn update_stops_and_waits() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let mut client = env.client();
    client.handshake(env.root()).expect("bind session");

    // Simulate an older installed binary.
    let bin_dir = env.data().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    fs::write(env.binary(), b"old binary").expect("old binary");

    let pid = daemon_state_pid(env.cache());
    let manifest = env.install();
    assert_eq!(manifest["binary"], serde_json::json!(canon(&env.binary())));
    assert!(
        wait_until(Duration::from_secs(2), || !process_alive(pid)),
        "install must wait for the daemon process to exit"
    );
    assert!(daemon_pid(env.cache()).is_none(), "no daemon may remain");
    assert!(
        !platform::exists(&platform::daemon_endpoint(env.cache())),
        "socket must be gone after install"
    );
    let installed = fs::read(env.binary()).expect("read installed binary");
    let source = fs::read(daemon_bin()).expect("read source binary");
    assert_eq!(installed, source, "binary must be replaced atomically");
}

#[test]
fn uninstall_lists_indexes_with_confirmation() {
    let env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.install();

    let output = env.cmd().arg("uninstall").output().expect("run uninstall");
    assert!(output.status.success(), "uninstall info must succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("indexed projects:"), "stdout: {stdout}");
    assert!(
        stdout.contains(
            env.root()
                .file_name()
                .expect("name")
                .to_str()
                .expect("utf8")
        ),
        "project must be listed: {stdout}"
    );
    assert!(stdout.contains("--yes"), "confirmation hint: {stdout}");
    assert!(env.binary().is_file(), "no deletion without --yes");
    assert!(env.manifest().is_file(), "no deletion without --yes");
    assert!(env.cache().exists(), "no deletion without --yes");

    let output = env
        .cmd()
        .args(["uninstall", "--yes"])
        .output()
        .expect("run uninstall --yes");
    assert!(output.status.success(), "uninstall --yes must succeed");
    assert!(!env.binary().exists(), "binary must be removed");
    assert!(!env.manifest().exists(), "manifest must be removed");
    assert!(!env.cache().exists(), "cache root must be removed");
}

#[test]
fn uninstall_preserves_foreign() {
    let env = Env::new(&[("README.md", DOC)]);
    env.install();
    let foreign = env.data().join("bin/other-tool");
    fs::write(&foreign, b"foreign").expect("foreign file");

    let output = env
        .cmd()
        .args(["uninstall", "--yes"])
        .output()
        .expect("run uninstall");
    assert!(output.status.success(), "uninstall must succeed");
    assert!(foreign.is_file(), "foreign binary must survive");
    assert!(!env.binary().exists(), "owned binary must be removed");
}

#[test]
fn update_waits_for_running_job() {
    let mut env = Env::new(&[]);
    let mut files = Vec::new();
    for index in 0..300 {
        let mut body = format!("# File {index}\n\nintro prose {index}.\n");
        for section in 0..30 {
            let _ = writeln!(
                body,
                "\n## Section {section}\n\nwidget prose {index}-{section}."
            );
        }
        files.push((format!("docs/file-{index}.md"), body));
    }
    for (rel, body) in &files {
        write_file(env.root(), rel, body.as_bytes());
    }
    env.index();
    env.start_daemon();
    let pid = daemon_state_pid(env.cache());
    let mut client = env.client();
    client.handshake(env.root()).expect("bind session");
    client
        .call_tool("sync_start", serde_json::json!({}))
        .expect("start sync job");

    // The full sync job runs in the daemon; install must wait for it and the
    // process before swapping the binary (FR-5).
    let manifest = env.install();
    assert_eq!(manifest["binary"], serde_json::json!(canon(&env.binary())));
    assert!(
        wait_until(Duration::from_secs(2), || !process_alive(pid)),
        "install must wait for the busy daemon to exit"
    );
    let installed = fs::read(env.binary()).expect("read installed binary");
    let source = fs::read(daemon_bin()).expect("read source binary");
    assert_eq!(installed, source);
}

#[test]
fn concurrent_stops_do_not_wedge() {
    let mut env = Env::new(&[("README.md", DOC)]);
    env.index();
    env.start_daemon();
    let pid = daemon_state_pid(env.cache());

    let mut stops = Vec::new();
    for _ in 0..4 {
        let mut command = env.cmd();
        stops.push(std::thread::spawn(move || {
            command
                .args(["daemon", "stop"])
                .output()
                .expect("run stop")
                .status
                .success()
        }));
    }
    for stop in stops {
        assert!(stop.join().expect("join"), "concurrent stop must succeed");
    }
    assert!(
        wait_until(Duration::from_secs(3), || !process_alive(pid)),
        "daemon must exit after concurrent stops"
    );
}

#[test]
fn update_waits_for_synchronous_job() {
    let mut env = Env::new(&[]);
    for index in 0..3000 {
        let mut body = format!("# File {index}\n\nintro prose {index}.\n");
        for section in 0..40 {
            let _ = writeln!(
                body,
                "\n## Section {section}\n\nwidget prose {index}-{section}."
            );
        }
        write_file(
            env.root(),
            &format!("docs/file-{index}.md"),
            body.as_bytes(),
        );
    }
    env.start_daemon();
    let pid = daemon_state_pid(env.cache());

    let mut client = env.client();
    client
        .set_read_timeout(Some(Duration::from_secs(120)))
        .expect("timeout");
    // The project is not registered yet: this call runs the *first* full index
    // through the daemon, which takes far longer than the 5 s stop timeout.
    client.handshake_registry().expect("hello");
    let root = env.root().to_path_buf();
    let job = std::thread::spawn(move || {
        client.call_tool(
            "index_project",
            serde_json::json!({ "path": root.to_str().expect("utf8") }),
        )
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!job.is_finished(), "the full index must still be running");

    let started = Instant::now();
    let manifest = env.install();
    let waited = started.elapsed();
    assert_eq!(manifest["binary"], serde_json::json!(canon(&env.binary())));
    assert!(
        waited >= Duration::from_secs(2),
        "install must wait for the running job, waited {waited:?}"
    );
    let job_result = job.join().expect("join job thread");
    assert!(
        job_result.is_ok(),
        "in-flight job must complete: {job_result:?}"
    );
    assert!(
        wait_until(Duration::from_secs(3), || !process_alive(pid)),
        "install must wait for the running job and process exit"
    );
    let installed = fs::read(env.binary()).expect("read installed binary");
    let source = fs::read(daemon_bin()).expect("read source binary");
    assert_eq!(installed, source);
}

#[test]
fn uninstall_after_cache_removed_still_removes_binary() {
    let env = Env::new(&[("README.md", DOC)]);
    env.install();
    fs::remove_dir_all(env.cache()).expect("remove cache");

    let output = env
        .cmd()
        .args(["uninstall", "--yes"])
        .output()
        .expect("run uninstall");
    assert!(
        output.status.success(),
        "uninstall must cope with a missing cache: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!env.binary().exists(), "owned binary must be removed");
    assert!(!env.manifest().exists(), "manifest must be removed");
}

#[test]
fn install_recovers_from_stale_state() {
    let env = Env::new(&[("README.md", DOC)]);
    // Stale daemon.json from a crashed (older) daemon whose pid is now reused
    // by a live process: install must clear it and proceed.
    let state_dir = env.cache().join("state");
    fs::create_dir_all(&state_dir).expect("state dir");
    let state = serde_json::json!({
        "pid": std::process::id(),
        "socket": platform::daemon_endpoint(env.cache()).as_path().to_path_buf(),
        "build_id": "docsbase 0.0.0-old",
        "schema_version": 0,
        "cache_root": env.cache(),
    });
    fs::write(
        state_dir.join("daemon.json"),
        serde_json::to_vec(&state).expect("json"),
    )
    .expect("write state");

    let manifest = env.install();
    assert_eq!(manifest["binary"], serde_json::json!(canon(&env.binary())));
    assert!(
        !state_dir.join("daemon.json").exists(),
        "stale state must be cleared"
    );
}
