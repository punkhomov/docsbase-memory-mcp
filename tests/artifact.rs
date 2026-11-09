#![cfg(target_os = "linux")]
//! Release artifact guarantees (NFR-2, NFR-6): the release binary is static
//! (no dynamic library dependencies) and fits the size budget.
//!
//! Opt-in consumer of a prebuilt artifact. First run `cargo release-static
//! --locked`, then `cargo test --locked --features cli-full --test artifact`.
//! Workflows always build first: an existing file alone may be stale.

use std::io::Read;
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
    let binary = target_dir().join("x86_64-unknown-linux-gnu/release/docsbase");
    assert!(
        binary.is_file(),
        "static artifact missing at {}; run cargo release-static --locked before these checks",
        binary.display()
    );
    binary
}

fn ldd_output(binary: &std::path::Path) -> String {
    let output = Command::new("ldd").arg(binary).output().expect("run ldd");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    // GNU ldd returns 1 for a valid fully static executable.
    assert!(
        matches!(output.status.code(), Some(0 | 1)) && !text.trim().is_empty(),
        "unexpected ldd result for {}: {output:?}",
        binary.display()
    );
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
    let binary = static_binary();
    let mut magic = [0; 4];
    std::fs::File::open(&binary)
        .expect("open static artifact")
        .read_exact(&mut magic)
        .expect("read ELF header");
    assert_eq!(
        &magic, b"\x7fELF",
        "static artifact must be an ELF executable"
    );
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
    let binary = static_binary();
    let size = std::fs::metadata(&binary).expect("stat binary").len();
    eprintln!(
        "size {}: {size} bytes (budget {SIZE_BUDGET})",
        binary.display()
    );
    assert!(size > 0, "binary is empty");
    assert!(
        size <= SIZE_BUDGET,
        "release binary is {size} bytes, over the {SIZE_BUDGET}-byte budget (NFR-2)"
    );
}
