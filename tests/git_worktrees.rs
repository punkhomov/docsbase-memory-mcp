//! Real-git regression suite for worktree roots and ignore layers (T51).

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::index::walk;
use docsbase_memory::store::Db;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git(dir: &Path, args: &[&str]) {
    let empty_global = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty_global)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

fn walked(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = walk::walk(root, &Config::default())
        .expect("walk")
        .map(|entry| entry.expect("entry"))
        .map(|path| {
            path.strip_prefix(root)
                .expect("inside root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    names.sort();
    names
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q"]);
    write(root, "a.md", "# A\n");
    write(root, "secret.md", "# Secret\n");
    write(root, ".git/info/exclude", "secret.md\n");
    git(root, &["add", "a.md"]);
    git(root, &["commit", "-qm", "init"]);
}

#[test]
fn exclude_file_is_honored() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = TempDir::new().expect("repo");
    init_repo(repo.path());
    assert_eq!(walked(repo.path()), vec!["a.md".to_owned()]);
}

#[test]
fn linked_worktree_root_and_common_excludes() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let parent = TempDir::new().expect("parent");
    let main = parent.path().join("main");
    let worktree = parent.path().join("wt");
    std::fs::create_dir_all(&main).expect("mkdir main");
    init_repo(&main);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().expect("utf8"),
        ],
    );
    write(&worktree, "secret.md", "# Secret\n");
    write(&worktree, "anchored.md", "# Anchored\n");
    write(&worktree, "docs/nested.md", "# Nested\n");
    write(
        &main,
        ".git/info/exclude",
        "secret.md\n/anchored.md\ndocs/nested.md\n",
    );

    // `.git/info/exclude` from the common dir applies inside the worktree,
    // including anchored patterns (rooted at the worktree root, not the CWD).
    assert_eq!(walked(&worktree), vec!["a.md".to_owned()]);
    // The watcher's per-directory expansion honors it too.
    let rel = |path: &Path| {
        path.strip_prefix(&worktree)
            .expect("inside root")
            .to_string_lossy()
            .replace('\\', "/")
    };
    let files = walk::indexable_files(&worktree, &worktree, &Config::default())
        .expect("indexable_files")
        .into_iter()
        .map(|path| rel(&path))
        .collect::<Vec<_>>();
    assert_eq!(files, vec!["a.md".to_owned()]);
    // Watcher-style call for a subdirectory: anchored patterns still apply.
    let nested = walk::indexable_files(&worktree.join("docs"), &worktree, &Config::default())
        .expect("indexable_files docs");
    assert!(
        nested.is_empty(),
        "docs/nested.md must stay excluded: {nested:?}"
    );

    let cache = TempDir::new().expect("cache");
    let mut db = Db::open(cache.path()).expect("db");
    let project = registry::ensure_project(&mut db, &worktree).expect("project");
    assert_eq!(
        project.canonical_root,
        worktree.canonicalize().expect("canonical")
    );

    // A subdirectory resolves to the same worktree root and project.
    let subdir = worktree
        .join("a.md")
        .parent()
        .expect("parent")
        .to_path_buf();
    let same = registry::ensure_project(&mut db, &subdir).expect("subdir project");
    assert_eq!(same.id, project.id);
    assert_eq!(registry::list_projects(&db).expect("projects").len(), 1);
}

#[test]
fn main_and_worktree_are_separate_projects() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let parent = TempDir::new().expect("parent");
    let main = parent.path().join("main");
    let worktree = parent.path().join("wt");
    std::fs::create_dir_all(&main).expect("mkdir main");
    init_repo(&main);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().expect("utf8"),
        ],
    );

    let cache = TempDir::new().expect("cache");
    let mut db = Db::open(cache.path()).expect("db");
    let first = registry::ensure_project(&mut db, &main).expect("main");
    let second = registry::ensure_project(&mut db, &worktree).expect("worktree");
    assert_ne!(first.id, second.id);
    assert_ne!(first.canonical_root, second.canonical_root);
    assert_eq!(registry::list_projects(&db).expect("projects").len(), 2);
}

#[test]
fn garbage_git_file_is_a_plain_directory() {
    let dir = TempDir::new().expect("dir");
    write(dir.path(), ".git", "not a gitdir line\n");
    write(dir.path(), "a.md", "# A\n");
    let cache = TempDir::new().expect("cache");
    let mut db = Db::open(cache.path()).expect("db");
    let project = registry::ensure_project(&mut db, dir.path()).expect("project");
    assert_eq!(
        project.canonical_root,
        dir.path().canonicalize().expect("canonical")
    );
    assert_eq!(walked(dir.path()), vec!["a.md".to_owned()]);
}

#[test]
fn detached_worktree_and_unborn_branch_are_indexable() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let parent = TempDir::new().expect("parent");
    let main = parent.path().join("main");
    let detached = parent.path().join("detached");
    std::fs::create_dir_all(&main).expect("mkdir main");
    init_repo(&main);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().expect("utf8"),
            "HEAD",
        ],
    );
    let unborn = parent.path().join("unborn");
    std::fs::create_dir_all(&unborn).expect("mkdir unborn");
    git(&unborn, &["init", "-q"]);
    write(&unborn, "draft.md", "# Draft\n");

    let cache = TempDir::new().expect("cache");
    let mut db = Db::open(cache.path()).expect("db");
    let detached_project = registry::ensure_project(&mut db, &detached).expect("detached");
    assert_eq!(walked(&detached), vec!["a.md".to_owned()]);
    let unborn_project = registry::ensure_project(&mut db, &unborn).expect("unborn");
    assert_eq!(walked(&unborn), vec!["draft.md".to_owned()]);
    assert_ne!(detached_project.id, unborn_project.id);
}
