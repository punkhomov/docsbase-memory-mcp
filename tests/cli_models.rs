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

#[test]
fn models_use_without_purpose_fails() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    env.cmd()
        .current_dir(dir.path())
        .args(["models", "use"])
        .assert()
        .failure();
}

#[test]
fn models_use_embed_dry_run_prints_table_without_writing() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "bge-small-en-v1.5",
            "--provider",
            "fastembed",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[hybrid]"), "{stdout}");
    assert!(stdout.contains("bge-small-en-v1.5"), "{stdout}");
    assert!(stdout.contains("384"), "{stdout} registry dims inferred");
    // Nothing written: seeded project file is untouched.
    let project_file = std::fs::read_to_string(dir.path().join(".docsbase.toml")).expect("read");
    assert_eq!(project_file, "[hybrid]\n");
}

#[test]
fn models_use_outside_project_requires_global() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp"); // no .docsbase.toml
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "m/e",
            "--provider",
            "openrouter",
            "--dims",
            "128",
            "--dry-run",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--global"), "{stderr}");
}

#[test]
fn models_use_both_scope_flags_error() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    env.cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "m/e",
            "--provider",
            "openrouter",
            "--dims",
            "128",
            "--local",
            "--global",
        ])
        .assert()
        .failure();
}

#[test]
fn models_use_path_with_global_errors() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    env.cmd()
        .args([
            "models",
            "use",
            "embed",
            "m/e",
            "--provider",
            "openrouter",
            "--dims",
            "128",
            "--global",
            "--path",
        ])
        .arg(dir.path())
        .assert()
        .failure();
}

#[test]
fn models_use_dims_only_on_embed() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    // --dims is illegal on rerank.
    env.cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "rerank",
            "jina-reranker-v1-turbo-en",
            "--provider",
            "fastembed",
            "--dims",
            "128",
            "--dry-run",
        ])
        .assert()
        .failure();
    // --window is illegal on embed.
    env.cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "bge-small-en-v1.5",
            "--provider",
            "fastembed",
            "--window",
            "20",
            "--dry-run",
        ])
        .assert()
        .failure();
}

#[test]
fn models_use_unknown_model_requires_provider() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "some/model",
            "--dims",
            "128",
            "--dry-run",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--provider"), "{stderr}");
}

#[test]
fn models_use_openrouter_requires_dims() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "some/model",
            "--provider",
            "openrouter",
            "--dry-run",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--dims"), "{stderr}");
}

#[test]
fn models_use_fastembed_unfetched_model_gives_fetch_hint() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    // Configure the runtime so probe reaches the model-cache check.
    let runtime = env.cache.path().join("libonnxruntime.so");
    std::fs::write(&runtime, b"fake").expect("touch");
    std::fs::write(
        env.config.path().join("config.toml"),
        format!(
            "[providers.fastembed]\nenabled = true\nruntime_path = \"{}\"\n",
            runtime.display()
        ),
    )
    .expect("global config");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "bge-small-en-v1.5",
            "--provider",
            "fastembed",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("models fetch"), "{stderr}");
}

#[test]
fn models_use_fastembed_without_runtime_gives_connect_hint() {
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "bge-small-en-v1.5",
            "--provider",
            "fastembed",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("connect fastembed"), "{stderr}");
}

#[test]
fn index_admits_unfetched_fastembed_with_real_provider_record() {
    // AC3 wiring regression (review panel C5): a real product entry point
    // (`docsbase index`) must thread the real `ProvidersCfg` into
    // `from_config`. The discriminator: with a provider record whose
    // `runtime_path` dangles, the resolve-stage admission names the runtime
    // and the `--force` remediation — a `ProvidersCfg::default()` would
    // instead report "provider is not connected".
    let env = Env::new();
    let proj = TempDir::new().expect("proj");
    std::fs::write(
        proj.path().join(".docsbase.toml"),
        "[hybrid.embedding]\nprovider = \"fastembed\"\nmodel = \"bge-small-en-v1.5\"\n",
    )
    .expect("project file");
    std::fs::write(
        env.config.path().join("config.toml"),
        "[providers.fastembed]\nenabled = true\nruntime = \"1.24.4\"\nruntime_path = \"/nonexistent/libonnxruntime.so\"\n",
    )
    .expect("global config");
    let output = env
        .cmd()
        .args(["index", proj.path().to_str().expect("utf8")])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.contains("fastembed runtime"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");
    assert!(!stderr.contains("is not connected"), "{stderr}");
}

#[test]
fn models_remove_unknown_name_is_admission() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["models", "remove", "no-such-model"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(stderr_of(&output).contains("no-such-model"));
}

#[test]
fn models_remove_not_fetched_is_admission() {
    let env = Env::new();
    let output = env
        .cmd()
        .args(["models", "remove", "bge-small-en-v1.5"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(stderr.contains("not fetched"), "{stderr}");
}

#[test]
fn models_remove_unlocked_succeeds() {
    let env = Env::new();
    // Fabricate a cached model dir (revision = registry pin) + .lock file.
    let revision = "ea104dacec62c0de699686887e3f920caeb4f3e3";
    let model_dir = env
        .cache
        .path()
        .join("models")
        .join("bge-small-en-v1.5")
        .join(revision);
    std::fs::create_dir_all(&model_dir).expect("model dir");
    std::fs::write(model_dir.join("model.onnx"), b"fake").expect("file");
    env.cmd()
        .args(["models", "remove", "bge-small-en-v1.5"])
        .assert()
        .success();
    assert!(!model_dir.exists(), "cache dir removed");
}
