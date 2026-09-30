//! `status` exposes git state of registered projects (T49).

use serde_json::json;
use tempfile::TempDir;

use docsbase_memory::daemon::session::Stats;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::store::Db;

fn write(path: &std::path::Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

#[test]
fn status_reports_branch_and_head() {
    let cache = TempDir::new().expect("cache");
    let project = TempDir::new().expect("project");
    let root = project.path();
    let head = "a".repeat(40);
    write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(&root.join(".git/refs/heads/main"), &format!("{head}\n"));

    let mut db = Db::open(cache.path()).expect("db");
    let project = registry::ensure_project(&mut db, root).expect("project");
    let stats = Stats {
        sessions: 0,
        fd_count: 0,
        threads: 0,
    };
    let value = tools::status(&db, &stats, 0, None, &|_| false).expect("status");
    let entry = &value["projects"][0];
    assert_eq!(entry["id"], json!(project.id));
    assert_eq!(entry["name"], json!(project.name));
    assert_eq!(entry["git"]["branch"], json!("main"));
    assert_eq!(entry["git"]["head"], json!(head));
    assert_eq!(entry["git"]["detached"], json!(false));
    assert_eq!(entry["git"]["is_worktree"], json!(false));
}

#[test]
fn status_omits_git_outside_repository() {
    let cache = TempDir::new().expect("cache");
    let project = TempDir::new().expect("project");
    let mut db = Db::open(cache.path()).expect("db");
    registry::ensure_project(&mut db, project.path()).expect("project");
    let stats = Stats {
        sessions: 0,
        fd_count: 0,
        threads: 0,
    };
    let value = tools::status(&db, &stats, 0, None, &|_| false).expect("status");
    assert!(value["projects"][0].get("git").is_none());
}
