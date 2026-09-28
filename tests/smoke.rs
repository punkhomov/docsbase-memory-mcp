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
    assert!(
        cfg.contains("global-min-publish-age = \"14 days\""),
        "{cfg}"
    );
    assert!(cfg.contains("incompatible-publish-age = \"deny\""), "{cfg}");
}
