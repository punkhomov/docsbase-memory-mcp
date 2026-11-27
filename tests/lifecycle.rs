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

#[path = "support/transport.rs"]
mod test_transport;

struct Env {
    transport: test_transport::TestTransport,
    cache: TempDir,
}

#[test]
fn owner_connection_limit_is_enforced_by_the_accept_loop() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let env = Env::new();
    let config = TempDir::new().unwrap();
    fs::write(
        config.path().join("config.toml"),
        "[daemon]\nmax_connections=1\n",
    )
    .unwrap();
    let _child = ChildGuard(
        env.cli()
            .env("DOCSBASE_CONFIG_DIR", config.path())
            .args(["serve", "--grace-ms", "60000"])
            .spawn()
            .unwrap(),
    );
    assert!(wait_until(Duration::from_secs(10), || env.endpoint_up()));
    let mut held = Client::connect(&env.transport, env.cache()).unwrap();
    held.handshake_registry().unwrap();
    let mut extra = Client::connect(&env.transport, env.cache()).unwrap();
    assert!(
        extra.handshake_registry().is_err(),
        "the second connection must be shed by owner max_connections=1"
    );
    held.handshake_registry().unwrap();
}

impl Env {
    fn new() -> Self {
        Self {
            transport: test_transport::TestTransport::new(160),
            cache: TempDir::new().expect("cache"),
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn connect_until(
        &self,
        deadline: Instant,
    ) -> std::io::Result<platform::transport::BlockingStream> {
        let endpoint = platform::daemon_endpoint(self.cache());
        #[cfg(windows)]
        {
            self.transport.connect_blocking_until(&endpoint, deadline)
        }
        #[cfg(unix)]
        {
            if Instant::now() >= deadline {
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
            }
            platform::connect_blocking(&self.transport, &endpoint)
        }
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
fn launcher_start_timeout_comes_from_global_config() {
    const CHILD: &str = "DOCSBASE_TEST_LAUNCH_TIMEOUT_CHILD";
    let test_binary = std::env::current_exe().unwrap();
    if let Some(cache) = std::env::var_os(CHILD) {
        // The test harness passed `serve --detached` exits without opening a
        // listener, so this exercises the real launcher wait without a daemon.
        let error = ensure_daemon_with(Path::new(&cache), Some(&test_binary)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("daemon did not start within 35ms"),
            "{error}"
        );
        return;
    }

    let cache = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();
    fs::write(
        config.path().join("config.toml"),
        "config_version = 2\n[daemon]\nstart_timeout_ms = 35\n",
    )
    .unwrap();
    assert_cmd::Command::new(test_binary)
        .args(["--exact", "launcher_start_timeout_comes_from_global_config"])
        .env(CHILD, cache.path())
        .env("DOCSBASE_CONFIG_DIR", config.path())
        .timeout(Duration::from_secs(5))
        .assert()
        .success();
}

#[test]
fn launcher_stop_timeout_bounds_an_unanswered_native_exchange() {
    let env = Env::new();
    let config = TempDir::new().unwrap();
    let path = config.path().join("config.toml");
    fs::write(
        &path,
        "config_version = 2\n[daemon]\nstop_timeout_ms = 175\n",
    )
    .unwrap();
    let endpoint = write_live_state(&env);
    let _listener = platform::bind_blocking(&env.transport, &endpoint).unwrap();

    let view = env
        .cli()
        .current_dir(env.cache())
        .env("DOCSBASE_CONFIG_DIR", config.path())
        .args(["config", "effective"])
        .output()
        .unwrap();
    assert!(view.status.success(), "{view:?}");
    let view: serde_json::Value = serde_json::from_slice(&view.stdout).unwrap();
    for (key, value, source) in [
        (
            "daemon.start_timeout_ms",
            serde_json::json!(10000),
            "defaults",
        ),
        ("daemon.stop_timeout_ms", serde_json::json!(175), "global"),
        (
            "daemon.no_listener_timeout_ms",
            serde_json::json!(cfg!(unix).then_some(2000)),
            "defaults",
        ),
    ] {
        let row = view["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == key)
            .unwrap();
        assert_eq!(row["value"], value);
        assert_eq!(row["source"]["kind"], source);
        assert_eq!(row["unit"], "milliseconds");
        assert_eq!(row["scope"], "daemon");
    }

    let mut stop = assert_cmd::Command::from_std(env.cli());
    let result = stop
        .env("DOCSBASE_CONFIG_DIR", config.path())
        .args(["daemon", "stop"])
        .timeout(Duration::from_secs(5))
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&result.get_output().stderr);
    assert!(stderr.contains("did not stop within 175ms"), "{stderr}");
    assert!(env.cache().join("state/daemon.json").exists());
}

#[cfg(unix)]
#[test]
fn launcher_no_listener_timeout_can_finish_before_stop_deadline() {
    let env = Env::new();
    let config = TempDir::new().unwrap();
    let path = config.path().join("config.toml");
    write_live_state(&env);
    for (no_listener, success) in [(25, true), (750, false)] {
        fs::write(
            &path,
            format!("config_version = 2\n[daemon]\nstop_timeout_ms = 500\nno_listener_timeout_ms = {no_listener}\n"),
        )
        .unwrap();
        let mut stop = assert_cmd::Command::from_std(env.cli());
        let result = stop
            .env("DOCSBASE_CONFIG_DIR", config.path())
            .args(["daemon", "stop"])
            .timeout(Duration::from_secs(5))
            .assert();
        if success {
            result.success();
        } else {
            let result = result.failure();
            let stderr = String::from_utf8_lossy(&result.get_output().stderr);
            assert!(stderr.contains("did not stop within 500ms"), "{stderr}");
        }
        assert!(env.cache().join("state/daemon.json").exists());
    }
}

fn write_live_state(env: &Env) -> platform::Endpoint {
    let endpoint = platform::daemon_endpoint(env.cache());
    let state_dir = env.cache().join("state");
    fs::create_dir(&state_dir).unwrap();
    fs::write(
        state_dir.join("daemon.json"),
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(), "socket": endpoint,
            "build_id": docsbase_memory::ipc::protocol::build_id(),
            "schema_version": 5, "cache_root": env.cache(),
        }))
        .unwrap(),
    )
    .unwrap();
    endpoint
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
    let listener = platform::bind_blocking(&env.transport, &platform::daemon_endpoint(env.cache()))
        .expect("stale socket");
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

    let session =
        platform::connect_blocking(&env.transport, &platform::daemon_endpoint(env.cache()))
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
    // Long grace: the flood sequence (64 connects plus saturation probing)
    // outlives the 5 s default on slow runners (macOS CI), and the daemon
    // must stay up for the whole test.
    let mut child = env
        .cli()
        .args(["serve", "--detached", "--grace-ms", "60000"])
        .spawn()
        .expect("spawn serve");
    assert!(
        wait_until(Duration::from_secs(10), || env.endpoint_up()),
        "socket must appear"
    );

    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        held.push(
            env.connect_until(Instant::now() + Duration::from_secs(10))
                .expect("connect within cap"),
        );
    }
    // The accept loop consumes the held connections asynchronously: probe
    // until the daemon is actually at capacity instead of trusting a fixed
    // sleep (CI timing flake). Each accepted probe is dropped before the next
    // iteration, so a probe cannot hold a slot itself.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let extra = env.connect_until(deadline).expect("connect handshake");
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
        let shed = match BufReader::new(extra).read_line(&mut line) {
            Ok(0) | Err(_) => true,
            Ok(_) => false,
        };
        if shed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon never reached capacity; probe reply: {line}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Freeing one slot restores service; the daemon may take a moment to
    // notice the EOF and release the permit on slow runners. Retry the full
    // admission (connect plus registry handshake), not just connect: on
    // Linux a shed connection still connects and only the handshake fails.
    drop(held.pop());
    let mut admitted = None;
    assert!(
        wait_until(Duration::from_secs(5), || {
            let Some(mut client) = Client::connect(&env.transport, env.cache()) else {
                return false;
            };
            if client.handshake_registry().is_err() {
                return false;
            }
            admitted = Some(client);
            true
        }),
        "client under cap"
    );
    let mut client = admitted.expect("client under cap");
    let status = client
        .call_tool("status", serde_json::json!({}))
        .expect("status");
    assert!(status["projects"].is_array());
    drop(held);
    stop_daemon(env.cache()).expect("stop daemon");
    let _ = child.wait();
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

    let mut client = Client::connect(&env.transport, env.cache()).expect("connect");
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

#[test]
fn configured_sync_history_retains_the_newest_receipts_on_both_registries() {
    for version in [1, 2] {
        let env = Env::new();
        let project = TempDir::new().unwrap();
        let config = TempDir::new().unwrap();
        let header = if version == 2 {
            "config_version = 2\n"
        } else {
            ""
        };
        fs::write(
            config.path().join("config.toml"),
            format!("{header}[daemon]\nmax_sync_jobs = 2\n"),
        )
        .unwrap();
        fs::write(project.path().join("guide.md"), "# Sync history\n").unwrap();

        let command = || {
            let mut command = env.cli();
            command
                .env("DOCSBASE_CONFIG_DIR", config.path())
                .env("DOCSBASE_DATA_DIR", config.path().join("data"));
            command
        };
        let indexed = command().arg("index").arg(project.path()).output().unwrap();
        assert!(indexed.status.success(), "version {version}: {indexed:?}");
        let mut daemon = command()
            .args(["serve", "--detached", "--grace-ms", "10000"])
            .spawn()
            .unwrap();
        assert!(wait_until(Duration::from_secs(10), || env.endpoint_up()));
        let mut client = Client::connect(&env.transport, env.cache()).unwrap();
        client.handshake(project.path()).unwrap();
        let db = rusqlite::Connection::open_with_flags(
            env.cache().join(DB_FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let has_catalog: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'project_generation_head')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_catalog, version == 2);
        let mut receipts = db.prepare("SELECT id FROM sync_jobs ORDER BY id").unwrap();
        let mut jobs = Vec::new();
        for _ in 0..4 {
            let mut id = None;
            assert!(wait_until(Duration::from_secs(5), || {
                match client.call_tool("sync_start", serde_json::json!({})) {
                    Ok(reply) => {
                        let candidate = reply["job_id"].as_i64().unwrap();
                        if jobs.last() == Some(&candidate) {
                            // V5 can return the terminal receipt while its native
                            // owner still holds the admission permit.
                            assert_eq!(reply["state"], "done");
                        } else {
                            assert!(jobs.last().is_none_or(|previous| candidate > *previous));
                            id = Some(candidate);
                        }
                        id.is_some()
                    }
                    Err(error) => {
                        assert!(
                            error
                                .to_string()
                                .contains("project sync is starting or retiring"),
                            "{error}"
                        );
                        false
                    }
                }
            }));
            let id = id.unwrap();
            jobs.push(id);
            assert!(wait_until(Duration::from_secs(5), || {
                let reply = client
                    .call_tool("sync_status", serde_json::json!({"job_id": id}))
                    .unwrap();
                assert_ne!(reply["state"], "error");
                reply["state"] == "done"
            }));
            let kept = receipts
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                kept,
                jobs[jobs.len().saturating_sub(2)..],
                "version {version} must retain the two newest receipts"
            );
        }

        drop(client);
        env.stop();
        assert!(daemon.wait().unwrap().success());
    }
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
        platform::connect_blocking(&env.transport, &platform::daemon_endpoint(env.cache()))
            .expect("connect");
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

    let mut stopper = Client::connect(&env.transport, env.cache()).expect("stopper");
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
fn configured_daemon_log_rotates_only_above_the_owned_threshold() {
    const CHILD: &str = "DOCSBASE_TEST_OWNED_LOG_THRESHOLD";
    if std::env::var_os(CHILD).is_none() {
        let config = TempDir::new().unwrap();
        fs::write(
            config.path().join("config.toml"),
            "config_version = 2\n[daemon]\nlog_rotate_bytes = 32\n",
        )
        .unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "configured_daemon_log_rotates_only_above_the_owned_threshold",
            ])
            .env(CHILD, "1")
            .env("DOCSBASE_CONFIG_DIR", config.path())
            .env("DOCSBASE_DATA_DIR", config.path().join("data"))
            .status()
            .unwrap();
        assert!(child.success());
        return;
    }

    let env = Env::new();
    let logs = env.cache().join("logs");
    fs::create_dir_all(&logs).unwrap();
    let log = logs.join("daemon.log");
    let old = logs.join("daemon.log.old");

    fs::File::create(&log).unwrap().set_len(32).unwrap();
    env.start();
    env.stop();
    assert!(!old.exists());

    fs::File::create(&log).unwrap().set_len(33).unwrap();
    env.start();
    env.stop();
    assert_eq!(fs::metadata(old).unwrap().len(), 33);
}

#[test]
fn oversized_daemon_state_is_treated_as_corrupt() {
    let env = Env::new();
    let state = env.cache().join("state");
    fs::create_dir_all(&state).expect("state dir");
    let path = state.join("daemon.json");
    let file = fs::File::create(&path).expect("create state");
    file.set_len(docsbase_memory::limits::MAX_STATE_BYTES + 1)
        .expect("grow state");
    drop(file);

    // The cap is enforced before reading: stop reports the corrupt file
    // instead of loading it into memory.
    let output = env
        .cli()
        .args(["daemon", "stop"])
        .output()
        .expect("run daemon stop");
    assert!(!output.status.success(), "oversized state must be refused");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("over the"), "unexpected stderr: {stderr}");
    assert!(path.is_file(), "stop must not delete the corrupt state");

    // Recovery: a fresh daemon replaces the corrupt state.
    env.start();
    let rewritten = fs::metadata(&path).expect("rewritten state");
    assert!(
        rewritten.len() <= docsbase_memory::limits::MAX_STATE_BYTES,
        "state must be rewritten small, got {}",
        rewritten.len()
    );
    env.stop();
}
