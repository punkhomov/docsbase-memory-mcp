#![cfg(target_os = "linux")]
//! Offline guarantee (NFR-5, SC-8): the daemon and CLI workflow must work
//! without network access and must not hold any TCP/UDP sockets.
//!
//! When user namespaces allow it the whole workflow runs inside a fresh
//! network namespace (`unshare -rn`); regardless of that, the daemon's open
//! file descriptors are checked against `/proc/net/{tcp,tcp6,udp,udp6}` so a
//! stray `AF_INET` socket fails the test even without namespace support.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::{daemon_pid, stop_daemon};
use docsbase_memory::ipc::client::Client;
use docsbase_memory::platform;

struct Env {
    cache: TempDir,
    root: TempDir,
    daemon: Option<Child>,
    namespace: bool,
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
            namespace: unshare_available(),
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn cmd(&self) -> Command {
        let mut command = if self.namespace {
            let mut command = Command::new("unshare");
            command.args(["-rn"]);
            command
        } else {
            Command::new(daemon_bin())
        };
        if self.namespace {
            command.arg(daemon_bin());
        }
        command
            .env("DOCSBASE_CACHE_DIR", self.cache())
            .env("DOCSBASE_CONFIG_DIR", self.cache().join("config"))
            .current_dir(self.root());
        command
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
            wait_until(Duration::from_secs(10), || platform::exists(
                &platform::daemon_endpoint(self.cache())
            )),
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

fn unshare_available() -> bool {
    Command::new("unshare")
        .args(["-rn", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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

/// Socket inodes held by `pid` that appear in *that process's* TCP/UDP
/// tables (read from `/proc/<pid>/net/...`, so a network namespace cannot
/// hide them); any hit means an `AF_INET`/`AF_INET6` socket.
fn inet_socket_inodes(pid: u32) -> Vec<String> {
    let mut inet = HashSet::new();
    let mut readable = false;
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let path = format!("/proc/{pid}/net/{table}");
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        readable = true;
        for line in text.lines().skip(1) {
            if let Some(inode) = line.split_whitespace().nth(9) {
                inet.insert(inode.to_owned());
            }
        }
    }
    assert!(
        readable,
        "cannot inspect /proc/{pid}/net: offline check would fail open"
    );
    let dir = format!("/proc/{pid}/fd");
    let entries = fs::read_dir(&dir).unwrap_or_else(|err| panic!("cannot inspect {dir}: {err}"));
    let mut held = Vec::new();
    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let name = target.to_string_lossy();
        let Some(inode) = name
            .strip_prefix("socket:[")
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            continue;
        };
        if inet.contains(inode) {
            held.push(inode.to_owned());
        }
    }
    held
}

#[test]
fn no_network_syscalls() {
    let mut env = Env::new(&[
        ("docs/a.md", "# Alpha\n\nalpha networkwidget prose.\n"),
        ("docs/b.md", "# Beta\n\nbeta networkwidget prose.\n"),
    ]);
    eprintln!(
        "offline: network namespace {}",
        if env.namespace {
            "enabled (unshare -rn)"
        } else {
            "unavailable, falling back to fd scanning"
        }
    );
    env.start_daemon();

    // Full workflow: index (CLI), search/status via IPC, then verify fds.
    let output = env
        .cmd()
        .args(["index"])
        .output()
        .expect("run index inside the workflow");
    assert!(
        output.status.success(),
        "index must work offline: {output:?}"
    );

    let mut client = env.client();
    client.handshake(env.root()).expect("bind");
    let hits = client
        .call_tool(
            "search_docs",
            serde_json::json!({ "query": "networkwidget" }),
        )
        .expect("offline search");
    assert_eq!(
        hits.as_array().expect("hits").len(),
        2,
        "both docs searchable offline"
    );
    client
        .call_tool("status", serde_json::json!({}))
        .expect("offline status");

    let pid = daemon_pid(env.cache()).expect("daemon pid");
    let held = inet_socket_inodes(pid);
    assert!(held.is_empty(), "daemon holds TCP/UDP sockets: {held:?}");

    stop_daemon(env.cache()).expect("clean stop");
    let conflict_log = env.cache().join("logs/conflicts.ndjson");
    assert!(
        !conflict_log.exists(),
        "offline workflow must not log conflicts"
    );
}
