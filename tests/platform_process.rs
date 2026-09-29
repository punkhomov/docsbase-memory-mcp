//! Process/signals/permissions seam contract (ADR-9, T35).

use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::TempDir;

use docsbase_memory::platform::{self, fs as platform_fs, process};

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn alive_detects_self() {
    assert!(
        process::process_alive(std::process::id()),
        "own pid must be alive"
    );
}

#[test]
fn alive_rejects_dead_pid() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    child.kill().expect("kill");
    child.wait().expect("reap");
    assert!(!process::process_alive(pid), "reaped pid must be dead");
}

#[test]
fn private_log_is_0600() {
    let dir = TempDir::new().expect("tempdir");
    let logs = dir.path().join("logs");
    platform_fs::secure_dir(&logs).expect("secure dir");
    assert_eq!(mode(&logs), 0o700, "private dir");
    let file = platform_fs::open_private_log(&logs.join("daemon.log")).expect("log");
    drop(file);
    assert_eq!(mode(&logs.join("daemon.log")), 0o600, "private log");
    // Re-opening a pre-existing log keeps it private.
    fs::set_permissions(logs.join("daemon.log"), fs::Permissions::from_mode(0o644))
        .expect("loosen");
    let file = platform_fs::open_private_log(&logs.join("daemon.log")).expect("reopen");
    drop(file);
    assert_eq!(mode(&logs.join("daemon.log")), 0o600, "tightened again");
}

#[test]
fn secure_file_and_executable_modes() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("plain");
    fs::write(&file, b"x").expect("write");
    platform_fs::secure_file(&file).expect("secure file");
    assert_eq!(mode(&file), 0o600);

    let binary = dir.path().join("docsbase");
    fs::write(&binary, b"x").expect("write");
    platform_fs::secure_executable(&binary).expect("secure executable");
    assert_eq!(mode(&binary), 0o755);
}

#[test]
fn counts_are_positive() {
    assert!(process::fd_count() > 0, "fd count");
    assert!(process::thread_count() > 0, "thread count");
}

#[test]
fn detach_sets_own_process_group() {
    let mut command = std::process::Command::new("sleep");
    command.arg("5");
    process::detach(&mut command);
    let mut child = command.spawn().expect("spawn detached");
    let stat = fs::read_to_string(format!("/proc/{}/stat", child.id())).expect("stat");
    let rest = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    let mut fields = rest.split_whitespace();
    let _state = fields.next().expect("state");
    let _ppid = fields.next().expect("ppid");
    let pgrp: u32 = fields.next().expect("pgrp").parse().expect("pgrp number");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        pgrp,
        child.id(),
        "detach must place the child in its own group"
    );
}

#[test]
fn shutdown_signal_installs() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let _signal = platform::ShutdownSignal::new().expect("install handlers");
    });
}
