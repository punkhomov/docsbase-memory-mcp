use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::Request;
use docsbase_memory::platform;

#[path = "support/transport.rs"]
mod test_transport;

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

fn await_started(transport: &platform::Transport, daemon: &mut Daemon, cache: &Path) {
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
            panic!("daemon startup failed: {status}: {diagnostic}");
        }
        if platform::exists(&platform::daemon_endpoint(cache))
            && let Some(mut client) = Client::connect(transport, cache)
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

#[cfg(unix)]
fn seed_empty_v5(cache: &Path) {
    let mut registry = docsbase_memory::store::Db::open(cache).unwrap();
    let tx = registry.begin_write().unwrap();
    tx.execute_batch(include_str!("../src/store/generation_catalog/schema.sql"))
        .unwrap();
    tx.execute(
        "INSERT INTO generation_store VALUES (1, 1, ?1)",
        [[25_u8; 16].as_slice()],
    )
    .unwrap();
    tx.pragma_update(None, "user_version", 5).unwrap();
    tx.commit().unwrap();
}

#[cfg(unix)]
fn wait_for(mut done: impl FnMut() -> bool, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{context}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn index_project_through_cli(cache: &Path, config: &Path, root: &Path, text: &str) -> i64 {
    std::fs::write(root.join("guide.md"), text).unwrap();
    let output = cli(cache, config)
        .current_dir(root)
        .arg("index")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["project_id"].as_i64().unwrap()
}

#[cfg(unix)]
fn assert_catalog_views(client: &mut Client, updated: i64, state: &str, other: i64) {
    let expected = serde_json::json!({});
    let status = client.call_tool("status", expected.clone()).unwrap();
    assert_eq!(status["watchers"], 2);
    assert_eq!(status["schema_version"], 5);
    for projects in [
        status["projects"].clone(),
        client.call_tool("list_projects", expected.clone()).unwrap(),
        client.call_tool("panel_projects", expected).unwrap(),
    ] {
        let lookup = |id| {
            projects
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .unwrap()
        };
        assert_eq!(lookup(updated)["status"], state);
        assert_eq!(lookup(other)["status"], "indexed");
        assert_eq!(lookup(other)["docs"], 1);
    }
}

#[cfg(unix)]
fn start_shared_daemon(cache: &Path, config: &Path) -> (Daemon, test_transport::TestTransport) {
    let mut daemon = Daemon(
        cli(cache, config)
            .arg("serve")
            .current_dir(config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let transport = test_transport::TestTransport::new(4);
    await_started(&transport, &mut daemon, cache);
    (daemon, transport)
}

#[test]
#[cfg(unix)]
fn shared_v5_watcher_updates_one_project_while_another_remains_searchable() {
    use docsbase_memory::store::DB_FILE;
    use serde_json::json;

    let cache = tempfile::tempdir_in("/tmp").unwrap();
    let config = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("config.toml"),
        "config_version = 2\n[daemon]\ncontrol_active = 1\n",
    )
    .unwrap();
    seed_empty_v5(cache.path());
    let first_id =
        index_project_through_cli(cache.path(), config.path(), first.path(), "earlierlemur");
    let other_id =
        index_project_through_cli(cache.path(), config.path(), other.path(), "separateotter");
    let (_daemon, transport) = start_shared_daemon(cache.path(), config.path());
    let client = |id| {
        let mut client = Client::connect(&transport, cache.path()).unwrap();
        client.handshake_registry().unwrap();
        client
            .call(Request::BindReadOnly {
                pid: std::process::id(),
                project_id: Some(id),
            })
            .unwrap();
        client
    };
    let mut first_client = client(first_id);
    let mut other_client = client(other_id);
    let initial = first_client
        .call_tool(
            "search_docs",
            json!({"query":"earlierlemur","chunk_ref":true}),
        )
        .unwrap();
    let stale = initial[0]["chunk_ref"].as_str().unwrap();
    let db = rusqlite::Connection::open(cache.path().join(DB_FILE)).unwrap();
    let read_head = || {
        db.query_row(
            "SELECT active_id FROM project_generation_head WHERE project_id=?1",
            [first_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let head = read_head();
    let storage = cache.path().join("projects").join(first_id.to_string());
    let lock = fd_lock::RwLock::new(std::fs::File::open(storage.join(".writer.lock")).unwrap());
    let local = rusqlite::Connection::open(
        storage
            .join("generations")
            .join(head.to_string())
            .join("metadata.db"),
    )
    .unwrap();
    local.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(first.path().join("guide.md"), "updatedlemur").unwrap();
    wait_for(
        || lock.try_read().is_err(),
        "first watcher did not enter its writer lease",
    );
    assert_eq!(
        other_client
            .call_tool("search_docs", json!({"query":"separateotter"}))
            .unwrap()[0]["path"],
        "guide.md"
    );
    assert_catalog_views(&mut other_client, first_id, "indexing", other_id);
    local.execute_batch("ROLLBACK").unwrap();
    wait_for(
        || {
            first_client
                .call_tool("search_docs", json!({"query":"updatedlemur"}))
                .is_ok_and(|result| result[0]["path"] == "guide.md")
        },
        "watcher did not publish edited file",
    );
    assert_eq!(read_head(), head);
    let error = first_client
        .call_tool("read_neighbors_at", json!({"chunk_ref":stale}))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("chunk_ref does not match the current project generation"),
        "{error}"
    );
    local.execute("DROP TABLE chunks", []).unwrap();
    assert_catalog_views(&mut other_client, first_id, "error", other_id);
}

#[test]
#[cfg(unix)]
fn shared_v5_indexes_new_projects_and_syncs_an_existing_project() {
    let cache = tempfile::tempdir_in("/tmp").unwrap();
    let config = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("config.toml"), "config_version = 2\n").unwrap();
    seed_empty_v5(cache.path());
    let (_daemon, transport) = start_shared_daemon(cache.path(), config.path());
    let project_id =
        index_project_through_cli(cache.path(), config.path(), root.path(), "connectedvireo");
    let mut client = Client::connect(&transport, cache.path()).unwrap();
    client.handshake_registry().unwrap();
    let bound = client
        .call(Request::RegisterSession {
            pid: std::process::id(),
            cwd: root.path().to_owned(),
        })
        .unwrap();
    assert!(
        matches!(bound, docsbase_memory::ipc::protocol::Response::ToolResult { value }
        if value["project_id"] == project_id && value["status"] == "indexed"),
        "register did not select the published project"
    );
    let hit = client
        .call_tool("search_docs", serde_json::json!({"query":"connectedvireo"}))
        .unwrap();
    assert_eq!(hit[0]["path"], "guide.md");
    let sync = cli(cache.path(), config.path())
        .current_dir(root.path())
        .arg("sync")
        .output()
        .unwrap();
    assert!(
        sync.status.success(),
        "{}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync: serde_json::Value = serde_json::from_slice(&sync.stdout).unwrap();
    assert_eq!(sync["project_id"], project_id);
    assert_eq!(sync["state"], "done");
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
    let transport = test_transport::TestTransport::new(4);
    await_started(&transport, &mut daemon, cache.path());
    let mut client = Client::connect(&transport, cache.path()).unwrap();
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
    let transport = test_transport::TestTransport::new(4);
    await_started(&transport, &mut daemon, cache.path());
    let mut client = Client::connect(&transport, cache.path()).unwrap();
    client.handshake_registry().unwrap();
    let status = client.call_tool("status", serde_json::json!({})).unwrap();
    assert_eq!(status["watchers"], 0);
    assert_eq!(status["projects"].as_array().unwrap().len(), 1);
    client.call(Request::StopDaemon).unwrap();
    drop(client);
    assert!(daemon.0.wait().unwrap().success());
}

struct ProfileEnv {
    transport: test_transport::TestTransport,
    cache: tempfile::TempDir,
    root: tempfile::TempDir,
    config: tempfile::TempDir,
}
impl ProfileEnv {
    fn indexed() -> Self {
        let env = Self {
            transport: test_transport::TestTransport::new(16),
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
        await_started(&self.transport, &mut daemon, self.cache.path());
        daemon
    }
    fn connect(&self) -> Client {
        let mut client = Client::connect(&self.transport, self.cache.path()).unwrap();
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
        assert!(Client::connect(&env.transport, env.cache.path()).is_none());
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
    assert!(Client::connect(&env.transport, env.cache.path()).is_none());
}

#[test]
fn website_schema_upgrade_through_stopped_direct_index_keeps_search_ready() {
    let env = ProfileEnv::indexed();
    let db =
        rusqlite::Connection::open(env.cache.path().join(docsbase_memory::store::DB_FILE)).unwrap();
    db.execute_batch(
        "ALTER TABLE docs DROP COLUMN chunk_recipe;
         UPDATE projects SET schema_version=3;
         PRAGMA user_version=3;",
    )
    .unwrap();
    drop(db);

    let indexed = cli(env.cache.path(), env.config.path())
        .arg("index")
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );

    let mut daemon = env.start(&[]);
    let status = cli(env.cache.path(), env.config.path())
        .args(["status", "--project"])
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["ready"], true);
    let mut client = env.connect();
    client.call(Request::StopDaemon).unwrap();
    drop(client);
    assert!(daemon.0.wait().unwrap().success());
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

#[derive(Debug)]
struct StartupFailure {
    failed: bool,
    published: bool,
    timed_out: bool,
    diagnostic: String,
}

// A broken Website startup can stay alive forever. Stop observation as soon as
// it publishes an endpoint; otherwise use a startup watchdog and always reap.
fn observe_startup_failure(env: &ProfileEnv, args: &[&str]) -> StartupFailure {
    let mut daemon = Daemon(
        cli(env.cache.path(), env.config.path())
            .arg("serve")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut status = None;
    let mut published = false;
    while Instant::now() < deadline {
        if let Some(exited) = daemon.0.try_wait().unwrap() {
            status = Some(exited);
            break;
        }
        if matches!(
            Client::try_connect(&env.transport, env.cache.path()),
            Ok(Some(_))
        ) {
            published = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let timed_out = status.is_none() && !published;
    if status.is_none() {
        let _ = daemon.0.kill();
        daemon.0.wait().unwrap();
    }
    let mut diagnostic = String::new();
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    StartupFailure {
        failed: status.is_some_and(|status| !status.success()),
        published,
        timed_out,
        diagnostic,
    }
}

#[test]
fn cli_website_rejects_captured_legacy_source_errors_before_startup_resources() {
    let mut violations = Vec::new();
    for scenario in [
        "malformed",
        "oversized",
        "unavailable",
        "invalid_shared_legacy",
    ] {
        let env = ProfileEnv::indexed();
        let path = env.config.path().join("config.toml");
        match scenario {
            "malformed" => std::fs::write(&path, "[daemon\n").unwrap(),
            "oversized" => std::fs::write(&path, vec![b' '; 1_048_577]).unwrap(),
            "unavailable" => {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
            _ => std::fs::write(
                &path,
                "max_file_size=0\n[daemon]\nprofile={mode='shared'}\n",
            )
            .unwrap(),
        }
        let registry = env.cache.path().join("registry.db");
        let before = std::fs::read(&registry).unwrap();
        let outcome =
            observe_startup_failure(&env, &["--project", env.root.path().to_str().unwrap()]);
        let effects: Vec<_> = ["state", "logs", "vectors.db", "data"]
            .into_iter()
            .filter(|name| env.cache.path().join(name).exists())
            .collect();
        if !outcome.failed
            || outcome.published
            || outcome.timed_out
            || !effects.is_empty()
            || std::fs::read(registry).unwrap() != before
        {
            violations.push(format!("{scenario}: {outcome:?}, effects={effects:?}"));
        }
    }
    assert_eq!(violations, Vec::<String>::new());
}

#[test]
fn explicit_shared_retains_legacy_source_fallback_and_readonly_unbound_binding() {
    for source in [
        "[daemon\n",
        "max_file_size=0\n[daemon]\nprofile={mode='shared'}\n",
    ] {
        let env = ProfileEnv::indexed();
        std::fs::write(env.config.path().join("config.toml"), source).unwrap();
        let mut daemon = env.start(&["--profile", "shared", "--grace-ms", "60000"]);
        let mut client = env.connect();
        assert!(matches!(
            client
                .call(Request::BindReadOnly {
                    pid: std::process::id(),
                    project_id: None
                })
                .unwrap(),
            docsbase_memory::ipc::protocol::Response::ToolResult { .. }
        ));
        drop(client);
        env.stop(&mut daemon);
    }
}

#[test]
fn cli_shared_cannot_mask_recognized_website_source_policy_errors() {
    for (prefix, suffix) in [
        ("max_file_size=0\n", ""),
        ("", "idle_grace_ms=0\n"),
        ("", "max_events=0\n"),
    ] {
        let env = ProfileEnv::indexed();
        std::fs::write(
            env.config.path().join("config.toml"),
            format!(
                "{prefix}[daemon]\nprofile={{mode='website',project_root={}}}\n{suffix}",
                toml::Value::String(env.root.path().to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        let outcome = observe_startup_failure(&env, &["--profile", "shared"]);
        assert!(
            outcome.failed && !outcome.published && !outcome.timed_out,
            "{outcome:?}"
        );
        assert_ne!(outcome.diagnostic, "");
        assert!(!env.cache.path().join("state").exists());
    }
}

#[test]
fn explicit_project_status_uses_fixed_readonly_binding_from_unrelated_cwd() {
    let env = ProfileEnv::indexed();
    let mut daemon = env.start(&[]);
    let output = cli(env.cache.path(), env.config.path())
        .current_dir(env.config.path())
        .args(["status", "--project"])
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["profile"], "website");
    assert_eq!(view["project"]["id"], env.project().id);
    assert_eq!(view["ready"], true);
    let mut client = env.connect();
    assert!(
        client
            .call_tool("server_status", serde_json::json!({}))
            .is_err(),
        "unbound readiness must be refused"
    );
    client
        .call(Request::BindReadOnly {
            pid: std::process::id(),
            project_id: Some(env.project().id),
        })
        .unwrap();
    for args in [
        serde_json::json!({"project_id": env.project().id + 1}),
        serde_json::json!({"repair":true}),
    ] {
        assert!(client.call_tool("server_status", args).is_err());
    }
    assert_eq!(
        client.call_tool("status", serde_json::json!({})).unwrap()["watchers"],
        0
    );
    drop(client);
    env.stop(&mut daemon);
    let output = cli(env.cache.path(), env.config.path())
        .args(["status", "--project"])
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "explicit project status needs the daemon"
    );
    assert!(Client::connect(&env.transport, env.cache.path()).is_none());
}

#[test]
fn explicit_status_is_unavailable_in_shared_and_does_not_register_unknown_roots() {
    let env = ProfileEnv::indexed();
    let mut shared = env.start(&["--profile", "shared", "--grace-ms", "60000"]);
    let output = cli(env.cache.path(), env.config.path())
        .args(["status", "--project"])
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "fixed Website readiness requires Website profile"
    );
    env.stop(&mut shared);
    let unknown = tempfile::tempdir().unwrap();
    let mut website = env.start(&[]);
    let output = cli(env.cache.path(), env.config.path())
        .args(["status", "--project"])
        .arg(unknown.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let db = docsbase_memory::store::Db::open_readonly(env.cache.path()).unwrap();
    assert_eq!(
        docsbase_memory::daemon::registry::list_projects(&db)
            .unwrap()
            .len(),
        1
    );
    env.stop(&mut website);
}

#[test]
fn website_explicit_sync_adds_and_removes_files_and_publishes_readiness() {
    let env = ProfileEnv::indexed();
    let mut daemon = env.start(&[]);
    std::fs::remove_file(env.root.path().join("guide.md")).unwrap();
    std::fs::write(env.root.path().join("added.md"), "addedreadinesswidget").unwrap();
    let sync = cli(env.cache.path(), env.config.path())
        .current_dir(env.config.path())
        .arg("sync")
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(sync.status.success(), "{sync:?}");
    let output = cli(env.cache.path(), env.config.path())
        .current_dir(env.config.path())
        .args(["status", "--project"])
        .arg(env.root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["ready"], true);
    assert_eq!(view["latest_sync"]["stats"]["docs"], 1);
    assert_eq!(view["latest_sync"]["stats"]["removed"], 1);
    assert_eq!(
        view["latest_sync"]["attempt_id"],
        view["outcome"]["attempt_id"]
    );
    let status = env
        .connect()
        .call_tool("status", serde_json::json!({}))
        .unwrap();
    assert_eq!(status["watchers"], 0);
    let dir = env
        .cache
        .path()
        .join("projects")
        .join(env.project().id.to_string())
        .join("tantivy");
    let index = docsbase_memory::index::tantivy_index::ReadIndex::open(&dir).unwrap();
    assert_eq!(index.search("addedreadinesswidget", 10).unwrap().len(), 1);
    assert_eq!(index.search("originalwidget", 10).unwrap().len(), 0);
    drop(index);
    env.stop(&mut daemon);
}

#[cfg(unix)]
fn assert_mcp_catalog_binding(cache: &Path, config: &Path, root: &Path, expected: Option<&str>) {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};

    let mut frontend = Daemon(
        cli(cache, config)
            .arg("mcp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut input = frontend.0.stdin.take().unwrap();
    let output = BufReader::new(frontend.0.stdout.take().unwrap());
    let (send, replies) = std::sync::mpsc::sync_channel(1);
    let pump = std::thread::spawn(move || {
        for line in output.lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let receive = || -> Value {
        let line = replies
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    };
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}
    }})).unwrap();
    assert_eq!(
        receive()["result"]["serverInfo"]["name"],
        "docsbase-memory-mcp"
    );
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"search_docs","arguments":{"query":"websitefirsthelix","chunk_ref":true}
        }})
    )
    .unwrap();
    let hits = receive();
    if let Some(expected) = expected {
        assert_eq!(
            hits["result"]["structuredContent"][0]["chunk_ref"], expected,
            "{hits}"
        );
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                "name":"read_neighbors_at","arguments":{"chunk_ref":expected}
            }})
        )
        .unwrap();
        let neighbors = receive();
        assert!(
            neighbors["result"]["structuredContent"][0]["text"]
                .as_str()
                .unwrap()
                .contains("websitefirsthelix")
        );
    } else {
        assert_eq!(
            hits["result"]["structuredContent"]["code"], -32010,
            "{hits}"
        );
    }
    drop(input);
    drop(replies);
    drop(frontend);
    pump.join().unwrap();
}

#[test]
#[cfg(unix)]
#[allow(clippy::too_many_lines)] // Exercise the complete pair lifecycle through the daemon.
fn website_serves_and_replaces_an_existing_v5_generation() {
    use docsbase_memory::ipc::protocol::{self, Response};
    use docsbase_memory::store::{DB_FILE, Db};
    use serde_json::{Value, json};

    let cache = tempfile::tempdir().unwrap();
    // macOS paths such as /var/folders have a symlinked ancestor.
    // Keep the Unix socket path independent of TMPDIR's length.
    let alias_dir = tempfile::tempdir_in("/tmp").unwrap();
    let parent = alias_dir.path().join("p");
    std::os::unix::fs::symlink(cache.path().parent().unwrap(), &parent).unwrap();
    let cache_alias = parent.join(cache.path().file_name().unwrap());
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let doc = root.path().join("guide.md");
    std::fs::write(&doc, "# Profile\n\nwebsitefirsthelix\n").unwrap();
    std::fs::write(config.path().join("config.toml"), "config_version = 2\n").unwrap();

    let mut registry = Db::open(cache.path()).unwrap();
    let tx = registry.begin_write().unwrap();
    tx.execute_batch(include_str!("../src/store/generation_catalog/schema.sql"))
        .unwrap();
    tx.execute(
        "INSERT INTO generation_store VALUES (1, 1, ?1)",
        [[25_u8; 16].as_slice()],
    )
    .unwrap();
    tx.pragma_update(None, "user_version", 5).unwrap();
    tx.commit().unwrap();
    drop(registry);

    let run = |args: &[&str]| -> Value {
        let output = cli(&cache_alias, config.path())
            .current_dir(root.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let indexed = run(&["index"]);
    let active = || {
        let registry = rusqlite::Connection::open(cache.path().join(DB_FILE)).unwrap();
        registry
            .query_row("SELECT active_id FROM project_generation_head", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    let first = active();
    let mut daemon = Daemon(
        cli(&cache_alias, config.path())
            .args(["serve", "--project"])
            .arg(root.path())
            .current_dir(config.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let transport = test_transport::TestTransport::new(4);
    await_started(&transport, &mut daemon, &cache_alias);
    let mut probe = Client::connect(&transport, &cache_alias).unwrap();
    assert!(matches!(
        probe
            .call(Request::Hello {
                protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(),
                client: "website-catalog-test".into(),
            })
            .unwrap(),
        Response::Hello {
            schema_version: 5,
            ..
        }
    ));
    drop(probe);
    let state: Value =
        serde_json::from_slice(&std::fs::read(cache.path().join("state/daemon.json")).unwrap())
            .unwrap();
    assert_eq!(state["schema_version"], 5);

    let project = root.path().to_str().unwrap();
    let ready = || run(&["status", "--project", project]);
    let status = ready();
    assert_eq!(status["profile"], "website");
    assert_eq!(status["project"]["id"], indexed["project_id"]);
    assert_eq!(status["ready"], true);
    assert_eq!(run(&["search", "websitefirsthelix"])[0]["path"], "guide.md");

    let mut client = Client::connect(&transport, &cache_alias).unwrap();
    client.handshake_registry().unwrap();
    client
        .call(Request::BindReadOnly {
            pid: std::process::id(),
            project_id: indexed["project_id"].as_i64(),
        })
        .unwrap();
    let documents = client.call_tool("list_docs", json!({})).unwrap();
    assert_eq!(documents["docs"][0]["path"], "guide.md");
    let hits = client
        .call_tool("search_docs", json!({"query": "websitefirsthelix"}))
        .unwrap();
    assert!(hits[0].get("chunk_ref").is_none());
    let neighbors = client
        .call_tool("read_neighbors", json!({"chunk_id": hits[0]["chunk_id"]}))
        .unwrap();
    assert!(
        neighbors[0]["text"]
            .as_str()
            .unwrap()
            .contains("websitefirsthelix")
    );
    let referenced = client
        .call_tool(
            "search_docs",
            json!({"query": "websitefirsthelix", "chunk_ref": true}),
        )
        .unwrap();
    let first_ref = referenced[0]["chunk_ref"].as_str().unwrap();
    let at = client
        .call_tool("read_neighbors_at", json!({"chunk_ref": first_ref}))
        .unwrap();
    assert_eq!(at, neighbors);
    assert_mcp_catalog_binding(&cache_alias, config.path(), root.path(), Some(first_ref));
    assert_mcp_catalog_binding(&cache_alias, config.path(), config.path(), None);

    std::fs::write(doc, "# Profile\n\nwebsitesecondspiral\n").unwrap();
    std::fs::write(root.path().join("new.md"), "websiteadditionnote\n").unwrap();
    let registry = rusqlite::Connection::open(cache.path().join(DB_FILE)).unwrap();
    registry
        .execute_batch(
            "CREATE TRIGGER reject_sync_completion BEFORE UPDATE OF state ON sync_jobs
             WHEN NEW.state = 'done' BEGIN SELECT RAISE(ABORT, 'test interrupted publication'); END;",
        )
        .unwrap();
    let failed = cli(&cache_alias, config.path())
        .current_dir(root.path())
        .arg("sync")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let failure: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failure["state"], "error");
    assert!(
        failure["stats"]["error"]
            .as_str()
            .unwrap()
            .contains("test interrupted publication")
    );
    assert_eq!(active(), first);
    assert_eq!(ready()["reason"], "sync_failed");
    assert_eq!(run(&["search", "websitefirsthelix"])[0]["path"], "guide.md");
    assert_eq!(run(&["search", "websitesecondspiral"]), json!([]));
    registry
        .execute_batch("DROP TRIGGER reject_sync_completion")
        .unwrap();
    drop(registry);

    let sync = run(&["sync"]);
    assert_eq!(sync["state"], "done");
    let next = active();
    assert!(next > first);
    assert_eq!(sync["stats"]["index_outcome"]["generation_id"], next);
    let current = ready();
    assert_eq!(current["ready"], true);
    assert_eq!(current["index_generation"], status["index_generation"]);
    let second = client
        .call_tool(
            "search_docs",
            json!({"query": "websitesecondspiral", "chunk_ref": true}),
        )
        .unwrap();
    assert_eq!(second[0]["chunk_id"], referenced[0]["chunk_id"]);
    assert_ne!(second[0]["chunk_ref"], first_ref);
    assert!(matches!(
        client.call_tool("read_neighbors_at", json!({"chunk_ref": first_ref})),
        Err(docsbase_memory::error::Error::Project {
            instruction: Some(_),
            ..
        })
    ));
    let latest = client
        .call_tool(
            "read_neighbors_at",
            json!({"chunk_ref": second[0]["chunk_ref"]}),
        )
        .unwrap();
    assert!(
        latest[0]["text"]
            .as_str()
            .unwrap()
            .contains("websitesecondspiral")
    );
    assert_eq!(
        run(&["search", "websitesecondspiral"])[0]["path"],
        "guide.md"
    );
    assert_eq!(run(&["search", "websitefirsthelix"]), serde_json::json!([]));

    let documents = client.call_tool("list_docs", json!({})).unwrap();
    assert_eq!(documents["docs"][1]["path"], "new.md");
    for tool in ["list_projects", "panel_projects"] {
        let projects = client.call_tool(tool, json!({})).unwrap();
        assert_eq!(projects[0]["status"], "indexed", "{tool}");
        assert_eq!(projects[0]["docs"], 2, "{tool}");
    }
    let status = client.call_tool("status", json!({})).unwrap();
    assert_eq!(status["schema_version"], 5);
    assert_eq!(status["projects"][0]["status"], "indexed");
    assert_eq!(status["projects"][0]["docs"], 2);

    let orphan = rusqlite::Connection::open(cache.path().join(DB_FILE))
        .unwrap()
        .query_row(
            "INSERT INTO sync_jobs (project_id, state, started_at) VALUES (?1, 'running', 1) RETURNING id",
            [indexed["project_id"].as_i64().unwrap()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let started = client.call_tool("sync_start", json!({})).unwrap();
    let job = started["job_id"].as_i64().unwrap();
    assert_ne!(
        job, orphan,
        "a row without its native owner cannot be joined"
    );
    drop(client);
    let mut client = Client::connect(&transport, &cache_alias).unwrap();
    client.handshake_registry().unwrap();
    let retired = client
        .call_tool("sync_status", json!({"job_id": orphan}))
        .unwrap();
    assert_eq!(retired["state"], "error");
    assert_eq!(retired["stats"]["orphaned"], true);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = client
            .call_tool("sync_status", json!({"job_id": job}))
            .unwrap();
        if result["state"] == "done" {
            break;
        }
        assert_eq!(result["state"], "running", "{result}");
        assert!(
            Instant::now() < deadline,
            "detached sync did not finish: {result}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(active() > next);

    client.call(Request::StopDaemon).unwrap();
    drop(client);
    assert!(daemon.0.wait().unwrap().success());
}
