//! Integration coverage for the local fastembed embedder (T4): provisioning
//! failures and the canonical-id/key contract reachable without real weights.
//! Live inference is T8 (`tests/hybrid_local_live.rs`).

#![cfg(feature = "hybrid-local")]

use docsbase_memory::error::Error;
use docsbase_memory::hybrid::config::{PROVIDER_FASTEMBED, ProvidersCfg, embedding_model_id};
use docsbase_memory::hybrid::local::{ModelCacheKey, embedder as local_embedder};
use tempfile::TempDir;

/// Integration-test env isolation: the real user cache may already contain a
/// fetched model (e.g. after a live T8 run), which would break the
/// "without model → fetch hint" provisioning tests. Serialized across tests
/// in this binary.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[allow(unsafe_code)] // env mutation is confined to the locked section
fn with_isolated_empty_cache<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let cache = TempDir::new().expect("cache");
    let config = TempDir::new().expect("config");
    let prev_cache = std::env::var_os("DOCSBASE_CACHE_DIR");
    let prev_config = std::env::var_os("DOCSBASE_CONFIG_DIR");
    unsafe {
        std::env::set_var("DOCSBASE_CACHE_DIR", cache.path());
        std::env::set_var("DOCSBASE_CONFIG_DIR", config.path());
    }
    let out = f();
    unsafe {
        match prev_cache {
            Some(value) => std::env::set_var("DOCSBASE_CACHE_DIR", value),
            None => std::env::remove_var("DOCSBASE_CACHE_DIR"),
        }
        match prev_config {
            Some(value) => std::env::set_var("DOCSBASE_CONFIG_DIR", value),
            None => std::env::remove_var("DOCSBASE_CONFIG_DIR"),
        }
    }
    out
}

#[test]
fn embedder_without_runtime_is_connect_hint() {
    let cfg = docsbase_memory::hybrid::config::EmbeddingCfg {
        provider: PROVIDER_FASTEMBED.to_owned(),
        model: "bge-small-en-v1.5".to_owned(),
        dimensions: 384,
        normalize: true,
    };
    let providers = ProvidersCfg::default();
    let Err(err) = local_embedder(&cfg, &providers) else {
        panic!("expected admission without a runtime");
    };
    assert!(matches!(err, Error::Admission { .. }));
    assert!(err.to_string().contains("connect fastembed"), "{err}");
}

#[test]
fn embedder_without_model_is_fetch_hint() {
    let cfg = docsbase_memory::hybrid::config::EmbeddingCfg {
        provider: PROVIDER_FASTEMBED.to_owned(),
        model: "bge-small-en-v1.5".to_owned(),
        dimensions: 384,
        normalize: true,
    };
    let runtime = TempDir::new().expect("runtime");
    let runtime_path = runtime.path().join("libonnxruntime.so");
    std::fs::write(&runtime_path, b"fake").expect("touch");
    let providers = ProvidersCfg {
        fastembed: Some(docsbase_memory::hybrid::config::FastembedProvider {
            enabled: true,
            runtime: Some("1.24.4".to_owned()),
            runtime_path: Some(runtime_path),
        }),
        openrouter: None,
    };
    let Err(err) = with_isolated_empty_cache(|| local_embedder(&cfg, &providers)) else {
        panic!("expected admission without a fetched model");
    };
    assert!(matches!(err, Error::Admission { .. }));
    assert!(err.to_string().contains("models fetch"), "{err}");
}

#[test]
fn cache_key_shape_matches_canonical_formatter() {
    let cfg = docsbase_memory::hybrid::config::EmbeddingCfg {
        provider: PROVIDER_FASTEMBED.to_owned(),
        model: "multilingual-e5-small".to_owned(),
        dimensions: 384,
        normalize: true,
    };
    let id = embedding_model_id(&cfg);
    assert_eq!(id, "fastembed/multilingual-e5-small@d384@l2@r761b726d@p1");
    // The embedder cache key carries the canonical id, a bounded max length,
    // capped threads and a runtime path — the dedup contract (NFR-4/7).
    let key = ModelCacheKey {
        model_id: id.clone(),
        max_length: 512,
        intra_threads: Some(4),
        runtime: std::path::PathBuf::from("/tmp/libonnxruntime.so"),
    };
    assert_eq!(key.model_id, id);
}
