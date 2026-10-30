use std::process::Command;

#[test]
fn version_prints() {
    let bin = env!("CARGO_BIN_EXE_docsbase");
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .expect("run docsbase --version");
    assert!(out.status.success(), "exit status: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout: {stdout}"
    );
}

#[test]
fn toolchain_pinned() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/rust-toolchain.toml"))
            .expect("read rust-toolchain.toml");
    assert!(manifest.contains("channel = \"nightly\""), "{manifest}");
    assert!(manifest.contains("\"rustfmt\""), "{manifest}");
    assert!(manifest.contains("\"clippy\""), "{manifest}");
}

#[test]
fn min_publish_age_configured() {
    let cfg = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/.cargo/config.toml"))
        .expect("read .cargo/config.toml");
    // Policy default since ann-index T1.5 (2026-10-23): the temporary 13-day
    // exception for hnsw_rs 0.3.5 is reverted; 0.3.5 stays pinned in the lock.
    assert!(
        cfg.contains("global-min-publish-age = \"14 days\""),
        "{cfg}"
    );
    assert!(cfg.contains("incompatible-publish-age = \"deny\""), "{cfg}");
}

/// End-to-end `models use --dry-run`: real binary, arg parsing, merge and
/// render through the CLI — the unit tests only cover `resolve_hybrid_table`
/// in-process. The dry-run path needs no key and writes nothing.
#[test]
fn models_use_dry_run_prints_pinnable_table() {
    let dir = tempfile::TempDir::new().expect("tmp");
    // A project file is required for the default scope; seed an empty one
    // so the dry-run resolves the project target (FR-8).
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let out = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args([
            "models",
            "use",
            "embed",
            "openrouter/m/e",
            "--dims",
            "128",
            "--dry-run",
            "--path",
        ])
        .arg(dir.path())
        .output()
        .expect("run models use");
    assert!(
        out.status.success(),
        "exit status: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[hybrid]"),
        "root-shaped table required for paste-in: {stdout}"
    );
    assert!(stdout.contains("m/e"), "{stdout}");
    assert!(stdout.contains("128"), "{stdout}");
    let project_file = std::fs::read_to_string(dir.path().join(".docsbase.toml")).expect("read");
    assert_eq!(
        project_file, "[hybrid]\n",
        "dry-run must not write anything"
    );
}

/// Bare `models use` must fail loudly (no purpose), not silently succeed.
#[test]
fn models_use_without_purpose_fails() {
    let dir = tempfile::TempDir::new().expect("tmp");
    std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("seed");
    let out = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args(["models", "use", "--path"])
        .arg(dir.path())
        .output()
        .expect("run models use");
    assert!(
        !out.status.success(),
        "bare models use must fail: {:?}",
        out.status
    );
}
