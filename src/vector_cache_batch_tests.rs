//! Page transactions preserve streaming memory and rollback failed LRU touches.
use super::*;

#[test]
fn vector_page_streams_in_order_with_model_isolation_and_missing_rows() {
    let cache = tempfile::tempdir().expect("cache");
    let mut vectors = VectorCache::open(cache.path()).expect("vectors");
    vectors.put("model", "a", &[1.0, 0.0]).expect("put");
    vectors.put("other", "a", &[0.0, 1.0]).expect("other model");
    vectors.put("model", "b", &[0.5, 0.5]).expect("put");
    let hashes = vec![
        (1, "b".to_owned()),
        (2, "missing".to_owned()),
        (3, "a".to_owned()),
    ];
    let mut observed = Vec::new();
    vectors
        .visit_page("model", &hashes, |id, vector| {
            observed.push((id, vector.map(<[f32]>::to_vec)));
            Ok(())
        })
        .expect("page");
    assert_eq!(
        observed,
        vec![
            (1, Some(vec![0.5, 0.5])),
            (2, None),
            (3, Some(vec![1.0, 0.0]))
        ]
    );
    assert!(vectors.conn.is_autocommit());
    let used: Vec<i64> = vectors
        .conn
        .prepare("SELECT used_at FROM vector_cache WHERE model_id='model' ORDER BY chunk_hash")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows");
    assert!(used[0] > used[1], "LRU follows visitation order");
}

#[test]
fn failed_vector_page_rolls_back_touches_and_allows_retry() {
    let cache = tempfile::tempdir().expect("cache");
    let mut vectors = VectorCache::open(cache.path()).expect("vectors");
    vectors.put("model", "a", &[1.0, 0.0]).expect("put");
    let before: i64 = vectors
        .conn
        .query_row("SELECT used_at FROM vector_cache", [], |row| row.get(0))
        .expect("before");
    let hashes = vec![(1, "a".to_owned())];
    let result = vectors.visit_page("model", &hashes, |_, _| {
        Err(Error::Admission {
            message: "cancelled test page".to_owned(),
        })
    });
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert!(vectors.conn.is_autocommit());
    let after: i64 = vectors
        .conn
        .query_row("SELECT used_at FROM vector_cache", [], |row| row.get(0))
        .expect("after");
    assert_eq!(before, after);
    vectors
        .visit_page("model", &hashes, |_, vector| {
            assert_eq!(vector, Some([1.0, 0.0].as_slice()));
            Ok(())
        })
        .expect("retry");
    assert!(vectors.conn.is_autocommit());
}
