//! Path semantics seam (ADR-9, T36): home discovery, containment and the
//! comparison key used for project/cache roots.
//!
//! Everything else in the crate treats paths portably; `$HOME`/`UserProfile`
//! are read only here.

use std::path::{Path, PathBuf};

/// Home directory via the `directories` crate (XDG/known folders).
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Component-wise containment (`candidate` equals `base` or lives under it).
/// Callers canonicalize symlinked ancestors first (FR-32, I4).
#[must_use]
pub fn is_under(candidate: &Path, base: &Path) -> bool {
    candidate.starts_with(base)
}

/// Comparison key for root paths: identity on Unix; separator and case
/// folded on Windows, where the filesystem is case-insensitive.
#[must_use]
pub fn normalize_for_compare(path: &Path) -> String {
    #[cfg(windows)]
    {
        windows_key(path)
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

/// Windows comparison key (`/` separators, lowercase); compiled under
/// `cfg(test)` on Linux so the folding stays covered (ADR-9, T36).
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn windows_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_available() {
        let home = home_dir().expect("home must resolve in tests");
        assert!(home.is_absolute(), "home {home:?} must be absolute");
    }

    #[test]
    fn is_under_rejects_siblings() {
        assert!(is_under(Path::new("/a/b"), Path::new("/a")));
        assert!(is_under(Path::new("/a"), Path::new("/a")), "equality");
        assert!(
            !is_under(Path::new("/a/bc"), Path::new("/a/b")),
            "component-wise, not string prefix"
        );
        assert!(!is_under(Path::new("/x"), Path::new("/a")));
    }

    #[test]
    fn windows_key_normalizes() {
        assert_eq!(
            windows_key(Path::new(r"C:\Users\X\Docs")),
            "c:/users/x/docs"
        );
        assert_eq!(windows_key(Path::new("c:/Users/x/DOCS")), "c:/users/x/docs");
    }
}
