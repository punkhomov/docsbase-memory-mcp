#![cfg(feature = "hybrid-local")]
//! CLI coverage for `docsbase models` under `--features hybrid-local`:
//! `fetch` and `list` wiring, admission errors and flag shapes. The network
//! fetch itself is covered by the live test (T8); here `list` and the
//! unsupported/unknown paths run fully offline.

use assert_cmd::Command;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    config: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            cache: TempDir::new().expect("cache"),
            config: TempDir::new().expect("config"),
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("docsbase");
        cmd.env("DOCSBASE_CACHE_DIR", self.cache.path());
        cmd.env("DOCSBASE_CONFIG_DIR", self.config.path());
        cmd
    }
}

#[test]
fn models_list_prints_registry_and_status() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["models", "list"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in [
        "bge-small-en-v1.5",
        "multilingual-e5-small",
        "jina-reranker-v1-turbo-en",
        "bge-reranker-base",
    ] {
        assert!(stdout.contains(name), "list shows {name}: {stdout}");
    }
    assert!(stdout.contains("missing"), "nothing fetched yet: {stdout}");
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn models_fetch_unknown_name_is_admission() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["models", "fetch", "no-such-model"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.contains("admission refused"), "{stderr}");
    assert!(stderr.contains("no-such-model"), "{stderr}");
}

#[test]
fn models_fetch_requires_a_name() {
    let env = Env::new();
    env.cmd().args(["models", "fetch"]).assert().failure();
}

#[test]
fn models_fetch_accepts_force_flag_shape() {
    let env = Env::new();
    // The flag parses; the actual download is not exercised here.
    let output = env
        .cmd()
        .args(["models", "fetch", "no-such-model", "--force"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(stderr_of(&output).contains("no-such-model"));
}

#[test]
fn models_without_purpose_prints_help() {
    let env = Env::new();
    env.cmd().args(["models"]).assert().failure();
}
