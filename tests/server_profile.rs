use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::Request;
use docsbase_memory::platform;

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cli(cache: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_docsbase"));
    command
        .env("DOCSBASE_CACHE_DIR", cache)
        .env("DOCSBASE_CONFIG_DIR", config)
        .env("DOCSBASE_DATA_DIR", cache.join("data"));
    command
}

fn await_started(daemon: &mut Daemon, cache: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            let mut diagnostic = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut diagnostic)
                .unwrap();
            panic!("indexed Website startup failed: {status}: {diagnostic}");
        }
        if platform::exists(&platform::daemon_endpoint(cache))
            && let Some(mut client) = Client::connect(cache)
            && client.handshake_registry().is_ok()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not publish its listener"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn indexed_global_website_profile_starts_with_zero_owned_watchers() {
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("guide.md"),
        "# Profile\n\nprofilewidget is searchable.\n",
    )
    .unwrap();
    let indexed = cli(cache.path(), config.path())
        .arg("index")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    std::fs::write(
        config.path().join("config.toml"),
        format!(
            "[daemon]\nprofile={{mode='website',project_root={}}}\n",
            toml::Value::String(root.path().to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let mut daemon = Daemon(
        cli(cache.path(), config.path())
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    await_started(&mut daemon, cache.path());
    let mut client = Client::connect(cache.path()).unwrap();
    client.handshake_registry().unwrap();
    let status = client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(status["watchers"], 0);
    client.call(Request::StopDaemon).unwrap();
    drop(client);
    assert!(daemon.0.wait().unwrap().success());
}

#[test]
fn cli_project_selects_a_complete_website_profile_from_unrelated_cwd() {
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("guide.md"), "profilewidget").unwrap();
    let indexed = cli(cache.path(), config.path())
        .arg("index")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let mut daemon = Daemon(
        cli(cache.path(), config.path())
            .arg("serve")
            .arg("--project")
            .arg(root.path().join("."))
            .current_dir(config.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    await_started(&mut daemon, cache.path());
    let mut client = Client::connect(cache.path()).unwrap();
    client.handshake_registry().unwrap();
    let status = client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(status["watchers"], 0);
    assert_eq!(status["projects"].as_array().unwrap().len(), 1);
    client.call(Request::StopDaemon).unwrap();
    drop(client);
    assert!(daemon.0.wait().unwrap().success());
}

struct ProfileEnv {
    cache: tempfile::TempDir,
    root: tempfile::TempDir,
    config: tempfile::TempDir,
}
impl ProfileEnv {
    fn indexed() -> Self {
        let env = Self {
            cache: tempfile::tempdir().unwrap(),
            root: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
        };
        std::fs::write(env.root.path().join("guide.md"), "originalwidget").unwrap();
        let output = cli(env.cache.path(), env.config.path())
            .arg("index")
            .arg(env.root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        env.website();
        env
    }
    fn website(&self) {
        std::fs::write(
            self.config.path().join("config.toml"),
            format!(
                "[daemon]\nprofile={{mode='website',project_root={}}}\n",
                toml::Value::String(self.root.path().to_string_lossy().into_owned())
            ),
        )
        .unwrap();
    }
    fn start(&self, args: &[&str]) -> Daemon {
        let mut daemon = Daemon(
            cli(self.cache.path(), self.config.path())
                .arg("serve")
                .args(args)
                .current_dir(self.config.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        await_started(&mut daemon, self.cache.path());
        daemon
    }
    fn connect(&self) -> Client {
        let mut client = Client::connect(self.cache.path()).unwrap();
        client.handshake_registry().unwrap();
        client
    }
    fn project(&self) -> docsbase_memory::store::models::Project {
        let db = docsbase_memory::store::Db::open_readonly(self.cache.path()).unwrap();
        docsbase_memory::daemon::registry::resolve_by_cwd(&db, self.root.path()).unwrap()
    }
    fn stop(&self, daemon: &mut Daemon) {
        self.connect().call(Request::StopDaemon).unwrap();
        assert!(daemon.0.wait().unwrap().success());
    }
}
fn denied(response: &docsbase_memory::ipc::protocol::Response) {
    assert!(
        matches!(
            response,
            docsbase_memory::ipc::protocol::Response::Error { code: -32010, .. }
        ),
        "{response:?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One real read/edit/sync/restart lifecycle.
fn website_readers_bind_readonly_and_explicit_sync_updates_real_index_with_watchers_zero() {
    let env = ProfileEnv::indexed();
    let mut daemon = env.start(&[]);
    let mut client = env.connect();
    let id = env.project().id;
    denied(
        &client
            .call(Request::RegisterSession {
                pid: std::process::id(),
                cwd: env.root.path().to_owned(),
            })
            .unwrap(),
    );
    denied(
        &client
            .call(Request::RegisterUnbound {
                pid: std::process::id(),
                cwd: env.root.path().to_owned(),
            })
            .unwrap(),
    );
    for project_id in [None, Some(id + 1)] {
        denied(
            &client
                .call(Request::BindReadOnly {
                    pid: std::process::id(),
                    project_id,
                })
                .unwrap(),
        );
    }
    client
        .call(Request::BindReadOnly {
            pid: std::process::id(),
            project_id: Some(id),
        })
        .unwrap();
    let before = client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(before["sessions"], 1);
    assert_eq!(before["watchers"], 0);
    let search = cli(env.cache.path(), env.config.path())
        .current_dir(env.root.path())
        .args(["search", "originalwidget"])
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "readonly Website search: {search:?}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&search.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::write(env.root.path().join("guide.md"), "replacementwidget").unwrap();
    // Owned watcher state and the start-gate tests establish that no event
    // producer exists. Read committed index immediately; no negative sleep.
    {
        let index = docsbase_memory::index::tantivy_index::ReadIndex::open(
            &env.cache
                .path()
                .join("projects")
                .join(id.to_string())
                .join("tantivy"),
        )
        .unwrap();
        assert_eq!(index.search("replacementwidget", 10).unwrap().len(), 0);
        assert_eq!(index.search("originalwidget", 10).unwrap().len(), 1);
    }
    let sync = cli(env.cache.path(), env.config.path())
        .current_dir(env.config.path())
        .arg("sync")
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(sync.status.success(), "{sync:?}");
    {
        let index = docsbase_memory::index::tantivy_index::ReadIndex::open(
            &env.cache
                .path()
                .join("projects")
                .join(id.to_string())
                .join("tantivy"),
        )
        .unwrap();
        assert_eq!(index.search("replacementwidget", 10).unwrap().len(), 1);
        assert_eq!(index.search("originalwidget", 10).unwrap().len(), 0);
    }
    let after = client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(after["watchers"], 0);
    assert!(
        after["projects"][0]["index_generation"].as_i64().unwrap()
            > before["projects"][0]["index_generation"].as_i64().unwrap()
    );
    assert!(!env.cache.path().join("vectors.db").exists());
    assert!(
        !env.cache
            .path()
            .join("projects")
            .join(id.to_string())
            .join("ann")
            .exists()
    );
    drop(client);
    env.stop(&mut daemon);
    let mut restarted = env.start(&[]);
    let status = env
        .connect()
        .call_tool("status", serde_json::json!({}))
        .unwrap();
    assert_eq!(status["watchers"], 0);
    assert_eq!(status["projects"].as_array().unwrap().len(), 1);
    env.stop(&mut restarted);
}

#[test]
fn explicit_shared_replaces_global_website_and_invalid_selectors_never_start() {
    let env = ProfileEnv::indexed();
    let mut shared = env.start(&["--profile", "shared", "--grace-ms", "60000"]);
    let status = env
        .connect()
        .call_tool("status", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        status["watchers"], 1,
        "Shared retains owned watcher behavior"
    );
    env.stop(&mut shared);
    for args in [
        vec!["--profile", "website"],
        vec![
            "--profile",
            "shared",
            "--project",
            env.root.path().to_str().unwrap(),
        ],
        vec!["--detached"],
        vec!["--grace-ms", "0"],
        vec!["--project", "relative"],
    ] {
        let output = cli(env.cache.path(), env.config.path())
            .arg("serve")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(Client::connect(env.cache.path()).is_none());
    }
}

#[test]
fn wrong_schema_cli_admission_creates_no_conflict_listener_or_runtime_state() {
    let env = ProfileEnv::indexed();
    let path = env.cache.path().join("registry.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .pragma_update(
            None,
            "user_version",
            docsbase_memory::store::migrations::SCHEMA_VERSION + 1,
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    let output = cli(env.cache.path(), env.config.path())
        .arg("serve")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(path).unwrap(), before);
    for name in ["state", "logs", "vectors.db"] {
        assert!(!env.cache.path().join(name).exists(), "unexpected {name}");
    }
    assert!(Client::connect(env.cache.path()).is_none());
}

#[test]
fn cli_website_root_overlay_selects_an_independently_indexed_other_project() {
    let env = ProfileEnv::indexed();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("guide.md"), "otherwidget").unwrap();
    let output = cli(env.cache.path(), env.config.path())
        .arg("index")
        .arg(other.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut daemon = env.start(&[
        "--profile",
        "website",
        "--project",
        other.path().to_str().unwrap(),
    ]);
    let status = env
        .connect()
        .call_tool("status", serde_json::json!({}))
        .unwrap();
    assert_eq!(status["projects"].as_array().unwrap().len(), 1);
    let root = status["projects"][0]["root"].as_str().unwrap();
    assert_eq!(
        Path::new(root).canonicalize().unwrap(),
        other.path().canonicalize().unwrap()
    );
    assert_eq!(status["watchers"], 0);
    let mut client = env.connect();
    denied(
        &client
            .call(Request::BindReadOnly {
                pid: std::process::id(),
                project_id: Some(env.project().id),
            })
            .unwrap(),
    );
    drop(client);
    env.stop(&mut daemon);
}
