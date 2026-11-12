//! Actual persisted registry observation, without operational Db reader effects.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use docsbase_memory::daemon::registry::ensure_project;
use docsbase_memory::store::registry_inspect::{
    InspectionLimits, RegistryObservation, RegistryState, UnavailableReason, inspect_registry,
};
use docsbase_memory::store::{DB_FILE, Db, migrations, settings};

fn limits() -> InspectionLimits {
    InspectionLimits {
        max_projects: 8,
        max_metadata_bytes: 64 * 1024,
        max_sqlite_value_bytes: 1024 * 1024,
        sqlite_cache_kib: 64,
    }
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    if root.exists() {
        result.insert(root.to_owned(), None);
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(snapshot(&path));
            } else {
                result.insert(path.clone(), Some(std::fs::read(path).unwrap()));
            }
        }
    }
    result
}
fn observe(cache: &Path, limits: InspectionLimits) -> RegistryObservation {
    let before = snapshot(cache);
    let report = inspect_registry(cache, &settings::standalone().unwrap(), limits).unwrap();
    assert_eq!(
        snapshot(cache),
        before,
        "observer must not write, including on error"
    );
    assert_eq!(report.cache_root, cache);
    assert_eq!(report.observation_version, 1);
    report
}
fn roots(report: &RegistryObservation) -> Vec<PathBuf> {
    match &report.state {
        RegistryState::Ready {
            schema_version,
            projects,
            revision,
        } => {
            assert_eq!(*schema_version, migrations::SCHEMA_VERSION);
            assert!(revision.starts_with("registry-v1:"));
            projects.iter().map(|p| p.root.clone()).collect()
        }
        other => panic!("expected captured registry, got {other:?}"),
    }
}

#[test]
fn missing_cache_is_missing_without_creating_directories() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("missing");
    assert_eq!(observe(&cache, limits()).state, RegistryState::Missing);
    assert!(!cache.exists());
}

#[test]
fn persisted_registry_contains_unindexed_and_missing_roots() {
    let cache = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    let a = ensure_project(&mut db, first.path()).unwrap();
    let b = ensure_project(&mut db, second.path()).unwrap();
    drop(db);
    drop(second);
    let report = observe(cache.path(), limits());
    assert_eq!(roots(&report), vec![a.canonical_root, b.canonical_root]);
    assert_eq!(observe(cache.path(), limits()), report);
}

#[test]
fn empty_existing_registry_is_distinct_from_missing() {
    let cache = tempfile::tempdir().unwrap();
    drop(Db::open(cache.path()).unwrap());
    assert_eq!(
        roots(&observe(cache.path(), limits())),
        [] as [std::path::PathBuf; 0]
    );
}

#[test]
fn active_writer_and_recovery_sidecars_never_become_main_only_snapshots() {
    let cache = tempfile::tempdir().unwrap();
    let db = Db::open(cache.path()).unwrap();
    assert!(matches!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(_)
    ));
    drop(db);
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = cache.path().join(format!("{DB_FILE}{suffix}"));
        std::fs::write(&sidecar, b"unresolved recovery").unwrap();
        assert!(matches!(
            observe(cache.path(), limits()).state,
            RegistryState::Unavailable(_)
        ));
        std::fs::remove_file(sidecar).unwrap();
    }
    std::fs::remove_file(cache.path().join(DB_FILE)).unwrap();
    std::fs::write(cache.path().join(format!("{DB_FILE}-wal")), b"orphaned WAL").unwrap();
    assert!(matches!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(_)
    ));
}

#[test]
fn schema_mismatch_never_writes_a_conflict_log_or_attempts_upgrade() {
    let cache = tempfile::tempdir().unwrap();
    let db = Db::open(cache.path()).unwrap();
    db.connection()
        .pragma_update(None, "user_version", 999)
        .unwrap();
    drop(db);
    assert_eq!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(UnavailableReason::UnsupportedSchema)
    );
}

#[test]
fn count_and_retained_metadata_bounds_are_exact_and_never_partial_ready() {
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    let registered = ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    let mut cap = limits();
    cap.max_projects = 1;
    cap.max_metadata_bytes = cache.path().to_str().unwrap().len()
        + 4
        + 8
        + registered.canonical_root.to_str().unwrap().len();
    assert_eq!(
        roots(&observe(cache.path(), cap)),
        vec![registered.canonical_root]
    );
    cap.max_metadata_bytes -= 1;
    assert_eq!(
        observe(cache.path(), cap).state,
        RegistryState::Unavailable(UnavailableReason::LimitExceeded)
    );
    cap = limits();
    cap.max_projects = 0;
    assert_eq!(
        observe(cache.path(), cap).state,
        RegistryState::Unavailable(UnavailableReason::LimitExceeded)
    );
    cap = limits();
    cap.max_sqlite_value_bytes = 8;
    assert_eq!(
        observe(cache.path(), cap).state,
        RegistryState::Unavailable(UnavailableReason::LimitExceeded)
    );
}

#[test]
fn malformed_rows_and_invalid_limits_are_explicit() {
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    db.connection()
        .execute("UPDATE projects SET canonical_root='relative/path'", [])
        .unwrap();
    drop(db);
    assert_eq!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(UnavailableReason::InvalidRegistry)
    );
    let before = snapshot(cache.path());
    let mut cap = limits();
    cap.sqlite_cache_kib = 0;
    assert!(inspect_registry(cache.path(), &settings::standalone().unwrap(), cap).is_err());
    assert_eq!(snapshot(cache.path()), before);
}

#[test]
fn uri_metacharacters_do_not_change_the_opened_database_or_create_files() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache # % Unicode-ёж");
    std::fs::create_dir(&cache).unwrap();
    drop(Db::open(&cache).unwrap());
    assert_eq!(
        roots(&observe(&cache, limits())),
        [] as [std::path::PathBuf; 0]
    );
    #[cfg(unix)]
    {
        let question = root.path().join("cache?mode=rw");
        std::fs::create_dir(&question).unwrap();
        drop(Db::open(&question).unwrap());
        assert_eq!(
            roots(&observe(&question, limits())),
            [] as [std::path::PathBuf; 0]
        );
    }
}

#[test]
fn registry_revision_binds_membership_and_selected_cache_context() {
    let cache = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    drop(Db::open(cache.path()).unwrap());
    drop(Db::open(other.path()).unwrap());
    let initial = observe(cache.path(), limits());
    assert_ne!(initial.state, observe(other.path(), limits()).state);
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    assert_ne!(initial.state, observe(cache.path(), limits()).state);
}

#[cfg(unix)]
#[test]
fn database_symlink_is_unavailable_without_touching_its_target() {
    let real = tempfile::tempdir().unwrap();
    drop(Db::open(real.path()).unwrap());
    let cache = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(real.path().join(DB_FILE), cache.path().join(DB_FILE)).unwrap();
    let before = snapshot(real.path());
    assert!(matches!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(_)
    ));
    assert_eq!(snapshot(real.path()), before);
}

#[test]
fn registry_writer_child() {
    use std::io::{BufRead, Write};
    let Some(cache) = std::env::var_os("DOCSBASE_TEST_REGISTRY_WRITER") else {
        return;
    };
    let writer = rusqlite::Connection::open(PathBuf::from(cache).join(DB_FILE)).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE projects SET name='committed-in-wal';").unwrap();
    println!("registry-writer-ready");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
}

#[test]
fn separate_process_writer_is_unavailable_without_touching_committed_wal() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry_writer_child", "--nocapture"])
            .env("DOCSBASE_TEST_REGISTRY_WRITER", cache.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut ready = false;
        for line in BufReader::new(output).lines() {
            if line.unwrap().trim() == "registry-writer-ready" {
                send.send(()).unwrap();
                ready = true;
            }
        }
        assert!(ready, "writer exited before readiness");
    });
    // Watchdog only; readiness comes from the child, never from elapsed time.
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    assert!(matches!(
        observe(cache.path(), limits()).state,
        RegistryState::Unavailable(_)
    ));
    child.0.stdin.take().unwrap().write_all(b"close\n").unwrap();
    assert!(child.0.wait().unwrap().success());
    reader.join().unwrap();
    assert_eq!(roots(&observe(cache.path(), limits())).len(), 1);
}

#[cfg(unix)]
#[test]
fn repeated_leading_separator_is_a_local_path_not_a_uri_authority() {
    let cache = tempfile::tempdir().unwrap();
    drop(Db::open(cache.path()).unwrap());
    let alias = PathBuf::from(format!("/{}", cache.path().display()));
    assert_eq!(alias, cache.path());
    assert_eq!(
        roots(&observe(&alias, limits())),
        [] as [std::path::PathBuf; 0]
    );
}

#[cfg(unix)]
#[test]
fn symlink_cache_ancestor_preserves_selected_context() {
    let parent = tempfile::tempdir().unwrap();
    let cache = parent.path().join("real-cache");
    drop(Db::open(&cache).unwrap());
    let alias = parent.path().join("cache-alias");
    std::os::unix::fs::symlink(&cache, &alias).unwrap();
    let report = observe(&alias, limits());
    assert_eq!(roots(&report), [] as [std::path::PathBuf; 0]);
    assert_ne!(report.state, observe(&cache, limits()).state);
}

#[test]
fn oversized_persisted_root_exceeds_sqlite_value_limit() {
    let cache = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut db = Db::open(cache.path()).unwrap();
    ensure_project(&mut db, project.path()).unwrap();
    drop(db);
    let mut cap = limits();
    cap.max_sqlite_value_bytes = 4096;
    assert_eq!(roots(&observe(cache.path(), cap)).len(), 1);
    let db = Db::open(cache.path()).unwrap();
    let root = project.path().join("x".repeat(8192));
    db.connection()
        .execute(
            "UPDATE projects SET canonical_root=?1",
            [root.to_str().unwrap()],
        )
        .unwrap();
    drop(db);
    assert_eq!(
        observe(cache.path(), cap).state,
        RegistryState::Unavailable(UnavailableReason::LimitExceeded)
    );
    assert_eq!(roots(&observe(cache.path(), limits())), vec![root]);
}
