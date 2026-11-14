use super::*;

#[test]
fn legacy_public_negative_ids_cannot_forge_job_pin_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut cache = VectorCache::open(dir.path()).unwrap();
    let hashes = vec![chunk_hash("owned")];
    for id in [-1, -2, i64::MIN] {
        assert!(
            cache
                .pin(id, "model", &hashes, Duration::from_secs(60))
                .is_err(),
            "negative lease ids require private move-only authority"
        );
        assert!(
            cache.unpin(id, "model", &hashes).is_err(),
            "legacy unpin cannot target a job's negative namespace"
        );
    }
    cache
        .pin(1, "model", &hashes, Duration::from_secs(60))
        .unwrap();
    cache.unpin(1, "model", &hashes).unwrap();
}

fn authority(root: &std::path::Path) -> JobPinAuthority {
    JobPinAuthority::allocate(
        root,
        VectorCacheSource::Legacy(CacheLimits::default()),
        "model".into(),
        Duration::from_secs(60),
        8,
    )
    .unwrap()
}
fn leases(cache: &VectorCache) -> Vec<i64> {
    cache.conn.prepare("SELECT DISTINCT lease_id FROM vector_cache_leases WHERE lease_id<>?1 ORDER BY lease_id").unwrap().query_map([i64::MIN], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}
#[test]
fn allocated_pins_survive_old_positive_cleanup_and_release_only_their_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = authority(dir.path());
    let mut second = authority(dir.path());
    let hash = chunk_hash("same content");
    first.track(&hash).unwrap();
    second.track(&hash).unwrap();
    first.renew().unwrap();
    second.renew().unwrap();
    let mut legacy = VectorCache::open(dir.path()).unwrap();
    legacy
        .pin(
            1,
            "model",
            std::slice::from_ref(&hash),
            Duration::from_secs(60),
        )
        .unwrap();
    legacy
        .unpin(1, "model", std::slice::from_ref(&hash))
        .unwrap();
    assert_eq!(leases(&legacy), vec![-2, -1]);
    drop(first);
    assert_eq!(leases(&legacy), vec![-2]);
    drop(second);
    assert_eq!(leases(&legacy), Vec::<i64>::new());
    let mut third = authority(dir.path());
    third.track(&hash).unwrap();
    third.renew().unwrap();
    assert_eq!(leases(&legacy), vec![-3], "released ids never reset/reuse");
}
#[test]
fn missing_corrupt_and_exhausted_pin_counter_fail_closed() {
    for corrupt in [
        "DELETE FROM vector_cache_leases WHERE lease_id=-9223372036854775808",
        "UPDATE vector_cache_leases SET chunk_hash='bad' WHERE lease_id=-9223372036854775808",
        "UPDATE vector_cache_leases SET chunk_hash='-9223372036854775807' WHERE lease_id=-9223372036854775808",
    ] {
        let dir = tempfile::tempdir().unwrap();
        drop(authority(dir.path()));
        let cache = VectorCache::open(dir.path()).unwrap();
        cache.conn.execute_batch(corrupt).unwrap();
        assert!(
            JobPinAuthority::allocate(
                dir.path(),
                VectorCacheSource::Legacy(CacheLimits::default()),
                "model".into(),
                Duration::from_secs(60),
                8
            )
            .is_err()
        );
    }
}
#[test]
fn parallel_connections_issue_distinct_persistent_pin_authorities() {
    let dir = tempfile::tempdir().unwrap();
    // Existing metadata/WAL bootstrap is separate from concurrent allocator transactions.
    let _bootstrap = VectorCache::open(dir.path()).unwrap();
    let first = std::thread::spawn({
        let path = dir.path().to_path_buf();
        move || {
            let mut owner = authority(&path);
            owner.track("first").unwrap();
            owner.renew().unwrap();
            owner
        }
    });
    let mut second = authority(dir.path());
    second.track("second").unwrap();
    second.renew().unwrap();
    let first = first.join().unwrap();
    let cache = VectorCache::open(dir.path()).unwrap();
    assert_eq!(leases(&cache), vec![-2, -1]);
    drop((first, second));
}
