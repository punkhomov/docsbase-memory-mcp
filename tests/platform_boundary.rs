//! Platform seam invariant (ADR-9, T34): OS transport calls live only in
//! `src/platform/`. Signals/process/permissions/path patterns are added by
//! T35/T36.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Test modules may use OS facilities to build fakes; only production code
/// counts, so each file is truncated at its first `#[cfg(test)]` marker.
fn production_prefix(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(index) => &text[..index],
        None => text,
    }
}

fn check_file(path: &Path, banned: &[&str], violations: &mut Vec<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let production = production_prefix(&text);
    // Normalize whitespace and braces so grouped/multiline imports
    // (`use tokio::net::{UnixListener, …}`) cannot slip past.
    let normalized: String = production
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '{' && *ch != '}')
        .collect();
    for pattern in banned {
        let compact: String = pattern.chars().filter(|ch| !ch.is_whitespace()).collect();
        if normalized.contains(&compact) {
            violations.push(format!(
                "{}: {pattern}",
                path.strip_prefix(manifest_dir()).unwrap_or(path).display()
            ));
        }
    }
}

fn scan_dir(dir: &Path, banned: &[&str], violations: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            // Only `src/platform/` itself is exempt; a nested directory with
            // the same name must not escape the invariant.
            scan_dir(&path, banned, violations);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            check_file(&path, banned, violations);
        }
    }
}

#[test]
fn scanner_catches_grouped_and_multiline_imports() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let grouped = dir.path().join("grouped.rs");
    fs::write(
        &grouped,
        "use std::os::unix::{net::UnixStream, fs::PermissionsExt};\n",
    )
    .expect("write");
    let multiline = dir.path().join("multiline.rs");
    fs::write(&multiline, "use tokio::net::{\n    UnixListener,\n};\n").expect("write");

    let banned = ["std::os::unix::net", "tokio::net::Unix"];
    let mut violations = Vec::new();
    check_file(&grouped, &banned, &mut violations);
    check_file(&multiline, &banned, &mut violations);
    assert_eq!(
        violations.len(),
        2,
        "grouped and multiline imports must be caught: {violations:?}"
    );
}

#[test]
fn no_os_transport_outside_platform() {
    let src = manifest_dir().join("src");
    let banned = [
        "std::os::unix::net",
        "tokio::net::Unix",
        "/proc/",
        "SignalKind",
        "process_group",
        "PermissionsExt",
        "DirBuilderExt",
        "OpenOptionsExt",
        "var_os(\"HOME\")",
        "var(\"HOME\")",
        "env!(\"HOME\")",
    ];
    let mut violations = Vec::new();
    for entry in fs::read_dir(&src).expect("read src") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "platform") {
                continue;
            }
            scan_dir(&path, &banned, &mut violations);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            check_file(&path, &banned, &mut violations);
        }
    }
    assert!(
        violations.is_empty(),
        "OS transport outside src/platform/ (ADR-9):\n{}",
        violations.join("\n")
    );
}
