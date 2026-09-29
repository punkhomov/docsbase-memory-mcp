//! Platform seam invariant (ADR-9, T34): OS transport calls live only in
//! `src/platform/`. Signals/process/permissions/path patterns are added by
//! T35/T36.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Patterns that must not appear outside `src/platform/` (ADR-9).
const BANNED: &[&str] = &[
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
    // Bare types catch grouped/aliased imports where the module prefix is
    // split across braces (`use tokio::net::{TcpListener, UnixListener}`).
    "UnixListener",
    "UnixStream",
    "UnixDatagram",
    "std::os::unix",
    "tokio::net",
];

/// Test modules may use OS facilities to build fakes; only production code
/// counts, so each file is truncated at its first `#[cfg(test)] mod` marker.
/// Any other `#[cfg(test)]` item fails safe: the scan continues past it.
fn production_prefix(text: &str) -> &str {
    let marker = "#[cfg(test)]";
    let mut search_from = 0;
    while let Some(found) = text[search_from..].find(marker) {
        let index = search_from + found;
        let rest = text[index + marker.len()..].trim_start();
        if rest.starts_with("mod") {
            return &text[..index];
        }
        search_from = index + marker.len();
    }
    text
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
    let samples = [
        (
            "grouped",
            "use std::os::unix::{net::UnixStream, fs::PermissionsExt};\n",
        ),
        (
            "sorted_group",
            "use tokio::net::{TcpListener, UnixListener};\n",
        ),
        ("multiline", "use tokio::net::{\n    UnixListener,\n};\n"),
        ("aliased", "use tokio::net as local_net;\n"),
    ];
    for (name, body) in samples {
        let path = dir.path().join(format!("{name}.rs"));
        fs::write(&path, body).expect("write sample");
        let mut violations = Vec::new();
        check_file(&path, BANNED, &mut violations);
        assert!(
            !violations.is_empty(),
            "{name} import must be caught: {body:?}"
        );
    }
}

#[test]
fn no_os_transport_outside_platform() {
    let src = manifest_dir().join("src");
    let mut violations = Vec::new();
    for entry in fs::read_dir(&src).expect("read src") {
        let path = entry.expect("entry").path();
        if path.file_name().is_some_and(|name| name == "platform") {
            continue;
        }
        if path.is_dir() {
            scan_dir(&path, BANNED, &mut violations);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            check_file(&path, BANNED, &mut violations);
        }
    }
    assert!(
        violations.is_empty(),
        "OS calls outside src/platform/ (ADR-9):\n{}",
        violations.join("\n")
    );
}
