use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::platform;

struct Env {
    cache: TempDir,
    root: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        fs::create_dir_all(cache.path().join("config")).expect("config dir");
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

    fn global_config(&self) -> PathBuf {
        self.cache().join("config/config.toml")
    }

    fn project_config(&self) -> PathBuf {
        self.root().join(".docsbase.toml")
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
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
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
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

    fn restart_daemon(&mut self) {
        stop_daemon(self.cache()).expect("stop daemon");
        self.start_daemon();
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
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn doc(body: &str) -> String {
    let mut text = format!("# Title\n\n{body}\n");
    for section in 0..3 {
        let _ = writeln!(text, "\n## Section {section}\n\n{body} details.");
    }
    text
}

fn index(client: &mut Client, env: &Env) -> Value {
    client
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect("index_project")
}

fn search(client: &mut Client, query: &str) -> Vec<String> {
    let hits = client
        .call_tool("search_docs", serde_json::json!({ "query": query }))
        .expect("search_docs");
    let mut paths: Vec<String> = hits
        .as_array()
        .expect("hits array")
        .iter()
        .map(|hit| hit["path"].as_str().expect("path").to_owned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[test]
fn ignores_from_project_config() {
    let mut env = Env::new(&[
        ("keep.md", &doc("publicwidget")),
        ("secret/hide.md", &doc("secretwidget")),
    ]);
    fs::write(env.project_config(), "ignores = [\"secret/**\"]\n").expect("project config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind");
    assert_eq!(search(&mut bound, "publicwidget"), vec!["keep.md"]);
    assert!(
        search(&mut bound, "secretwidget").is_empty(),
        "project ignore must exclude secret/**"
    );
}

#[test]
fn project_config_applied_on_open() {
    let mut env = Env::new(&[
        ("aaa/a.md", &doc("alphawidget")),
        ("bbb/b.md", &doc("betawidget")),
    ]);
    fs::write(env.project_config(), "ignores = [\"aaa/**\"]\n").expect("project config v1");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);

    let mut first = env.client();
    first.handshake(env.root()).expect("first open");
    assert!(
        search(&mut first, "alphawidget").is_empty(),
        "v1 ignores aaa"
    );

    // Project config changes are only picked up when the project is opened
    // again (OQ-6): the open session keeps the configuration it opened with.
    fs::write(env.project_config(), "ignores = [\"bbb/**\"]\n").expect("project config v2");
    write_file(env.root(), "aaa/a.md", doc("alphawidget2").as_bytes());
    write_file(env.root(), "bbb/b.md", doc("betawidget2").as_bytes());
    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);
    assert!(
        search(&mut first, "alphawidget2").is_empty(),
        "mid-session config edit must not apply"
    );
    assert_eq!(
        search(&mut first, "betawidget2"),
        vec!["bbb/b.md"],
        "changed but still-indexed file is refreshed"
    );

    let mut second = env.client();
    second.handshake(env.root()).expect("reopen");
    index(&mut second, &env);
    assert_eq!(search(&mut second, "alphawidget2"), vec!["aaa/a.md"]);
    assert!(
        search(&mut second, "betawidget2").is_empty(),
        "reopened project must use v2 ignores"
    );
}

#[test]
fn global_change_requires_restart_hint() {
    let mut env = Env::new(&[
        ("keep.md", &doc("publicwidget")),
        ("ggg/g.md", &doc("gammawidget")),
    ]);
    fs::write(env.global_config(), "ignores = [\"ggg/**\"]\n").expect("global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let before = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(before["restart_required"], false, "status: {before}");
    index(&mut registry, &env);

    let mut bound = env.client();
    bound.handshake(env.root()).expect("bind");
    assert!(
        search(&mut bound, "gammawidget").is_empty(),
        "global v1 ignores ggg"
    );

    fs::write(env.global_config(), "# changed\n").expect("global config v2");
    let status = bound
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice string");
    assert!(
        notice.contains("daemon stop"),
        "notice must point at `docsbase daemon stop`: {notice}"
    );

    // The running daemon keeps the configuration it started with: the project
    // still ignores ggg until it is restarted.
    assert!(
        search(&mut bound, "gammawidget").is_empty(),
        "running daemon must keep the startup global config"
    );
}

#[test]
fn broken_global_config_blocks_open_until_restart() {
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    fs::write(env.global_config(), "not = [valid\n").expect("break global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice");
    assert!(
        notice.contains("invalid") && notice.contains("daemon stop"),
        "notice: {notice}"
    );

    let err = registry
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect_err("broken global config must block indexing");
    assert!(
        err.to_string().contains("daemon stop"),
        "unexpected error: {err}"
    );

    fs::remove_file(env.global_config()).expect("remove broken config");
    let still = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(
        still["restart_required"], true,
        "a fixed file still needs a restart: {still}"
    );

    env.restart_daemon();
    let mut fresh = env.client();
    fresh.handshake_registry().expect("registry hello");
    index(&mut fresh, &env);
    let status = fresh
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], false, "status: {status}");
}

#[test]
fn watcher_uses_reopened_config() {
    let mut env = Env::new(&[
        ("aaa/a.md", &doc("alphawidget")),
        ("bbb/b.md", &doc("betawidget")),
    ]);
    fs::write(env.project_config(), "ignores = [\"aaa/**\"]\n").expect("project config v1");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    index(&mut registry, &env);
    let mut first = env.client();
    first.handshake(env.root()).expect("first open");

    // Run one watcher batch under v1 so the filter caches bbb as indexable;
    // the v2 swap must invalidate that cache too.
    write_file(env.root(), "bbb/seed.md", doc("seedwidget").as_bytes());
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && search(&mut first, "seedwidget").is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !search(&mut first, "seedwidget").is_empty(),
        "v1 watcher must index bbb/seed.md"
    );

    fs::write(env.project_config(), "ignores = [\"bbb/**\"]\n").expect("project config v2");
    let mut second = env.client();
    second
        .handshake(env.root())
        .expect("reopen swaps watcher config");
    write_file(env.root(), "aaa/a.md", doc("alphawidget9").as_bytes());
    write_file(env.root(), "bbb/b.md", doc("betawidget9").as_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut found = false;
    while Instant::now() < deadline {
        if search(&mut second, "alphawidget9") == vec!["aaa/a.md"] {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(found, "watcher must index newly unignored aaa/**");
    assert!(
        search(&mut second, "betawidget9").is_empty(),
        "watcher must not index newly ignored bbb/**"
    );
}

#[test]
fn max_file_size_above_frame_cap_is_rejected() {
    let mut env = Env::new(&[("keep.md", &doc("publicwidget"))]);
    fs::write(env.global_config(), "max_file_size = 99999999\n").expect("global config");
    env.start_daemon();

    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    let status = registry
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert_eq!(status["restart_required"], true, "status: {status}");
    let notice = status["notice"].as_str().expect("notice");
    assert!(
        notice.contains("max_file_size"),
        "notice must name the limit: {notice}"
    );

    let err = registry
        .call_tool(
            "index_project",
            serde_json::json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect_err("out-of-frame max_file_size must block indexing");
    assert!(
        err.to_string().contains("max_file_size"),
        "unexpected error: {err}"
    );
}
