use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::daemon::registry::{
    RootState, ensure_project, list_projects, resolve_by_cwd, root_state, set_status,
};
use docsbase_memory::error::Error;
use docsbase_memory::store::Db;
use docsbase_memory::store::models::ProjectStatus;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    root: TempDir,
    db: Db,
}

impl Env {
    fn new() -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        let db = Db::open(cache.path()).expect("db");
        Self { cache, root, db }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let path = self.root.path().join(rel);
        fs::create_dir_all(&path).expect("mkdir");
        path
    }
}

fn git_dir(path: &Path) {
    fs::create_dir_all(path.join(".git")).expect("git dir");
}

#[test]
fn register_normalizes_root() {
    let mut env = Env::new();
    let repo = env.dir("repo");
    git_dir(&repo);
    let docs = env.dir("repo/docs");

    let project = ensure_project(&mut env.db, &docs).expect("register");
    assert_eq!(project.canonical_root, repo.canonicalize().expect("canon"));
    assert_eq!(project.name, "repo");
    assert_eq!(project.status, ProjectStatus::NotIndexed);
}

#[test]
fn idempotent() {
    let mut env = Env::new();
    let repo = env.dir("repo");
    git_dir(&repo);

    let first = ensure_project(&mut env.db, &repo).expect("first");
    let second = ensure_project(&mut env.db, &repo).expect("second");
    assert_eq!(first.id, second.id);
    assert_eq!(list_projects(&env.db).expect("list").len(), 1);
}

#[test]
fn rejects_fs_root_and_cache_dir() {
    let mut env = Env::new();
    let root = Path::new("/");
    let err = ensure_project(&mut env.db, root).expect_err("must reject /");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");

    let cache = env.cache.path().to_path_buf();
    let err = ensure_project(&mut env.db, &cache).expect_err("must reject cache");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
}

#[test]
fn rejects_missing_and_file_paths() {
    let mut env = Env::new();
    let missing = env.root.path().join("nope");
    assert!(ensure_project(&mut env.db, &missing).is_err());

    let file = env.root.path().join("a.md");
    fs::write(&file, "x").expect("write");
    assert!(ensure_project(&mut env.db, &file).is_err());
}

#[test]
fn resolve_from_subdir() {
    let mut env = Env::new();
    let repo = env.dir("repo");
    git_dir(&repo);
    let nested = env.dir("repo/vendor/tool");
    git_dir(&nested);
    let deep = env.dir("repo/vendor/tool/src/deep");

    let outer = ensure_project(&mut env.db, &repo).expect("outer");
    let inner = ensure_project(&mut env.db, &nested).expect("inner");

    let resolved = resolve_by_cwd(&env.db, &deep).expect("resolve");
    assert_eq!(resolved.id, inner.id, "nearest ancestor wins");

    let mid = env.dir("repo/src");
    let resolved = resolve_by_cwd(&env.db, &mid).expect("resolve outer");
    assert_eq!(resolved.id, outer.id);
}

#[test]
fn resolve_unknown() {
    let env = Env::new();
    let loose = env.dir("loose/place");

    let err = resolve_by_cwd(&env.db, &loose).expect_err("must fail");
    assert_eq!(err.mcp_code(), -32012);
    match err {
        Error::Project { instruction, .. } => {
            let instruction = instruction.expect("instruction");
            assert!(instruction.contains("index_project"), "{instruction}");
        }
        other => panic!("expected Project error, got {other:?}"),
    }
}

#[test]
fn status_round_trip() {
    let mut env = Env::new();
    let repo = env.dir("repo");
    git_dir(&repo);
    let project = ensure_project(&mut env.db, &repo).expect("register");

    set_status(&env.db, project.id, ProjectStatus::Indexed).expect("set");
    let listed = list_projects(&env.db).expect("list");
    assert_eq!(listed[0].status, ProjectStatus::Indexed);
    assert!(
        listed[0].last_indexed_at.is_some(),
        "Indexed must stamp last_indexed_at"
    );

    let err = set_status(&env.db, 999, ProjectStatus::Error).expect_err("unknown");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
}

#[test]
fn system_dirs_are_refused() {
    let cache = tempfile::TempDir::new().expect("cache");
    #[cfg(unix)]
    let dirs = ["/etc", "/usr", "/boot", "/proc"];
    #[cfg(windows)]
    let dirs = ["C:\\Windows", "C:\\Program Files"];
    for dir in dirs {
        let err = docsbase_memory::daemon::registry::project_root_for(
            std::path::Path::new(dir),
            cache.path(),
        )
        .expect_err("system dir must be refused");
        assert!(
            err.to_string().contains("system directories"),
            "unexpected error for {dir}: {err}"
        );
    }
}

#[test]
fn status_marks_missing_root() {
    let cache = TempDir::new().expect("cache");
    let project = TempDir::new().expect("project");
    let mut db = Db::open(cache.path()).expect("db");
    let registered = ensure_project(&mut db, project.path()).expect("project");
    assert_eq!(root_state(&registered.canonical_root), RootState::Present);

    // A root replaced by a regular file is missing too.
    fs::remove_dir_all(project.path()).expect("remove");
    fs::write(project.path(), b"not a directory").expect("file");
    assert_eq!(root_state(&registered.canonical_root), RootState::Missing);

    let stats = docsbase_memory::daemon::session::Stats {
        sessions: 0,
        fd_count: 0,
        threads: 0,
    };
    let value =
        docsbase_memory::daemon::tools::status(&db, &stats, 0, None, &|_| false).expect("status");
    assert_eq!(value["projects"][0]["root_state"], "missing");
    assert_eq!(
        list_projects(&db).expect("registry").len(),
        1,
        "the registry entry must survive"
    );
}

#[cfg(unix)]
#[test]
fn uncertain_root_is_not_missing() {
    use std::os::unix::fs::PermissionsExt;

    // Root users bypass permissions; the uncertainty path cannot be
    // reproduced there.
    let probe = std::env::temp_dir().join(format!("docsbase-euid-{}", std::process::id()));
    fs::write(&probe, b"x").expect("probe");
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).expect("chmod probe");
    let unreadable = fs::read(&probe).is_err();
    let _ = fs::remove_file(&probe);
    if !unreadable {
        eprintln!("skipping: effective root bypasses permissions");
        return;
    }

    let parent = TempDir::new().expect("parent");
    let root = parent.path().join("project");
    fs::create_dir_all(&root).expect("mkdir");
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o000)).expect("chmod parent");
    assert_eq!(root_state(&root), RootState::Uncertain);
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).expect("restore");
}
