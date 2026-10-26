//! Hybrid stage resolution, cache-key composition and the blocking-context
//! contract (FR-3, FR-5, FR-13, FR-14, I3). Ungated: runs in the default
//! offline build and in `--features hybrid-remote` (SC-3, SC-4).

#[cfg(feature = "hybrid-local")]
use std::sync::Mutex;

use docsbase_memory::error::Error;
use docsbase_memory::hybrid::Ranking;
use docsbase_memory::hybrid::blocking;
#[cfg(feature = "hybrid-remote")]
use docsbase_memory::hybrid::config::RerankerCfg;
use docsbase_memory::hybrid::config::{
    CacheCfg, EmbeddingCfg, HybridCfg, ProvidersCfg, embedding_model_id, from_config,
};

#[cfg(feature = "hybrid-local")]
#[test]
#[allow(unsafe_code)] // env isolation mirrors the shared CLI test pattern
fn jobs_embed_texts_threads_real_providers() {
    // C5 sibling guard: `hybrid::jobs::embed_texts` (used by the daemon
    // job pipeline) must thread the real `ProvidersCfg`; a dangling
    // runtime_path surfaces the resolve-stage admission (--force hint),
    // never "is not connected" (which would indicate a `default()`).
    use docsbase_memory::hybrid::config::FastembedProvider;
    use docsbase_memory::hybrid::jobs;
    let cache = tempfile::TempDir::new().expect("cache");
    let cfg = HybridCfg {
        enabled: true,
        embedding: Some(EmbeddingCfg {
            provider: "fastembed".to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            ..EmbeddingCfg::default()
        }),
        ..HybridCfg::default()
    };
    let providers = ProvidersCfg {
        fastembed: Some(FastembedProvider {
            enabled: true,
            runtime: Some("1.24.4".to_owned()),
            runtime_path: Some("/nonexistent/libonnxruntime.so".into()),
        }),
        openrouter: None,
    };
    let prev_cache = std::env::var_os("DOCSBASE_CACHE_DIR");
    let env_guard = ENV_LOCK.lock().expect("env lock");
    // SAFETY: single-threaded under the shared module lock; restored
    // immediately after (round-2 M3: sibling env-mutating tests are
    // serialized through the same lock).
    unsafe {
        std::env::set_var("DOCSBASE_CACHE_DIR", cache.path());
    }
    let result = jobs::embed_texts(cache.path(), &cfg, &providers, &["hello".to_owned()]);
    // SAFETY: same locked section as above.
    unsafe {
        match prev_cache {
            Some(value) => std::env::set_var("DOCSBASE_CACHE_DIR", value),
            None => std::env::remove_var("DOCSBASE_CACHE_DIR"),
        }
    }
    drop(env_guard);
    let err = match result {
        Ok(_) => panic!("expected admission, got embeddings"),
        Err(e) => e,
    };
    let text = err.to_string();
    assert!(text.contains("fastembed runtime"), "{text}");
    assert!(text.contains("--force"), "{text}");
    assert!(!text.contains("is not connected"), "{text}");
}

#[test]
fn cache_key_carries_model_dim_and_postprocess() {
    let l2 = EmbeddingCfg {
        model: "qwen/qwen3-embedding-8b".to_owned(),
        dimensions: 1024,
        normalize: true,
        ..EmbeddingCfg::default()
    };
    // The fastembed arm is feature-gated; the OpenRouter arm must stay
    // byte-identical to the legacy formatter (FR-9, T3).
    assert_eq!(
        embedding_model_id(&l2),
        "openrouter/qwen/qwen3-embedding-8b@d1024@l2"
    );
    let raw = EmbeddingCfg {
        normalize: false,
        ..l2
    };
    assert_eq!(
        embedding_model_id(&raw),
        "openrouter/qwen/qwen3-embedding-8b@d1024@raw"
    );
}

#[test]
fn disabled_hybrid_resolves_to_empty_stages() {
    let stages = from_config(
        &HybridCfg::default(),
        &ProvidersCfg::default(),
        cfg!(feature = "hybrid-remote"),
        cfg!(feature = "hybrid-local"),
    )
    .expect("disabled hybrid must resolve");
    assert!(stages.embedder.is_none());
    assert!(stages.reranker.is_none());
}

#[test]
fn feature_off_admission_is_explicit() {
    // `feature_on = false` models the offline build even when the feature is
    // compiled in; the always-off build takes the same path.
    let error = from_config(
        &HybridCfg::enabled_with_defaults(),
        &ProvidersCfg::default(),
        false,
        cfg!(feature = "hybrid-local"),
    )
    .expect_err("feature-off + enabled must be an explicit admission error");
    let Error::Admission { message } = error else {
        panic!("expected Admission, got {error}");
    };
    assert!(
        message.contains("hybrid-remote"),
        "message must name the feature: {message}"
    );

    if !cfg!(feature = "hybrid-remote") {
        let error = from_config(
            &HybridCfg::enabled_with_defaults(),
            &ProvidersCfg::default(),
            true,
            cfg!(feature = "hybrid-local"),
        )
        .expect_err("default build must reject enabled hybrid");
        assert!(matches!(error, Error::Admission { .. }));
    }
}

#[cfg(feature = "hybrid-remote")]
#[test]
fn unknown_provider_is_rejected_before_any_network_setup() {
    let cfg = HybridCfg {
        enabled: true,
        embedding: Some(EmbeddingCfg {
            provider: "cohere".to_owned(),
            ..EmbeddingCfg::default()
        }),
        reranker: None,
        ranking: Ranking::Lexical,
        cache: CacheCfg::default(),
    };
    let error = from_config(
        &cfg,
        &ProvidersCfg::default(),
        true,
        cfg!(feature = "hybrid-local"),
    )
    .expect_err("provider must be validated");
    let Error::Admission { message } = error else {
        panic!("expected Admission");
    };
    assert!(message.contains("openrouter"), "{message}");

    let cfg = HybridCfg {
        enabled: true,
        embedding: None,
        reranker: Some(RerankerCfg {
            provider: "voyage".to_owned(),
            ..RerankerCfg::default()
        }),
        ranking: Ranking::Lexical,
        cache: CacheCfg::default(),
    };
    assert!(matches!(
        from_config(
            &cfg,
            &ProvidersCfg::default(),
            true,
            cfg!(feature = "hybrid-local")
        ),
        Err(Error::Admission { .. })
    ));
}

#[cfg(feature = "hybrid-remote")]
#[test]
fn rerank_window_bounds_are_enforced() {
    for window in [1, 9, 101, 500] {
        let cfg = HybridCfg {
            enabled: true,
            embedding: None,
            reranker: Some(RerankerCfg {
                window,
                ..RerankerCfg::default()
            }),
            ranking: Ranking::Lexical,
            cache: CacheCfg::default(),
        };
        let error = from_config(
            &cfg,
            &ProvidersCfg::default(),
            true,
            cfg!(feature = "hybrid-local"),
        )
        .expect_err("window must be bounded");
        let Error::Admission { message } = error else {
            panic!("expected Admission for window {window}");
        };
        assert!(message.contains("window"), "{message}");
    }
}

#[cfg(feature = "hybrid-remote")]
#[test]
fn missing_key_is_an_admission_error() {
    if std::env::var_os("OPENROUTER_API_KEY").is_some() {
        return; // the developer environment has a key; CI covers the error path
    }
    let error = from_config(
        &HybridCfg::enabled_with_defaults(),
        &ProvidersCfg::default(),
        true,
        cfg!(feature = "hybrid-local"),
    )
    .expect_err("missing key must fail admission");
    let Error::Admission { message } = error else {
        panic!("expected Admission");
    };
    assert!(message.contains("OPENROUTER_API_KEY"), "{message}");
}

#[test]
fn blocking_context_contract() {
    assert!(
        !blocking::in_context(),
        "outside enter the marker must be clear"
    );
    let inside = blocking::enter(|| {
        assert!(blocking::in_context());
        Ok(blocking::in_context())
    })
    .expect("enter propagates ok");
    assert!(inside);
    assert!(
        !blocking::in_context(),
        "the marker must be restored after enter"
    );

    let error = blocking::enter(|| Err::<(), _>(Error::internal("boom")))
        .expect_err("enter propagates errors");
    assert!(format!("{error}").contains("boom"));
    assert!(!blocking::in_context());
}

#[test]
fn rrf_without_the_embedding_stage_is_an_admission_error() {
    let cfg = HybridCfg {
        enabled: true,
        embedding: None,
        reranker: None,
        ranking: Ranking::Rrf,
        cache: CacheCfg::default(),
    };
    let err = from_config(
        &cfg,
        &ProvidersCfg::default(),
        true,
        cfg!(feature = "hybrid-local"),
    )
    .expect_err("rrf without embedding");
    assert!(
        matches!(&err, Error::Admission { message } if message.contains("rrf")),
        "{err}"
    );
}

#[test]
fn rrf_keeps_the_configuration_error_across_feature_modes() {
    let cfg = HybridCfg {
        enabled: true,
        embedding: None,
        reranker: None,
        ranking: Ranking::Rrf,
        cache: CacheCfg::default(),
    };
    for feature_on in [false, true] {
        let err = from_config(
            &cfg,
            &ProvidersCfg::default(),
            feature_on,
            cfg!(feature = "hybrid-local"),
        )
        .expect_err("rrf without embedding");
        assert!(
            matches!(&err, Error::Admission { message } if message.contains("rrf")),
            "feature_on={feature_on}: {err}"
        );
    }
}

#[test]
fn enabled_without_stages_resolves_to_empty_stages_offline() {
    let cfg = HybridCfg {
        enabled: true,
        embedding: None,
        reranker: None,
        ranking: Ranking::Lexical,
        cache: CacheCfg::default(),
    };
    let stages = from_config(&cfg, &ProvidersCfg::default(), false, false)
        .expect("no stages, nothing to resolve");
    assert!(stages.embedder.is_none() && stages.reranker.is_none());
}

#[cfg(all(feature = "hybrid-local", feature = "hybrid-remote"))]
#[test]
fn fastembed_only_config_does_not_demand_openrouter_key() {
    use docsbase_memory::hybrid::config::PROVIDER_FASTEMBED;
    use docsbase_memory::hybrid::config::resolve_embedding;
    let embedding = resolve_embedding(
        PROVIDER_FASTEMBED,
        Some("bge-small-en-v1.5".to_owned()),
        None,
        true,
    )
    .expect("resolve fastembed");
    let cfg = HybridCfg {
        enabled: true,
        embedding: Some(embedding),
        ..HybridCfg::default()
    };
    // No OPENROUTER_API_KEY in the environment: the fastembed-only config
    // must fail on the local seam (stub, T4), never on the remote key.
    let err = from_config(&cfg, &ProvidersCfg::default(), true, true).expect_err("local stage");
    assert!(!err.to_string().contains("OPENROUTER_API_KEY"), "{err}");
}

/// Module-wide lock serializing every env-mutating test in this binary
/// (round-2 M3): was a function-local static on
/// `from_config_fastembed_uses_real_provider_config`, which did not cover
/// `jobs_embed_texts_threads_real_providers`. Gated: both users are
/// `hybrid-local`-only, so the static must not survive in the default
/// build (round-3 P0 — default-feature clippy was red).
#[cfg(feature = "hybrid-local")]
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(feature = "hybrid-local")]
#[test]
#[allow(unsafe_code)] // env isolation under the local lock
fn from_config_fastembed_uses_real_provider_config() {
    use docsbase_memory::hybrid::config::{FastembedProvider, ProviderState};

    // Runtime present via runtime_path; model cache isolated & empty → the
    // stage must fail with the FETCH hint, never "not connected" (P0: the
    // connect-provisioned provider must actually be honored).
    let cache = tempfile::TempDir::new().expect("cache");
    let runtime = tempfile::TempDir::new().expect("runtime");
    let runtime_path = runtime.path().join("libonnxruntime.so");
    std::fs::write(&runtime_path, b"fake").expect("touch");
    let providers = ProvidersCfg {
        fastembed: Some(FastembedProvider {
            enabled: true,
            runtime: Some("1.24.4".to_owned()),
            runtime_path: Some(runtime_path),
        }),
        openrouter: None,
    };
    let cfg = HybridCfg {
        enabled: true,
        embedding: Some(EmbeddingCfg {
            provider: docsbase_memory::hybrid::config::PROVIDER_FASTEMBED.to_owned(),
            model: "bge-small-en-v1.5".to_owned(),
            dimensions: 384,
            normalize: true,
        }),
        ..HybridCfg::default()
    };

    let _env_guard = ENV_LOCK.lock().expect("env lock");
    let prev = std::env::var_os("DOCSBASE_CACHE_DIR");
    // SAFETY: single-threaded under the local lock.
    unsafe {
        std::env::set_var("DOCSBASE_CACHE_DIR", cache.path());
    }
    let result = from_config(&cfg, &providers, false, true);
    // SAFETY: same locked section.
    unsafe {
        match prev {
            Some(value) => std::env::set_var("DOCSBASE_CACHE_DIR", value),
            None => std::env::remove_var("DOCSBASE_CACHE_DIR"),
        }
    }
    let err = result.expect_err("fastembed stage must fail on missing model");
    let text = err.to_string();
    assert!(text.contains("models fetch"), "{text}");
    assert!(!text.contains("not connected"), "{text}");
    let _ = ProviderState::Unknown; // keep import used for future state checks
}
