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
#[cfg(unix)]
#[must_use]
pub fn is_under(candidate: &Path, base: &Path) -> bool {
    candidate.starts_with(base)
}

/// Windows containment: compare folded keys so case and `/` vs `\` do not
/// defeat the check (ADR-10, FR-32).
#[cfg(windows)]
#[must_use]
pub fn is_under(candidate: &Path, base: &Path) -> bool {
    key_under(&windows_key(candidate), &windows_key(base))
}

/// Shared-root test on an already-normalized key: equal, or a prefix at a
/// separator boundary (`/docs` is not under `/doc`).
#[cfg(any(windows, test))]
#[must_use]
fn key_under(candidate: &str, base: &str) -> bool {
    let base = base.strip_suffix('/').unwrap_or(base);
    candidate == base
        || candidate
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True for a filesystem root (`/` on Unix, `C:\` or a UNC root on Windows):
/// project roots must live below one (I7).
#[must_use]
pub fn is_filesystem_root(path: &Path) -> bool {
    path.has_root() && path.parent().is_none()
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
    fn is_filesystem_root_is_platform_root_only() {
        assert!(is_filesystem_root(Path::new("/")), "unix root");
        assert!(!is_filesystem_root(Path::new("/home/user")), "below root");
        assert!(!is_filesystem_root(Path::new("")), "empty path");
        assert!(!is_filesystem_root(Path::new("relative")), "relative path");
    }

    #[test]
    fn windows_containment_folds_case_and_separators() {
        let base = windows_key(Path::new(r"C:\Users\X\Docs"));
        let candidate = windows_key(Path::new("c:/users/x/docs/a.md"));
        assert!(key_under(&candidate, &base), "case and separators fold");
        assert!(key_under(&base, &base), "equality");
        let sibling = windows_key(Path::new(r"C:\Users\X\Docs2\a.md"));
        assert!(!key_under(&sibling, &base), "component boundary");
        assert!(key_under("c:/users/x", "c:/"), "trailing separator in base");
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
