//! Bounded state reads and log rotation shared across modules (T48).

use std::path::{Path, PathBuf};

/// Largest accepted `daemon.json`/`install.json`; bigger files are treated as
/// corrupt instead of being read into memory.
pub const MAX_STATE_BYTES: u64 = 1_048_576;

/// Log files are rotated to `<name>.old` once they exceed this size.
pub const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// Rotates `path` next to itself when it exceeds [`MAX_LOG_BYTES`].
///
/// Replacement is Windows-safe (`remove_file` first) and `Ok(false)` when the
/// file is missing or small. Callers treat rotation failures as warnings.
///
/// # Errors
/// Returns IO errors from `remove_file`/`rename`.
pub fn rotate_log(path: &Path) -> std::io::Result<bool> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if metadata.len() <= MAX_LOG_BYTES {
        return Ok(false);
    }
    let mut old = path.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    match std::fs::remove_file(&old) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::rename(path, &old)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rotates_only_large_logs() {
        let dir = TempDir::new().expect("dir");
        let path = dir.path().join("daemon.log");
        assert!(!rotate_log(&path).expect("missing file is a no-op"));

        std::fs::write(&path, b"small").expect("write");
        assert!(!rotate_log(&path).expect("small file is a no-op"));

        let big = std::fs::File::create(&path).expect("create");
        big.set_len(MAX_LOG_BYTES + 1).expect("grow");
        drop(big);
        assert!(rotate_log(&path).expect("rotate"));
        assert!(!path.exists());
        assert!(dir.path().join("daemon.log.old").is_file());

        // A second rotation replaces the existing `.old` (Windows-safe path).
        let big = std::fs::File::create(&path).expect("create again");
        big.set_len(MAX_LOG_BYTES + 2).expect("grow again");
        drop(big);
        assert!(rotate_log(&path).expect("rotate again"));
        assert_eq!(
            std::fs::metadata(dir.path().join("daemon.log.old"))
                .expect("old")
                .len(),
            MAX_LOG_BYTES + 2
        );
    }
}
