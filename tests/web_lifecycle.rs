#![cfg(feature = "web")]
#[cfg(unix)]
#[path = "support/transport.rs"]
mod test_transport;

use std::{process::Command, time::Duration};

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_docsbase"));
    command
        .current_dir(root)
        .env("DOCSBASE_CACHE_DIR", root.join("cache"))
        .env("DOCSBASE_CONFIG_DIR", root.join("config"))
        .env("DOCSBASE_DATA_DIR", root.join("data"));
    command
}
#[test]
fn occupied_port_is_rejected_before_daemon_start() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = command(root.path())
        .args([
            "web",
            "--port",
            &listener.local_addr().unwrap().port().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot bind local Web port"));
    assert!(!root.path().join("cache/state/daemon.json").exists());
}

#[test]
fn oversized_web_config_is_rejected_before_binding_or_daemon_start() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    // Occupy the configured address so the pre-fix binary also exits cleanly.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cap = usize::try_from(docsbase_memory::limits::MAX_STATE_BYTES).unwrap();
    std::fs::write(
        config.join("web.toml"),
        format!("port = {port}\n#{}\n", "x".repeat(cap)),
    )
    .unwrap();

    let output = command(root.path()).arg("web").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("web.toml is over the"), "{stderr}");
    assert!(!root.path().join("cache/state/daemon.json").exists());
}

#[test]
fn startup_lock_timeout_exits_without_waiting_for_blocking_thread() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("cache/state")).unwrap();
    std::fs::create_dir(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/web.toml"),
        "startup_timeout_ms = 50\nshutdown_timeout_ms = 50\n",
    )
    .unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.path().join("cache/state/daemon.start.lock"))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let _held = lock.write().unwrap();
    let mut child = assert_cmd::Command::from_std(command(root.path()));
    child.arg("web").timeout(Duration::from_secs(10));
    let output = child.assert().failure().get_output().clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("daemon startup deadline"));
}
#[test]
fn web_attaches_to_running_daemon_after_global_config_becomes_invalid() {
    use docsbase_memory::daemon::lifecycle::daemon_pid;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Stdio},
        time::Instant,
    };

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join("web.toml"), "port = 0\n").unwrap();

    let mut daemon = ChildGuard(
        command(root.path())
            .args(["serve", "--grace-ms", "60000"])
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while daemon_pid(&cache) != Some(daemon.0.id()) {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited during startup"
        );
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(config.join("config.toml"), "[daemon]\ndrain_poll_ms = 0\n").unwrap();

    let mut web = ChildGuard(
        command(root.path())
            .arg("web")
            .env(
                "DOCSBASE_DAEMON_EXE",
                root.path().join("unexpected-autostart"),
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = web.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    assert!(
        line.starts_with("docsbase Web: http://127.0.0.1:"),
        "Web did not attach to the running daemon: {line:?}"
    );
}
#[cfg(unix)]
#[test]
fn ctrl_c_leaves_shared_daemon_alive_and_open_failure_is_nonfatal() {
    use docsbase_memory::ipc::{client::Client, protocol::Request};
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
    };
    let root = tempfile::tempdir().unwrap();
    let mut child = command(root.path())
        .args(["web", "--open"])
        .env("PATH", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(line.starts_with("docsbase Web: http://127.0.0.1:"));
    let transport = test_transport::TestTransport::new(4);
    let mut client = Client::connect(&transport, &root.path().join("cache")).unwrap();
    client.handshake_registry().unwrap();
    client
        .call(Request::BindReadOnly {
            pid: std::process::id(),
            project_id: None,
        })
        .unwrap();
    // kill is an OS signal utility; no shell interpretation or user input.
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait().unwrap()).unwrap());
    let exit = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(exit.success(), "Web frontend terminated: {exit:?}");
    assert!(
        client
            .call_tool("panel_projects", serde_json::json!({}))
            .is_ok()
    );
    client.call(Request::StopDaemon).unwrap();
}
