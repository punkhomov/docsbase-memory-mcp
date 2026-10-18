//! Commit-failure recovery (the CI flake family, tantivy #2847): a commit
//! that dies mid-way can leave files that make every later attempt fail with
//! `FileAlreadyExists` (tantivy opens files with `create_new`). The index
//! must be marked for recreation and rebuilt from `SQLite` by the next run
//! instead of being poisoned forever.
//!
//! Single test binary on purpose: the fault seam is process-env based, and a
//! second test in this file could observe the variables mid-flight.

use std::fs;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::{run_project_index, search_docs};
use docsbase_memory::index::tantivy_index::REBUILD_MARKER;
use docsbase_memory::store::Db;
use docsbase_memory::store::models::ProjectStatus;
use tempfile::TempDir;

#[test]
#[allow(unsafe_code)]
fn a_failed_commit_marks_the_index_for_rebuild_and_the_next_run_heals() {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    fs::write(root.path().join("a.md"), "# A\n\nalpha widgets\n").expect("write doc");
    let mut db = Db::open(cache.path()).expect("db");
    let canonical = root.path().canonicalize().expect("canonical");
    let project = registry::ensure_project(&mut db, &canonical).expect("project");
    let index_dir = cache
        .path()
        .join("projects")
        .join(project.id.to_string())
        .join("tantivy");

    // `unsafe` because the env is process-global; this file runs a single
    // test, so nothing else can observe the two variables.
    let root_prefix = cache.path().to_str().expect("utf-8 cache").to_owned();
    unsafe {
        std::env::set_var("DOCSBASE_INDEX_FAULT", "commit:1");
        std::env::set_var("DOCSBASE_INDEX_FAULT_ROOT", &root_prefix);
    }
    let err = run_project_index(&mut db, cache.path(), &project, &Config::default())
        .expect_err("the injected commit fault fails the run");
    unsafe {
        std::env::remove_var("DOCSBASE_INDEX_FAULT");
        std::env::remove_var("DOCSBASE_INDEX_FAULT_ROOT");
    }
    assert!(err.to_string().contains("injected"), "{err}");
    assert!(
        index_dir.join(REBUILD_MARKER).exists(),
        "a failed commit must mark the index for recreation"
    );
    let status = registry::project_by_id(&db, project.id)
        .expect("project")
        .expect("state")
        .status;
    assert_eq!(status, ProjectStatus::Error, "the failed run is visible");

    let healed = run_project_index(&mut db, cache.path(), &project, &Config::default())
        .expect("the next run must heal the index");
    assert!(healed.docs >= 1 && healed.chunks >= 1);
    assert!(
        !index_dir.join(REBUILD_MARKER).exists(),
        "the recovery rebuild clears the marker"
    );
    let project = registry::project_by_id(&db, project.id)
        .expect("project")
        .expect("state");
    assert_eq!(project.status, ProjectStatus::Indexed);

    let hits = search_docs(
        &db,
        cache.path(),
        &project,
        &serde_json::json!({ "query": "alpha" }),
    )
    .expect("search after recovery");
    assert!(
        !hits.as_array().expect("array").is_empty(),
        "the rebuilt index serves the document: {hits}"
    );
}
