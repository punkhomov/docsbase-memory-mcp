//! CLI coverage for `docsbase connect` under `--features hybrid-local` (and
//! `hybrid-remote` for the openrouter arm). Network paths are exercised only
//! via fake sources in the unit tests; here we assert wiring and status
//! output.

#![cfg(feature = "hybrid-local")]

use assert_cmd::Command;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    config: TempDir,
    data: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            cache: TempDir::new().expect("cache"),
            config: TempDir::new().expect("config"),
            data: TempDir::new().expect("data"),
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("docsbase");
        cmd.env("DOCSBASE_CACHE_DIR", self.cache.path());
        cmd.env("DOCSBASE_CONFIG_DIR", self.config.path());
        cmd.env("DOCSBASE_DATA_DIR", self.data.path());
        cmd
    }
}

#[test]
fn connect_without_provider_prints_status_table() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["connect"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("provider"), "header: {stdout}");
    assert!(stdout.contains("fastembed"), "fastembed row: {stdout}");
    assert!(stdout.contains("openrouter"), "openrouter row: {stdout}");
    assert!(
        stdout.contains("Unknown"),
        "fresh providers are Unknown: {stdout}"
    );
}

#[test]
fn connect_fastembed_rejects_local_flag_shape() {
    // `--local` is rejected (FR-4): clap has no such flag, so an unknown
    // argument fails.
    let env = Env::new();
    env.cmd()
        .args(["connect", "fastembed", "--local"])
        .assert()
        .failure();
}

#[test]
fn connect_fastembed_check_reports_missing_without_download() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["connect", "fastembed", "--check"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not installed"), "{stdout}");
}

#[test]
fn connect_fastembed_check_and_force_are_mutually_exclusive() {
    // Round-2 U4: `--check --force` must error, not silently install.
    let env = Env::new();
    let output = env
        .cmd()
        .args(["connect", "fastembed", "--check", "--force"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mutually exclusive"), "{stderr}");
    // Nothing was installed/downloaded and no runtime dir appears.
    assert!(
        !env.data.path().join("runtimes").exists(),
        "no install happened"
    );
}

#[test]
fn connect_fastembed_check_with_vanished_runtime_path_is_broken_not_internal() {
    // C8: a dangling recorded runtime_path must be reported as Broken state
    // (exit 0), never an internal stat error.
    let env = Env::new();
    std::fs::write(
        env.config.path().join("config.toml"),
        "[providers.fastembed]\nenabled = true\nruntime = \"1.24.4\"\nruntime_path = \"/nonexistent/libonnxruntime.so\"\n",
    )
    .expect("global config");
    let output = env
        .cmd()
        .args(["connect", "fastembed", "--check"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("is missing"), "{stdout}");
    assert!(!stderr.contains("internal error"), "{stderr}");
}

#[test]
fn unsupported_provider_name_is_clap_error() {
    let env = Env::new();
    env.cmd()
        .args(["connect", "no-such-provider"])
        .assert()
        .failure();
}
