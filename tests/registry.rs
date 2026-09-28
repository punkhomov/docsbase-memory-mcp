use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::daemon::registry::{
    ensure_project, list_projects, resolve_by_cwd, set_status,
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

    let err = set_status(&env.db, 999, ProjectStatus::Error).expect_err("unknown");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
}
