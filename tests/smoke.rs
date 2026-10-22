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
    // TEMPORARY 2026-10-22: 13 days to resolve hnsw_rs 0.3.5; revert on
    // 2026-10-23 together with .cargo/config.toml.
    assert!(
        cfg.contains("global-min-publish-age = \"13 days\""),
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
    let out = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args([
            "models",
            "use",
            "--embedding",
            "m/e",
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
    assert!(
        std::fs::read_dir(dir.path()).expect("ls").next().is_none(),
        "dry-run must not write anything"
    );
}

/// Bare `models use` must fail loudly (nothing to set), not silently succeed.
#[test]
fn models_use_without_flags_fails() {
    let dir = tempfile::TempDir::new().expect("tmp");
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
