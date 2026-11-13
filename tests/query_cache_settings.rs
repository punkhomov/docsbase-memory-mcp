use std::sync::{Arc, Barrier};

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::resolve_component;
use docsbase_memory::config::schema::{SettingsSpec, SourceId};
use docsbase_memory::hybrid::dense::{AnnPolicy, DenseCache, DenseCacheLimits};
use docsbase_memory::hybrid::dense_settings::{
    DensePatch, DenseSpec, QueryCacheSettings, standalone_query_cache,
};
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::vector_cache::Embedder;

fn settings(entries: usize, bytes: u64) -> QueryCacheSettings {
    resolve_component::<DenseSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            DensePatch {
                query_cache_max_entries: Some(entries),
                query_cache_max_bytes: Some(bytes),
                ..DensePatch::default()
            },
        )],
    )
    .unwrap()
    .settings()
    .query_cache_settings()
}

fn cache(settings: &QueryCacheSettings) -> DenseCache {
    let root = tempfile::tempdir().unwrap();
    let result = DenseCache::with_policy_limits_and_query_settings(
        AnnPolicy::Disabled,
        root.path(),
        DenseCacheLimits::default(),
        settings,
    )
    .unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    result
}

#[test]
fn owner_defaults_scope_and_positive_validation_are_preserved() {
    let defaults = standalone_query_cache().unwrap();
    assert_eq!(defaults.max_entries, 256);
    assert_eq!(defaults.max_bytes, 4 * 1024 * 1024);
    let dense = DenseSpec::defaults(&DefaultsContext::standalone());
    assert_eq!(dense.query_cache_settings(), defaults);
    for (entries, bytes) in [(0, 16), (2, 0)] {
        assert!(
            resolve_component::<DenseSpec>(
                &DefaultsContext::standalone(),
                [(
                    SourceId::Cli,
                    DensePatch {
                        query_cache_max_entries: Some(entries),
                        query_cache_max_bytes: Some(bytes),
                        ..DensePatch::default()
                    }
                )]
            )
            .is_err()
        );
        assert!(
            QueryCacheSettings {
                max_entries: entries,
                max_bytes: bytes
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        resolve_component::<DenseSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Project("project.toml".into()),
                DensePatch {
                    query_cache_max_entries: Some(2),
                    ..DensePatch::default()
                }
            )]
        )
        .is_err()
    );
}

#[test]
fn supplied_entry_limit_evicts_the_actual_least_recent_query() {
    let cache = cache(&settings(2, 128));
    cache.store_query_vector("model", "a", &[1., 2.]);
    cache.store_query_vector("model", "b", &[3., 4.]);
    assert!(cache.query_vector("model", "a").is_some());
    cache.store_query_vector("model", "c", &[5., 6.]);
    assert!(cache.query_vector("model", "b").is_none());
    assert!(cache.query_vector("model", "a").is_some());
    assert!(cache.query_vector("model", "c").is_some());
    assert_eq!(cache.query_cache_stats().entries, 2);
    assert_eq!(cache.query_cache_stats().bytes, 16);
}

#[test]
fn supplied_byte_limit_evicts_and_bypasses_oversized_vectors() {
    let cache = cache(&settings(10, 16));
    cache.store_query_vector("model", "a", &[1., 2., 3.]);
    cache.store_query_vector("model", "b", &[4., 5., 6.]);
    assert!(cache.query_vector("model", "a").is_none());
    assert!(cache.query_vector("model", "b").is_some());
    cache.store_query_vector("model", "oversized", &[1., 2., 3., 4., 5.]);
    assert!(cache.query_vector("model", "oversized").is_none());
    assert_eq!(cache.query_cache_stats().entries, 1);
    assert_eq!(cache.query_cache_stats().bytes, 12);
}

#[test]
fn sharing_query_cache_keeps_policy_and_eviction_state_together() {
    let cache = cache(&settings(1, 16));
    let shared = cache.sharing_query_cache();
    cache.store_query_vector("model", "a", &[1., 2.]);
    assert!(shared.query_vector("model", "a").is_some());
    shared.store_query_vector("model", "b", &[3., 4.]);
    assert!(cache.query_vector("model", "a").is_none());
    assert!(cache.query_vector("model", "b").is_some());
    assert_eq!(cache.query_cache_stats().entries, 1);
    assert_eq!(shared.query_cache_stats().bytes, 8);
}

#[test]
fn concurrent_shared_insertions_obey_one_entry_and_byte_budget() {
    let cache = cache(&settings(2, 16));
    let gate = Arc::new(Barrier::new(5));
    let threads: Vec<_> = (0..4)
        .map(|thread| {
            let cache = cache.sharing_query_cache();
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                for n in 0..30 {
                    cache.store_query_vector("model", &format!("{thread}-{n}"), &[1., 2.]);
                }
            })
        })
        .collect();
    gate.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(cache.query_cache_stats().entries, 2);
    assert_eq!(cache.query_cache_stats().bytes, 16);
}

struct CountingEmbedder(std::sync::atomic::AtomicUsize);
impl Embedder for CountingEmbedder {
    fn model_id(&self) -> &'static str {
        "query-policy@d3"
    }
    fn embed(&self, _: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        panic!("query cache must call query-specific embedding")
    }
    fn embed_query(&self, _: &str) -> docsbase_memory::error::Result<Vec<f32>> {
        assert!(docsbase_memory::hybrid::blocking::in_context());
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![1., 2., 3.])
    }
}

#[test]
fn actual_query_embedding_reuses_retained_vectors_and_serves_uncacheable_ones() {
    let retained = cache(&settings(2, 12));
    let embedder = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
    let first = retained
        .query_embedding(&embedder, "a", &Cancellation::new())
        .unwrap();
    let second = retained
        .query_embedding(&embedder, "a", &Cancellation::new())
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(embedder.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    let bypass = cache(&settings(2, 8));
    for _ in 0..2 {
        assert_eq!(
            bypass
                .query_embedding(&embedder, "a", &Cancellation::new())
                .unwrap()
                .as_ref(),
            &[1., 2., 3.]
        );
    }
    assert_eq!(embedder.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(bypass.query_cache_stats().entries, 0);
    assert_eq!(bypass.query_cache_stats().bytes, 0);
}

#[test]
fn invalid_cache_policy_fails_before_constructing_resources() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("absent");
    let invalid = QueryCacheSettings {
        max_entries: 0,
        max_bytes: 16,
    };
    assert!(
        DenseCache::with_policy_limits_and_query_settings(
            AnnPolicy::BuildOrLoad,
            &cache,
            DenseCacheLimits::default(),
            &invalid
        )
        .is_err()
    );
    assert!(!cache.exists());
}

#[test]
fn daemon_construction_consumes_explicit_query_policy() {
    let root = tempfile::tempdir().unwrap();
    let db = docsbase_memory::store::settings::standalone().unwrap();
    let pool = docsbase_memory::store::settings::standalone_read_pool().unwrap();
    let shared = docsbase_memory::daemon::server::Shared::open_with_resource_policies(
        root.path(),
        &db,
        &pool,
        &settings(1, 16),
    )
    .unwrap();
    shared.dense.store_query_vector("model", "a", &[1., 2.]);
    shared.dense.store_query_vector("model", "b", &[3., 4.]);
    assert!(shared.dense.query_vector("model", "a").is_none());
    assert!(shared.dense.query_vector("model", "b").is_some());
    assert_eq!(shared.dense.query_cache_stats().entries, 1);
}

#[test]
fn daemon_imported_invalid_policy_is_rejected_before_any_filesystem_resources() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("absent");
    let db = docsbase_memory::store::settings::standalone().unwrap();
    let pool = docsbase_memory::store::settings::standalone_read_pool().unwrap();
    let invalid = QueryCacheSettings {
        max_entries: 2,
        max_bytes: 0,
    };
    assert!(
        docsbase_memory::daemon::server::Shared::open_with_resource_policies(
            &cache, &db, &pool, &invalid
        )
        .is_err()
    );
    assert!(!cache.exists());
    assert!(
        docsbase_memory::daemon::lifecycle::run_daemon_with_resource_policies(
            &cache,
            std::time::Duration::from_millis(1),
            &db,
            &pool,
            &invalid
        )
        .is_err()
    );
    assert!(!cache.exists());
}
