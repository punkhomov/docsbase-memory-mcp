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
        const EQUALS: &[&str] = &["/home", "/root", "/var", "/mnt", "/media", "/run"];
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
            "/snap",
            "/var/lib",
            "/var/cache",
            "/var/log",
            "/var/spool",
        ];
        // Removable mounts live below /run/media (udisks); real projects.
        if path.starts_with("/run/media") {
            return false;
        }
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
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let program_files = std::env::var_os("ProgramFiles");
    let program_files_x86 = std::env::var_os("ProgramFiles(x86)");
    let program_data = std::env::var_os("ProgramData");
    windows_system_dirs_from(
        &system_root,
        program_files.as_deref(),
        program_files_x86.as_deref(),
        program_data.as_deref(),
    )
}

/// Pure builder behind [`windows_system_dirs`], unit-testable everywhere.
#[cfg(any(windows, test))]
#[must_use]
fn windows_system_dirs_from(
    system_root: &std::ffi::OsStr,
    program_files: Option<&std::ffi::OsStr>,
    program_files_x86: Option<&std::ffi::OsStr>,
    program_data: Option<&std::ffi::OsStr>,
) -> (Vec<String>, Vec<String>) {
    fn key(value: &std::ffi::OsStr) -> String {
        windows_key(Path::new(value))
    }
    let system_root = key(system_root);
    let drive = system_root.split('/').next().unwrap_or("c:").to_owned();
    let mut subtrees = vec![system_root];
    for value in [program_files, program_files_x86, program_data]
        .into_iter()
        .flatten()
    {
        subtrees.push(key(value));
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
    // Windows `canonicalize` yields verbatim (`\\?\`) paths; the comparison
    // keys are plain, so drop the prefix first (T47 review finding).
    let candidate = candidate
        .strip_prefix("//?/")
        .map_or(candidate, |stripped| stripped);
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
    let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
    // Windows canonicalization yields verbatim (`\\?\`) paths; they denote
    // the same file as the plain spelling, so fold the prefix away.
    key.strip_prefix("//?/").map_or(key.clone(), str::to_owned)
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
                "/root/project",
                "/tmp/x",
                "/var/www/site",
                "/opt/app",
                "/mnt/disk/project",
                "/media/user/disk/project",
                "/run/media/user/disk/project",
                "/srv/www",
                "/usr2/x",
                "/libx/x",
                "/var/www2",
                "/home2/x",
                "/etcetera/x",
            ] {
                assert!(!is_system_dir(Path::new(dir)), "{dir} must stay allowed");
            }
        }
    }

    #[test]
    fn windows_system_keys_are_refused() {
        let (subtrees, equals) = windows_system_dirs_from(
            std::ffi::OsStr::new(r"C:\Windows"),
            Some(std::ffi::OsStr::new(r"C:\Program Files")),
            Some(std::ffi::OsStr::new(r"C:\Program Files (x86)")),
            Some(std::ffi::OsStr::new(r"C:\ProgramData")),
        );
        assert!(is_system_key("c:/windows/system32", &subtrees, &equals));
        assert!(is_system_key("c:/program files/app", &subtrees, &equals));
        assert!(is_system_key(
            "c:/program files (x86)/app",
            &subtrees,
            &equals
        ));
        assert!(is_system_key("c:/programdata/x", &subtrees, &equals));
        assert!(is_system_key("c:/$recycle.bin", &subtrees, &equals));
        assert!(is_system_key("c:/perflogs", &subtrees, &equals));
        assert!(is_system_key("c:/users", &subtrees, &equals), "equality");
        assert!(
            is_system_key("//?/c:/windows/system32", &subtrees, &equals),
            "verbatim canonical paths must still match"
        );
        assert!(!is_system_key("c:/users/me/project", &subtrees, &equals));
        assert!(!is_system_key("d:/projects/x", &subtrees, &equals));
        assert!(
            !is_system_key("c:/windows2", &subtrees, &equals),
            "component boundary"
        );
    }

    #[test]
    fn windows_key_strips_verbatim_prefix() {
        assert_eq!(
            windows_key(Path::new(r"\\?\C:\Users\X\Docs")),
            "c:/users/x/docs"
        );
        assert!(key_under(
            &windows_key(Path::new(r"\\?\C:\Root\Docs\a.md")),
            &windows_key(Path::new(r"C:\Root\Docs"))
        ));
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
