//! Live record smoke test (T9): runs only with `--features hybrid-remote`
//! and `OPENROUTER_API_KEY` set. Records real `OpenRouter` fixtures into a
//! temp dir and replays them offline. Ignored by default so the offline
//! suite never touches the network.

#![cfg(feature = "hybrid-remote")]

use std::sync::Arc;

use docsbase_memory::hybrid::openrouter::{OpenRouterEmbedder, OpenRouterReranker};
use docsbase_memory::hybrid::{EmbeddingCfg, RerankerCfg};
use docsbase_memory::vector_cache::Embedder;

mod support;

fn openrouter_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
}

/// Record with the pinned production models, then replay offline.
/// Cost: 5 graded docs + 2 queries — well under the FR-18 $2 cap.
#[test]
#[ignore = "needs OPENROUTER_API_KEY + hybrid-remote (live record smoke)"]
fn live_record_and_replay() {
    let Some(key) = openrouter_key() else {
        eprintln!("SKIP: export OPENROUTER_API_KEY to run the live record smoke test");
        return;
    };
    assert!(!key.trim().is_empty());
    let tmp = tempfile::TempDir::new().expect("tmp");
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).expect("mkdir corpus");
    for (name, terms) in [
        ("a.md", "alpha beta gamma alpha beta alpha"),
        ("b.md", "alpha beta delta"),
        ("c.md", "epsilon zeta eta"),
        ("d.md", "alpha theta"),
        ("e.md", "iota kappa lambda"),
    ] {
        std::fs::write(corpus.join(name), format!("# {name}\n\n{terms}\n")).expect("write doc");
    }
    let env = support::bench::build_env(&corpus, "live", 5..=5).expect("index corpus");
    let cases = ["live-01", "live-02"]
        .iter()
        .zip(["alpha beta", "epsilon zeta"])
        .map(|(id, query)| support::bench::QueryCase {
            id: (*id).to_owned(),
            query: (*query).to_owned(),
            class: "probe".to_owned(),
            k: 5,
            expected: vec![],
            absent: vec![],
            source: "live-smoke".to_owned(),
            notes: "live record smoke".to_owned(),
            reviewed_by: vec![],
        })
        .collect::<Vec<_>>();
    let embedder: Arc<dyn Embedder> =
        Arc::new(OpenRouterEmbedder::new(EmbeddingCfg::default()).expect("embedder"));
    let reranker = OpenRouterReranker::new(RerankerCfg::default()).expect("reranker");
    let vectors_dir = tmp.path().join("vectors");
    let rerank_path = tmp.path().join("rerank.json");
    support::bench::record_fixtures_with(
        &env,
        &cases,
        &embedder,
        &reranker,
        "openrouter/cohere/rerank-v3.5",
        &vectors_dir,
        &rerank_path,
    )
    .expect("live record");
    // Reload from disk and replay fully offline.
    let loaded_vectors = support::bench::VectorFixture::load(&vectors_dir).expect("load vectors");
    let loaded_rerank = support::bench::RerankFixture::load(&rerank_path).expect("load rerank");
    assert_eq!(
        loaded_vectors.model_pin,
        "openrouter/qwen/qwen3-embedding-8b@d1024@l2"
    );
    let replay_embedder = support::bench::ReplayEmbedder {
        pin: loaded_vectors.model_pin.clone(),
        fixture: Arc::new(loaded_vectors),
    };
    assert!(replay_embedder.fixture.vectors.len() > 3);
    let replay_reranker = support::bench::ReplayReranker {
        fixture: Arc::new(loaded_rerank),
    };
    for cfg in [
        support::bench::ConfigId::Bm25,
        support::bench::ConfigId::Bm25Rerank,
        support::bench::ConfigId::Rrf,
        support::bench::ConfigId::RrfRerank,
    ] {
        let stages = support::bench::stages_for(
            cfg,
            Some(Arc::new(replay_embedder.clone())),
            Some(Arc::new(replay_reranker.clone())),
        );
        let report = support::bench::run_config(&env, &cases, cfg, &stages).expect("replay run");
        assert!(
            !report.cases[0].citations.is_empty(),
            "{cfg:?}: replay produced no citations"
        );
    }
}
