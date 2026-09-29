//! Release artifact guarantees (NFR-2, NFR-6): the release binary is static
//! (no dynamic library dependencies) and fits the size budget.

use std::path::PathBuf;
use std::process::Command;

/// NFR-2: capped at 40 MiB stripped.
const SIZE_BUDGET: u64 = 40 * 1024 * 1024;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `cargo release-static` honors `CARGO_TARGET_DIR`; mirror it here.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| manifest_dir().join("target"), PathBuf::from)
}

/// Static artifact path produced by `cargo release-static` (ADR-8): the
/// target-scoped rustflags only apply to real `--target` builds, so the host
/// `target/release` binary stays dynamic for local runs.
fn static_binary() -> PathBuf {
    target_dir().join("x86_64-unknown-linux-gnu/release/docsbase")
}

/// Builds the static artifact on demand (fast when fresh) and never falls
/// back to the dynamic host binary: `cargo test --test artifact` must always
/// validate the artifact CI will ship, not a stale or wrong one.
fn ensure_release_binary() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["release-static"])
        .current_dir(manifest_dir())
        .status()
        .expect("run cargo release-static");
    assert!(status.success(), "cargo release-static failed (ADR-8)");
    let binary = static_binary();
    assert!(
        binary.exists(),
        "static artifact missing at {} after cargo release-static",
        binary.display()
    );
    binary
}

fn ldd_output(binary: &std::path::Path) -> String {
    let output = Command::new("ldd").arg(binary).output().expect("run ldd");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Dynamic entries that `ldd` may still print for a static binary.
fn is_allowed(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("not a dynamic executable")
        || lower.contains("statically linked")
        || lower.contains("linux-vdso")
        || lower.contains("ld-linux")
}

#[test]
fn static_link_check() {
    let binary = ensure_release_binary();
    let text = ldd_output(&binary);
    eprintln!("ldd {}:\n{text}", binary.display());
    let dynamic: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_allowed(line))
        .collect();
    assert!(
        dynamic.is_empty(),
        "release binary must not depend on dynamic libraries (NFR-6): {dynamic:?}"
    );
}

#[test]
fn size_budget() {
    let binary = ensure_release_binary();
    let size = std::fs::metadata(&binary).expect("stat binary").len();
    assert!(size > 0, "binary is empty");
    assert!(
        size <= SIZE_BUDGET,
        "release binary is {size} bytes, over the {SIZE_BUDGET}-byte budget (NFR-2)"
    );
}
