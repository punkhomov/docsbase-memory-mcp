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
        // Host-independent: `runtime::resolve` honours `ORT_DYLIB_PATH`
        // before any provider/data-dir fallback, so an exported override
        // would turn the asserted connect/fetch hints into a load error
        // (round-7 C2).
        cmd.env_remove("ORT_DYLIB_PATH");
        cmd.env_remove("DOCSBASE_DATA_DIR");
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
            "fastembed/bge-small-en-v1.5",
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
            "openrouter/m/e",
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
            "openrouter/m/e",
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
            "openrouter/m/e",
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
            "fastembed/jina-reranker-v1-turbo-en",
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
            "fastembed/bge-small-en-v1.5",
            "--window",
            "20",
            "--dry-run",
        ])
        .assert()
        .failure();
}

#[test]
fn models_use_dry_run_previews_full_merged_file() {
    // C12: `--dry-run` must render the file as it would be written,
    // preserving unrelated keys from the existing project file.
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(
        dir.path().join(".docsbase.toml"),
        "other = 42\n\n[unrelated]\nkey = \"kept\"\n",
    )
    .expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args([
            "models",
            "use",
            "embed",
            "fastembed/bge-small-en-v1.5",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("other = 42"), "{stdout}");
    assert!(stdout.contains("key = \"kept\""), "{stdout}");
    assert!(stdout.contains("bge-small-en-v1.5"), "{stdout}");
}

#[test]
fn bare_model_name_is_rejected_with_the_corrected_address() {
    // FR-2: no provider inference from a bare name — the admission must hand
    // back the exact command to run, using the catalog to build it.
    let env = Env::new();
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let output = env
        .cmd()
        .current_dir(dir.path())
        .args(["models", "use", "embed", "bge-small-en-v1.5", "--dry-run"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("fastembed/bge-small-en-v1.5"),
        "admission must show the corrected address: {stderr}"
    );
    assert!(stderr.contains("providers:"), "{stderr}");
}

#[test]
fn unknown_provider_in_an_address_is_an_admission() {
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
    let stderr = stderr_of(&output);
    assert!(stderr.contains("unknown provider"), "{stderr}");
    assert!(stderr.contains("fastembed"), "{stderr}");
    assert!(stderr.contains("openrouter"), "{stderr}");
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
            "openrouter/some/model",
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
        .args(["models", "use", "embed", "fastembed/bge-small-en-v1.5"])
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
        .args(["models", "use", "embed", "fastembed/bge-small-en-v1.5"])
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
fn search_admits_unfetched_fastembed_with_real_provider_record() {
    // C5 sibling guard: the `docsbase search` product entry point threads
    // the real `ProvidersCfg` identically. Step 1 registers the project
    // with a pure-lexical config; step 2 pins fastembed in the project and
    // adds a global provider record with a dangling runtime_path — the
    // search must fail with the resolve-stage admission (runtime + --force),
    // never "is not connected" (which would indicate a `default()`).
    let env = Env::new();
    let proj = TempDir::new().expect("proj");
    std::fs::write(proj.path().join(".docsbase.toml"), "[hybrid]\n").expect("project file");
    env.cmd()
        .args(["index", proj.path().to_str().expect("utf8")])
        .assert()
        .success();
    std::fs::write(
        proj.path().join(".docsbase.toml"),
        "[hybrid.embedding]\nprovider = \"fastembed\"\nmodel = \"bge-small-en-v1.5\"\n",
    )
    .expect("project pin");
    std::fs::write(
        env.config.path().join("config.toml"),
        "[providers.fastembed]\nenabled = true\nruntime = \"1.24.4\"\nruntime_path = \"/nonexistent/libonnxruntime.so\"\n",
    )
    .expect("global config");
    let output = env
        .cmd()
        .current_dir(proj.path())
        .args(["search", "hello"])
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
        .args(["models", "remove", "fastembed/bge-small-en-v1.5"])
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
        .args(["models", "remove", "fastembed/bge-small-en-v1.5"])
        .assert()
        .success();
    assert!(!model_dir.exists(), "cache dir removed");
}
