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

/// Directories that never become a project root (T47, ADR-11): system trees
/// and parents of user homes. `path` is expected canonicalized.
#[must_use]
pub fn is_system_dir(path: &Path) -> bool {
    #[cfg(unix)]
    {
        const EQUALS: &[&str] = &["/home", "/var", "/mnt", "/media"];
        const SUBTREES: &[&str] = &[
            "/etc",
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib32",
            "/lib64",
            "/boot",
            "/proc",
            "/sys",
            "/dev",
            "/run",
            "/root",
            "/snap",
            "/var/lib",
            "/var/cache",
            "/var/log",
            "/var/spool",
        ];
        EQUALS.iter().any(|dir| path == Path::new(dir))
            || SUBTREES.iter().any(|dir| path.starts_with(Path::new(dir)))
    }
    #[cfg(windows)]
    {
        let (subtrees, equals) = windows_system_dirs();
        is_system_key(&windows_key(path), &subtrees, &equals)
    }
}

/// Windows system trees from the environment, as folded keys.
#[cfg(windows)]
fn windows_system_dirs() -> (Vec<String>, Vec<String>) {
    fn key(value: &std::ffi::OsStr) -> String {
        windows_key(Path::new(value))
    }
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let drive = key(&system_root)
        .split('/')
        .next()
        .unwrap_or("c:")
        .to_owned();
    let mut subtrees = vec![key(&system_root)];
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Some(value) = std::env::var_os(var) {
            subtrees.push(key(&value));
        }
    }
    subtrees.push(format!("{drive}/$recycle.bin"));
    subtrees.push(format!("{drive}/recovery"));
    subtrees.push(format!("{drive}/perflogs"));
    subtrees.push(format!("{drive}/system volume information"));
    let equals = vec![format!("{drive}/users")];
    (subtrees, equals)
}

/// Folded-key version of [`is_system_dir`] (Windows semantics; unit-testable
/// on every host).
#[cfg(any(windows, test))]
#[must_use]
fn is_system_key(candidate: &str, subtrees: &[String], equals: &[String]) -> bool {
    equals.iter().any(|dir| candidate == dir)
        || subtrees.iter().any(|dir| key_under(candidate, dir))
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
    fn system_dirs_are_refused_and_workdirs_allowed() {
        #[cfg(unix)]
        {
            for dir in [
                "/etc",
                "/etc/nginx",
                "/usr",
                "/usr/local/x",
                "/bin",
                "/boot/grub",
                "/proc",
                "/sys/fs/cgroup",
                "/dev",
                "/run",
                "/root/x",
                "/var",
                "/var/lib/docker",
                "/var/log",
                "/home",
                "/mnt",
                "/media",
                "/snap/x",
            ] {
                assert!(is_system_dir(Path::new(dir)), "{dir} must be refused");
            }
            for dir in [
                "/home/user/project",
                "/tmp/x",
                "/var/www/site",
                "/opt/app",
                "/mnt/disk/project",
                "/media/user/disk/project",
                "/srv/www",
            ] {
                assert!(!is_system_dir(Path::new(dir)), "{dir} must stay allowed");
            }
        }
    }

    #[test]
    fn windows_system_keys_are_refused() {
        let subtrees = vec![
            "c:/windows".to_owned(),
            "c:/program files".to_owned(),
            "c:/program files (x86)".to_owned(),
            "c:/programdata".to_owned(),
            "c:/$recycle.bin".to_owned(),
            "d:/recovery".to_owned(),
        ];
        let equals = vec!["c:/users".to_owned()];
        assert!(is_system_key("c:/windows/system32", &subtrees, &equals));
        assert!(is_system_key("c:/program files/app", &subtrees, &equals));
        assert!(is_system_key("c:/users", &subtrees, &equals), "equality");
        assert!(is_system_key("d:/recovery", &subtrees, &equals));
        assert!(!is_system_key("c:/users/me/project", &subtrees, &equals));
        assert!(!is_system_key("d:/projects/x", &subtrees, &equals));
        assert!(
            !is_system_key("c:/windows2", &subtrees, &equals),
            "component boundary"
        );
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
