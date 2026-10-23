//! Hybrid stage resolution, cache-key composition and the blocking-context
//! contract (FR-3, FR-5, FR-13, FR-14, I3). Ungated: runs in the default
//! offline build and in `--features hybrid-remote` (SC-3, SC-4).

use docsbase_memory::error::Error;
use docsbase_memory::hybrid::Ranking;
use docsbase_memory::hybrid::blocking;
#[cfg(feature = "hybrid-remote")]
use docsbase_memory::hybrid::config::RerankerCfg;
use docsbase_memory::hybrid::config::{
    CacheCfg, EmbeddingCfg, HybridCfg, embedding_model_id, from_config,
};

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
    let error = from_config(&cfg, true, cfg!(feature = "hybrid-local"))
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
        from_config(&cfg, true, cfg!(feature = "hybrid-local")),
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
        let error = from_config(&cfg, true, cfg!(feature = "hybrid-local"))
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
    let err =
        from_config(&cfg, true, cfg!(feature = "hybrid-local")).expect_err("rrf without embedding");
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
        let err = from_config(&cfg, feature_on, cfg!(feature = "hybrid-local"))
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
    let stages = from_config(&cfg, false, false).expect("no stages, nothing to resolve");
    assert!(stages.embedder.is_none() && stages.reranker.is_none());
}
