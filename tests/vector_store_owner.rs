//! Real store policy, lifetime exclusion and transactional eviction contracts.
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::Resolution;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::error::{Error, Result};
use docsbase_memory::vector_cache::settings::{
    VectorStorePatch, VectorStoreSettings, VectorStoreSpec, standalone,
};
use docsbase_memory::vector_cache::{
    CacheLimits, Embedder, VECTOR_DB_FILE, VectorCache, VectorStore,
};

fn policy(max_entries: u64, max_bytes: u64) -> VectorStoreSettings {
    VectorStoreSettings {
        max_entries,
        max_bytes,
        busy_timeout: Duration::ZERO,
    }
}

fn sql(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join(VECTOR_DB_FILE)).unwrap()
}

#[test]
fn invalid_strict_policy_never_creates_the_cache_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("absent");
    for settings in [
        policy(0, 16),
        policy(2, 0),
        VectorStoreSettings {
            busy_timeout: Duration::from_millis(i32::MAX as u64 + 1),
            ..policy(2, 16)
        },
    ] {
        assert!(matches!(
            VectorStore::acquire(&root, &settings),
            Err(Error::Admission { .. })
        ));
        assert!(!root.exists());
    }
}

#[test]
fn strict_patch_scope_origins_and_rejection_preserve_last_valid_policy() {
    let mut resolution = Resolution::<VectorStoreSpec>::new(&DefaultsContext::standalone());
    let global = SourceId::Global("global.toml".into());
    resolution
        .apply(
            &global,
            VectorStorePatch {
                max_entries: Some(3),
                ..VectorStorePatch::default()
            },
        )
        .unwrap();
    resolution
        .apply(
            &SourceId::Cli,
            VectorStorePatch {
                max_bytes: Some(24),
                ..VectorStorePatch::default()
            },
        )
        .unwrap();
    assert!(
        resolution
            .apply(
                &SourceId::Cli,
                VectorStorePatch {
                    max_entries: Some(0),
                    max_bytes: Some(99),
                    ..VectorStorePatch::default()
                }
            )
            .is_err()
    );
    assert!(
        resolution
            .apply(
                &SourceId::Project("project.toml".into()),
                VectorStorePatch {
                    max_bytes: Some(12),
                    ..VectorStorePatch::default()
                }
            )
            .is_err()
    );
    let resolved = resolution.finish().unwrap();
    assert_eq!(resolved.settings().max_entries, 3);
    assert_eq!(resolved.settings().max_bytes, 24);
    assert_eq!(
        resolved
            .provenance()
            .get("vector_cache.max_entries")
            .unwrap()
            .source,
        global
    );
    assert_eq!(
        resolved
            .provenance()
            .get("vector_cache.max_bytes")
            .unwrap()
            .source,
        SourceId::Cli
    );
    assert_eq!(
        resolved
            .provenance()
            .get("vector_cache.busy_timeout")
            .unwrap()
            .source,
        SourceId::Defaults
    );
}

#[test]
fn every_legacy_connection_blocks_owner_acquisition_until_closed() {
    let root = tempfile::tempdir().unwrap();
    let first = VectorCache::open(root.path()).unwrap();
    let second = VectorCache::open_with_limits(
        root.path(),
        CacheLimits {
            max_entries: 0,
            max_bytes: 0,
        },
    )
    .unwrap();
    assert!(matches!(
        VectorStore::acquire(root.path(), &policy(2, 16)),
        Err(Error::Admission { .. })
    ));
    drop(first);
    assert!(matches!(
        VectorStore::acquire(root.path(), &policy(2, 16)),
        Err(Error::Admission { .. })
    ));
    drop(second);
    let owner = VectorStore::acquire(root.path(), &policy(2, 16)).unwrap();
    assert_eq!(owner.connect().unwrap().len().unwrap(), 0);
}

#[test]
fn owned_connections_and_clones_retain_exclusion_after_original_owner_drops() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(2, 16)).unwrap();
    assert_eq!(owner.root(), std::fs::canonicalize(root.path()).unwrap());
    let clone = owner.clone();
    let cache = owner.connect().unwrap();
    drop(owner);
    assert!(matches!(
        VectorCache::open(root.path()),
        Err(Error::Admission { .. })
    ));
    drop(clone);
    assert!(matches!(
        VectorStore::acquire(root.path(), &policy(2, 16)),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        VectorCache::open_with_limits(
            root.path(),
            CacheLimits {
                max_entries: 0,
                max_bytes: 0
            }
        ),
        Err(Error::Admission { .. })
    ));
    drop(cache);
    let mut legacy = VectorCache::open_with_limits(
        root.path(),
        CacheLimits {
            max_entries: 0,
            max_bytes: 0,
        },
    )
    .unwrap();
    legacy.put("model", "key", &[1.0]).unwrap();
    assert!(
        legacy.is_empty().unwrap(),
        "legacy zero-cap semantics remain supported"
    );
}

#[test]
fn different_roots_are_independent_and_path_aliases_share_exclusion() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(first.path(), &policy(2, 16)).unwrap();
    assert!(matches!(
        VectorCache::open(&first.path().join(".")),
        Err(Error::Admission { .. })
    ));
    let other = VectorStore::acquire(second.path(), &policy(1, 4)).unwrap();
    owner.connect().unwrap().put("m", "a", &[1.0]).unwrap();
    other.connect().unwrap().put("m", "b", &[2.0]).unwrap();
    assert_eq!(owner.connect().unwrap().get("m", "b").unwrap(), None);
}

#[test]
fn failed_sqlite_initialization_does_not_leak_the_mode_lease() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(VECTOR_DB_FILE)).unwrap();
    assert!(matches!(
        VectorCache::open(root.path()),
        Err(Error::Internal { .. })
    ));
    assert!(matches!(
        VectorStore::acquire(root.path(), &policy(2, 16)),
        Err(Error::Internal { .. })
    ));
    std::fs::remove_dir(root.path().join(VECTOR_DB_FILE)).unwrap();
    let legacy = VectorCache::open(root.path()).unwrap();
    assert!(legacy.is_empty().unwrap());
    drop(legacy);
    assert!(VectorStore::acquire(root.path(), &policy(2, 16)).is_ok());
}

#[test]
fn all_owned_connections_share_entry_limits_and_one_lru_clock() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(2, 1024)).unwrap();
    let mut idle = owner.connect().unwrap();
    let mut active = owner.connect().unwrap();
    active.put("m", "recent", &[1.0]).unwrap();
    active.put("m", "old", &[2.0]).unwrap();
    assert_eq!(idle.get("m", "recent").unwrap(), Some(vec![1.0]));
    active.put("m", "new", &[3.0]).unwrap();
    assert_eq!(idle.get("m", "old").unwrap(), None);
    assert_eq!(idle.get("m", "recent").unwrap(), Some(vec![1.0]));
    assert_eq!(idle.len().unwrap(), 2);
}

#[test]
fn supplied_byte_cap_and_timeout_reach_real_connections() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(100, 8)).unwrap();
    let mut first = owner.connect().unwrap();
    let mut second = owner.connect().unwrap();
    first.put("m", "old", &[1.0, 2.0]).unwrap();
    second.put("m", "new", &[3.0, 4.0]).unwrap();
    assert_eq!(first.get("m", "old").unwrap(), None);
    let writer = sql(root.path());
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = second.put("m", "blocked", &[5.0]);
    let error = result.unwrap_err();
    assert!(std::error::Error::source(&error).unwrap().downcast_ref::<rusqlite::Error>().is_some_and(|e| matches!(e, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy)));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(second.get("m", "new").unwrap(), Some(vec![3.0, 4.0]));
    assert_eq!(owner.settings().busy_timeout, Duration::ZERO);
}

struct Counted(AtomicUsize);
impl Embedder for Counted {
    fn model_id(&self) -> &'static str {
        "shared-model"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.fetch_add(texts.len(), Ordering::Relaxed);
        Ok(texts.iter().map(|_| vec![1.0, 2.0]).collect())
    }
}

#[test]
fn project_handles_reuse_vectors_and_preserve_other_owners_leases() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(1, 8)).unwrap();
    let mut project_a = owner.connect().unwrap();
    let mut project_b = owner.connect().unwrap();
    let embedder = Counted(AtomicUsize::new(0));
    let texts = vec!["same chunk".to_owned()];
    let hash = docsbase_memory::vector_cache::chunk_hash(&texts[0]);
    project_a
        .pin(
            1,
            "shared-model",
            std::slice::from_ref(&hash),
            Duration::from_secs(600),
        )
        .unwrap();
    project_b
        .pin(
            2,
            "shared-model",
            std::slice::from_ref(&hash),
            Duration::from_secs(600),
        )
        .unwrap();
    project_a.embed_missing(&embedder, &texts).unwrap();
    project_b.embed_missing(&embedder, &texts).unwrap();
    assert_eq!(embedder.0.load(Ordering::Relaxed), 1);
    project_b.put("other-model", "new", &[3.0, 4.0]).unwrap();
    assert_eq!(project_b.get("other-model", "new").unwrap(), None);
    project_a
        .unpin(1, "shared-model", std::slice::from_ref(&hash))
        .unwrap();
    project_b.put("other-model", "again", &[5.0, 6.0]).unwrap();
    assert!(project_b.get("shared-model", &hash).unwrap().is_some());
    assert_eq!(project_b.get("other-model", "again").unwrap(), None);
    project_b.unpin(2, "shared-model", &[hash]).unwrap();
    project_b
        .put("other-model", "after-release", &[7.0, 8.0])
        .unwrap();
    assert_eq!(
        project_b.get("other-model", "after-release").unwrap(),
        Some(vec![7.0, 8.0])
    );
}

fn reject_eviction(root: &Path) {
    sql(root).execute_batch("CREATE TRIGGER reject_eviction BEFORE DELETE ON vector_cache BEGIN SELECT RAISE(ABORT, 'fixture eviction failure'); END;").unwrap();
}

#[test]
fn failed_strict_eviction_rolls_back_the_vector_write() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(1, 16)).unwrap();
    let mut cache = owner.connect().unwrap();
    cache.put("m", "old", &[1.0]).unwrap();
    reject_eviction(root.path());
    assert!(cache.put("m", "new", &[2.0]).is_err());
    assert_eq!(cache.len().unwrap(), 1);
    assert_eq!(cache.get("m", "new").unwrap(), None);
    assert_eq!(cache.get("m", "old").unwrap(), Some(vec![1.0]));
}

#[test]
fn failed_strict_unpin_eviction_rolls_back_lease_release() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(1, 16)).unwrap();
    let mut cache = owner.connect().unwrap();
    let hashes = vec!["first".to_owned(), "second".to_owned()];
    cache
        .pin(7, "m", &hashes, Duration::from_secs(600))
        .unwrap();
    cache.put("m", "first", &[1.0]).unwrap();
    cache.put("m", "second", &[2.0]).unwrap();
    reject_eviction(root.path());
    assert!(cache.unpin(7, "m", &hashes).is_err());
    let count: i64 = sql(root.path())
        .query_row(
            "SELECT COUNT(*) FROM vector_cache_leases WHERE lease_id=7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(cache.len().unwrap(), 2);
}

fn child_probe(root: &Path, mode: &str) {
    let output = assert_cmd::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "mode_exclusion_child", "--nocapture"])
        .env("DOCSBASE_TEST_VECTOR_ROOT", root)
        .env("DOCSBASE_TEST_VECTOR_MODE", mode)
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("mode probe completed")
    );
}

#[test]
fn mode_exclusion_child() {
    let Some(root) = std::env::var_os("DOCSBASE_TEST_VECTOR_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    match std::env::var("DOCSBASE_TEST_VECTOR_MODE").unwrap().as_str() {
        "legacy-blocked" => assert!(matches!(
            VectorCache::open(root),
            Err(Error::Admission { .. })
        )),
        "owner-blocked" => assert!(matches!(
            VectorStore::acquire(root, &policy(2, 16)),
            Err(Error::Admission { .. })
        )),
        "owner-available" => {
            VectorStore::acquire(root, &policy(2, 16)).unwrap();
        }
        other => panic!("unknown mode {other}"),
    }
    println!("mode probe completed");
}

#[test]
fn mode_exclusion_and_failed_probes_preserve_locks_across_processes() {
    let root = tempfile::tempdir().unwrap();
    let first = VectorCache::open(root.path()).unwrap();
    let second = VectorCache::open(root.path()).unwrap();
    child_probe(root.path(), "owner-blocked");
    drop(first);
    child_probe(root.path(), "owner-blocked");
    drop(second);
    child_probe(root.path(), "owner-available");
    let owner = VectorStore::acquire(root.path(), &policy(2, 16)).unwrap();
    child_probe(root.path(), "legacy-blocked");
    child_probe(root.path(), "owner-blocked");
    child_probe(root.path(), "legacy-blocked");
    drop(owner);
    child_probe(root.path(), "owner-available");
}

#[test]
fn defaults_reopen_existing_vectors_without_implicit_eviction() {
    let root = tempfile::tempdir().unwrap();
    let mut legacy = VectorCache::open(root.path()).unwrap();
    legacy.put("m", "saved", &[1.0, 2.0]).unwrap();
    drop(legacy);
    let owner = VectorStore::acquire(root.path(), standalone().unwrap().settings()).unwrap();
    let mut cache = owner.connect().unwrap();
    assert_eq!(cache.get("m", "saved").unwrap(), Some(vec![1.0, 2.0]));
}

#[test]
fn concurrent_owned_writers_share_one_budget_and_ordered_recency() {
    let root = tempfile::tempdir().unwrap();
    let settings = VectorStoreSettings {
        busy_timeout: Duration::from_secs(5),
        ..policy(8, 32)
    };
    let owner = VectorStore::acquire(root.path(), &settings).unwrap();
    let connections: Vec<_> = (0..4).map(|_| owner.connect().unwrap()).collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(connections.len()));
    let workers: Vec<_> = connections
        .into_iter()
        .enumerate()
        .map(|(worker, mut cache)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for index in 0..16 {
                    cache
                        .put("m", &format!("{worker}-{index}"), &[1.0])
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let summary: (i64, i64, i64, i64) = sql(root.path())
        .query_row(
            "SELECT COUNT(*), SUM(LENGTH(vector)), MIN(used_at), MAX(used_at) FROM vector_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(summary, (8, 32, 57, 64));
}

#[test]
fn reacquiring_an_owner_resumes_the_persisted_recency_clock() {
    let root = tempfile::tempdir().unwrap();
    let owner = VectorStore::acquire(root.path(), &policy(2, 32)).unwrap();
    let mut cache = owner.connect().unwrap();
    cache.put("m", "first", &[1.0]).unwrap();
    cache.put("m", "recent", &[2.0]).unwrap();
    // Simulate a store whose persisted sequence has outlived this process.
    sql(root.path())
        .execute(
            "UPDATE vector_cache SET used_at=1000 WHERE chunk_hash='recent'",
            [],
        )
        .unwrap();
    drop(cache);
    drop(owner);
    let owner = VectorStore::acquire(root.path(), &policy(2, 32)).unwrap();
    owner.connect().unwrap().put("m", "new", &[3.0]).unwrap();
    let mut cache = owner.connect().unwrap();
    assert_eq!(cache.get("m", "first").unwrap(), None);
    assert_eq!(cache.get("m", "new").unwrap(), Some(vec![3.0]));
    let latest: i64 = sql(root.path())
        .query_row("SELECT MAX(used_at) FROM vector_cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(latest, 1002);
}

#[cfg(unix)]
#[test]
fn symlinked_cache_roots_resolve_to_the_same_owner_namespace() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("cache");
    std::fs::create_dir(&root).unwrap();
    let alias = parent.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let owner = VectorStore::acquire(&alias, &policy(2, 16)).unwrap();
    assert_eq!(owner.root(), std::fs::canonicalize(&root).unwrap());
    assert!(matches!(
        VectorCache::open(&root),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        VectorStore::acquire(&root, &policy(3, 24)),
        Err(Error::Admission { .. })
    ));
}

#[test]
fn acquiring_a_smaller_policy_does_not_evict_existing_legacy_rows() {
    let root = tempfile::tempdir().unwrap();
    let mut legacy = VectorCache::open(root.path()).unwrap();
    for key in ["first", "second", "third"] {
        legacy.put("m", key, &[1.0]).unwrap();
    }
    drop(legacy);
    let owner = VectorStore::acquire(root.path(), &policy(1, 4)).unwrap();
    let mut first = owner.connect().unwrap();
    let second = owner.connect().unwrap();
    assert_eq!(first.len().unwrap(), 3);
    assert_eq!(second.len().unwrap(), 3);
    first.put("m", "new", &[2.0]).unwrap();
    assert_eq!(second.len().unwrap(), 1);
    assert_eq!(first.get("m", "new").unwrap(), Some(vec![2.0]));
}
