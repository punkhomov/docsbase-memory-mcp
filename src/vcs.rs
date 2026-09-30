//! Git state of a project root: branch, HEAD and worktree layout (T49).
//!
//! Read-only and dependency-free. The reader understands the `.git` file
//! (gitlink) of linked worktrees, their `commondir` and per-worktree `HEAD`,
//! resolves loose refs and falls back to a bounded scan of `packed-refs`.
//!
//! The `.git` file is untrusted input: only `HEAD`, `commondir` and
//! `packed-refs` are ever opened, every read is bounded, symlinks are not
//! traversed beyond those file opens, and extracted values must match their
//! expected shape or the field is reported as `None`. This keeps the probe
//! display-only: containment and indexing keep using `canonical_root`.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// Bound for single-line metadata files (`HEAD`, `commondir`, gitdir).
const META_READ_LIMIT: u64 = 4 * 1024;
/// Overall bound for the `packed-refs` scan.
const PACKED_REFS_LIMIT: u64 = 16 * 1024 * 1024;

/// Git state of one project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    /// Project root the state was probed from.
    pub root: PathBuf,
    /// Private git directory of this worktree.
    pub git_dir: PathBuf,
    /// Shared git directory (differs for linked worktrees).
    pub common_dir: PathBuf,
    /// True for a linked worktree (private git dir below the common one).
    pub is_worktree: bool,
    /// Branch name when `HEAD` is a well-formed symbolic ref.
    pub branch: Option<String>,
    /// Commit `HEAD` points at, when it can be resolved.
    pub head: Option<String>,
    /// True when `HEAD` holds a raw commit hash.
    pub detached: bool,
}

/// Probes the git state of `canonical_root`; `None` outside a repository.
#[must_use]
pub fn state(canonical_root: &Path) -> Option<GitState> {
    let (git_dir, common_dir) = locate(canonical_root, &canonical_root.join(".git"))?;
    let head = read_first_line(&git_dir.join("HEAD"))?;
    let (branch, detached) = parse_head(&head);
    let resolved = match &branch {
        Some(name) => resolve_ref(&common_dir, name),
        None if detached => valid_hash(&head).map(str::to_owned),
        None => None,
    };
    Some(GitState {
        root: canonical_root.to_path_buf(),
        is_worktree: git_dir != common_dir,
        git_dir,
        common_dir,
        branch,
        head: resolved,
        detached,
    })
}

/// Resolves the private git dir and its common dir from `.git`.
fn locate(root: &Path, dot_git: &Path) -> Option<(PathBuf, PathBuf)> {
    let meta = std::fs::metadata(dot_git).ok()?;
    if meta.is_dir() {
        return Some((dot_git.to_path_buf(), dot_git.to_path_buf()));
    }
    if !meta.is_file() {
        return None;
    }
    let line = read_first_line(dot_git)?;
    let target = line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let git_dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    let git_dir = normalize_lexical(&git_dir);
    let common_dir = match read_first_line(&git_dir.join("commondir")) {
        Some(line) => {
            let path = Path::new(line.trim());
            let joined = if path.is_absolute() {
                path.to_path_buf()
            } else {
                git_dir.join(path)
            };
            normalize_lexical(&joined)
        }
        None => git_dir.clone(),
    };
    Some((git_dir, common_dir))
}

/// Resolves `.`/`..` without touching the filesystem (path may be dangling).
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn parse_head(line: &str) -> (Option<String>, bool) {
    if let Some(rest) = line.strip_prefix("ref:") {
        let name = rest.trim();
        if let Some(branch) = name.strip_prefix("refs/heads/")
            && valid_branch(branch)
        {
            return (Some(branch.to_owned()), false);
        }
        return (None, false);
    }
    (None, valid_hash(line).is_some())
}

fn resolve_ref(common_dir: &Path, branch: &str) -> Option<String> {
    let loose = common_dir.join("refs").join("heads").join(branch);
    if let Some(line) = read_first_line(&loose)
        && let Some(sha) = valid_hash(&line)
    {
        return Some(sha.to_owned());
    }
    scan_packed_refs(
        &common_dir.join("packed-refs"),
        &format!("refs/heads/{branch}"),
    )
}

fn scan_packed_refs(path: &Path, wanted: &str) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(PACKED_REFS_LIMIT));
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut fields = line.split(' ');
        let sha = fields.next()?;
        if fields.next().map(str::trim) == Some(wanted) {
            return valid_hash(sha).map(str::to_owned);
        }
    }
}

fn read_first_line(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(META_READ_LIMIT));
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    Some(line.trim_end_matches(['\n', '\r']).to_owned())
}

fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 255
        && !branch.contains("..")
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn valid_hash(value: &str) -> Option<&str> {
    let length = value.len();
    ((length == 40 || length == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, text).expect("write");
    }

    fn sha(ch: char) -> String {
        std::iter::repeat_n(ch, 40).collect()
    }

    #[test]
    fn no_repository_is_none() {
        let dir = TempDir::new().expect("dir");
        assert!(state(dir.path()).is_none());
    }

    #[test]
    fn dir_repo_resolves_branch_and_head() {
        let dir = TempDir::new().expect("dir");
        let git = dir.path().join(".git");
        write(&git.join("HEAD"), "ref: refs/heads/main\n");
        write(&git.join("refs/heads/main"), &format!("{}\n", sha('a')));
        let state = state(dir.path()).expect("state");
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert_eq!(state.head.as_deref(), Some(sha('a').as_str()));
        assert!(!state.detached);
        assert!(!state.is_worktree);
        assert_eq!(state.git_dir, git);
        assert_eq!(state.common_dir, git);
    }

    #[test]
    fn detached_head_reports_hash() {
        let dir = TempDir::new().expect("dir");
        write(&dir.path().join(".git/HEAD"), &format!("{}\n", sha('b')));
        let state = state(dir.path()).expect("state");
        assert!(state.detached);
        assert!(state.branch.is_none());
        assert_eq!(state.head.as_deref(), Some(sha('b').as_str()));
    }

    #[test]
    fn packed_refs_resolve_when_loose_is_absent() {
        let dir = TempDir::new().expect("dir");
        let git = dir.path().join(".git");
        write(&git.join("HEAD"), "ref: refs/heads/release\n");
        write(
            &git.join("packed-refs"),
            &format!(
                "# pack-refs with: peeled fully-peeled sorted\n{} refs/heads/main\n{} refs/heads/release\n^{}\n",
                sha('c'),
                sha('d'),
                sha('d')
            ),
        );
        let state = state(dir.path()).expect("state");
        assert_eq!(state.branch.as_deref(), Some("release"));
        assert_eq!(state.head.as_deref(), Some(sha('d').as_str()));
    }

    #[test]
    fn unborn_branch_has_no_head() {
        let dir = TempDir::new().expect("dir");
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        let state = state(dir.path()).expect("state");
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert!(state.head.is_none());
    }

    #[test]
    fn gitlink_worktree_reads_common_dir() {
        let main = TempDir::new().expect("main");
        let wt = TempDir::new().expect("wt");
        let common = main.path().join(".git");
        let worktree_git = common.join("worktrees/wt");
        write(&worktree_git.join("HEAD"), "ref: refs/heads/feature\n");
        write(&worktree_git.join("commondir"), "../..\n");
        write(
            &common.join("refs/heads/feature"),
            &format!("{}\n", sha('e')),
        );
        std::fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .expect("gitlink");

        let state = state(wt.path()).expect("state");
        assert!(state.is_worktree);
        assert_eq!(state.git_dir, worktree_git);
        assert_eq!(state.common_dir, common);
        assert_eq!(state.branch.as_deref(), Some("feature"));
        assert_eq!(state.head.as_deref(), Some(sha('e').as_str()));
    }

    #[test]
    fn absolute_commondir_is_supported() {
        let main = TempDir::new().expect("main");
        let wt = TempDir::new().expect("wt");
        let common = main.path().join(".git");
        let worktree_git = common.join("worktrees/wt");
        write(&worktree_git.join("HEAD"), "ref: refs/heads/main\n");
        write(
            &worktree_git.join("commondir"),
            &format!("{}\n", common.display()),
        );
        write(&common.join("refs/heads/main"), &format!("{}\n", sha('f')));
        std::fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .expect("gitlink");
        let state = state(wt.path()).expect("state");
        assert!(state.is_worktree);
        assert_eq!(state.common_dir, common);
        assert_eq!(state.head.as_deref(), Some(sha('f').as_str()));
    }

    #[test]
    fn garbage_git_file_is_none() {
        let dir = TempDir::new().expect("dir");
        write(&dir.path().join(".git"), "not a gitdir line\n");
        assert!(state(dir.path()).is_none());
    }

    #[test]
    fn dangling_gitlink_is_none() {
        let dir = TempDir::new().expect("dir");
        write(&dir.path().join(".git"), "gitdir: /nonexistent/worktree\n");
        assert!(state(dir.path()).is_none());
    }

    #[test]
    fn branch_name_validation_rejects_traversal() {
        assert!(valid_branch("feature/x-1.2_3"));
        assert!(!valid_branch(""));
        assert!(!valid_branch(".."));
        assert!(!valid_branch("../escape"));
        assert!(!valid_branch("a b"));
    }
}
