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

#[test]
fn negative_authority_release_restores_cache_budget_and_renew_prunes_expired_leases() {
    for owned in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut policy = settings::VectorStoreSettings::owner_defaults();
        policy.max_entries = 1;
        policy.max_bytes = 1024;
        let source = if owned {
            VectorCacheSource::Owned(VectorStore::acquire(dir.path(), &policy).unwrap())
        } else {
            VectorCacheSource::Legacy(policy.limits())
        };
        let model = "model@d1@l2";
        let hashes = vec![chunk_hash("first"), chunk_hash("second")];
        let mut pin = JobPinAuthority::allocate(
            dir.path(),
            source.clone(),
            model.into(),
            Duration::from_secs(60),
            2,
        )
        .unwrap();
        for hash in &hashes {
            pin.track(hash).unwrap();
        }
        pin.renew().unwrap();
        let mut cache = source.connect(dir.path()).unwrap();
        for hash in &hashes {
            cache.put(model, hash, &[1.0]).unwrap();
        }
        assert_eq!(
            cache.count_present(model, &hashes).unwrap(),
            2,
            "both live own pins suspend eviction"
        );
        drop(pin);
        assert_eq!(
            cache.count_present(model, &hashes).unwrap(),
            1,
            "releasing negative authority must restore the same vector budget as public unpin"
        );
        cache
            .pin(7, model, &hashes, Duration::from_secs(60))
            .unwrap();
        cache
            .conn
            .execute(
                "UPDATE vector_cache_leases SET expires_at=0 WHERE lease_id=7",
                [],
            )
            .unwrap();
        let mut next = JobPinAuthority::allocate(
            dir.path(),
            source.clone(),
            model.into(),
            Duration::from_secs(60),
            2,
        )
        .unwrap();
        next.track(&hashes[0]).unwrap();
        next.renew().unwrap();
        let expired: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM vector_cache_leases WHERE lease_id=7",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            expired, 0,
            "private renewal preserves existing expiry pruning"
        );
        let counter: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM vector_cache_leases WHERE lease_id=?1",
                [i64::MIN],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            counter, 1,
            "expiry pruning cannot remove persistent allocator state"
        );
    }
}

#[test]
fn catalog_pin_admission_bounds_distinct_vector_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = VectorCacheSource::Legacy(CacheLimits {
        max_entries: 2,
        max_bytes: 8,
    });
    let mut pin = JobPinAuthority::allocate(
        root.path(),
        source,
        "model".into(),
        Duration::from_secs(60),
        3,
    )
    .unwrap();
    pin.track("first").unwrap();
    pin.track("first").unwrap();
    pin.check_capacity(2).unwrap();
    pin.track("second").unwrap();
    pin.check_capacity(1).unwrap();
    assert!(pin.check_capacity(2).is_err());
    pin.track("third").unwrap();
    assert!(pin.check_capacity(1).is_err());
}

#[test]
fn publication_vector_proof_rejects_missing_or_wrong_dimensions_and_holds_the_writer() {
    let root = tempfile::tempdir().unwrap();
    let mut pin = authority(root.path());
    pin.track("first").unwrap();
    pin.renew().unwrap();
    let mut cache = VectorCache::open(root.path()).unwrap();
    assert!(
        pin.prepare_verification(2)
            .unwrap()
            .hold_verified(|| Ok(()))
            .is_err()
    );
    cache.put("model", "first", &[1.0]).unwrap();
    assert!(
        pin.prepare_verification(2)
            .unwrap()
            .hold_verified(|| Ok(()))
            .is_err()
    );
    cache
        .conn
        .execute(
            "UPDATE vector_cache SET dim = 2, vector = 'bad row!' WHERE chunk_hash = 'first'",
            [],
        )
        .unwrap();
    assert!(
        cache
            .get_for_dimension("model", "first", 2)
            .unwrap()
            .is_none()
    );
    let pending = pin.prepare_verification(2).unwrap();
    cache.put("model", "first", &[f32::NAN, 0.0]).unwrap();
    assert!(pending.hold_verified(|| Ok(())).is_err());
    cache.put("model", "first", &[1.0, 0.0]).unwrap();
    let pending = pin.prepare_verification(2).unwrap();
    cache.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(pending.hold_verified(|| Ok(())).is_err());
    cache.conn.execute_batch("ROLLBACK").unwrap();
    let proof = pin
        .prepare_verification(2)
        .unwrap()
        .hold_verified(|| Ok(()))
        .unwrap();
    cache.conn.busy_timeout(Duration::ZERO).unwrap();
    let blocked = cache.conn.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert!(matches!(blocked, rusqlite::Error::SqliteFailure(code, _)
        if code.code == rusqlite::ErrorCode::DatabaseBusy));
    drop(proof);
    cache
        .conn
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
}
