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
        // Host-independent (round-7 C2): the env override outranks every
        // other resolution source.
        cmd.env_remove("ORT_DYLIB_PATH");
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
fn connect_fastembed_noop_does_not_ask_for_a_restart() {
    // FR-13 / round-7 C5: the daemon-restart note must appear only when this
    // run actually (re)installed the runtime. Seeding the data-dir install
    // makes `runtime::install` take its fast path (no download), so the CLI
    // must stay quiet about restarting.
    use docsbase_memory::hybrid::runtime::{ORT_VERSION, platform_target};

    let env = Env::new();
    let target = platform_target().expect("supported target");
    // `<data>/runtimes/onnxruntime/<version>/<triple>/<library>`: built from
    // the env tempdir because `library_path()` resolves against THIS
    // process's DOCSBASE_DATA_DIR, not the child's.
    let name = std::path::Path::new(target.library_path)
        .file_name()
        .expect("library file name");
    let install_dir = env
        .data
        .path()
        .join("runtimes")
        .join("onnxruntime")
        .join(ORT_VERSION)
        .join(target.triple);
    std::fs::create_dir_all(&install_dir).expect("install dir");
    std::fs::write(install_dir.join(name), b"fake-runtime").expect("fake library");

    let output = env
        .cmd()
        .args(["connect", "fastembed"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("installed:"),
        "reports the install: {stdout}"
    );
    assert!(
        !stdout.contains("restart a running detached daemon"),
        "no restart note on a no-op connect: {stdout}"
    );
}

#[test]
fn unsupported_provider_name_is_clap_error() {
    let env = Env::new();
    env.cmd()
        .args(["connect", "no-such-provider"])
        .assert()
        .failure();
}
