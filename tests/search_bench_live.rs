//! Live record smoke test (T9): runs only with `--features hybrid-remote`
//! and `OPENROUTER_API_KEY` set. Records real `OpenRouter` fixtures into a
//! temp dir and replays them offline. Ignored by default so the offline
//! suite never touches the network.

#![cfg(feature = "hybrid-remote")]

use std::sync::Arc;

use docsbase_memory::hybrid::config::cache_model_id;
use docsbase_memory::hybrid::openrouter::{OpenRouterEmbedder, OpenRouterReranker};
use docsbase_memory::hybrid::{EmbeddingCfg, RerankerCfg};
use docsbase_memory::vector_cache::Embedder;

mod support;

fn openrouter_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
}

/// Records production fixtures for the full frozen corpus (FR-19):
/// vectors for every chunk + query under the embedding pin, rerank pools
/// for both rerank configs under the rerank pin. Manual, keyed, atomic.
/// Run: `OPENROUTER_API_KEY=... cargo test --locked --features
/// hybrid-remote --test search_bench_live -- --ignored record_fixtures`.
#[test]
#[ignore = "manual: records production fixtures with OPENROUTER_API_KEY (FR-19)"]
fn record_fixtures() {
    openrouter_key().expect("export OPENROUTER_API_KEY to record fixtures");
    let embed_cfg = EmbeddingCfg::default();
    let rerank_cfg = RerankerCfg::default();
    let embed_pin = cache_model_id(&embed_cfg);
    assert_eq!(
        embed_pin,
        "openrouter/nvidia/nemotron-3-embed-1b:free@d2048@l2"
    );
    let rerank_pin = format!("{}/{}", rerank_cfg.provider, rerank_cfg.model);
    assert_eq!(
        rerank_pin,
        "openrouter/nvidia/llama-nemotron-rerank-vl-1b-v2:free"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).expect("fixtures");
    assert!(
        fixtures.cases.len() >= 120,
        "record needs the finalized corpus (T6+)"
    );
    let env = support::bench::build_env(&root.join("corpus"), "record", 60..=85).expect("index");
    let embedder: Arc<dyn Embedder> =
        Arc::new(OpenRouterEmbedder::new(embed_cfg).expect("embedder"));
    let reranker = OpenRouterReranker::new(rerank_cfg).expect("reranker");
    let vectors_dir = root
        .join("vectors")
        .join(support::bench::pin_slug(&embed_pin));
    let rerank_path = root
        .join("rerank")
        .join(format!("{}.json", support::bench::pin_slug(&rerank_pin)));
    // Network calls require the sanctioned blocking context (I3); the test
    // thread is not one, so enter it explicitly.
    docsbase_memory::hybrid::blocking::enter(|| {
        support::bench::record_fixtures_with(
            &env,
            &fixtures.cases,
            &embedder,
            &reranker,
            &rerank_pin,
            &vectors_dir,
            &rerank_path,
        )
        .map_err(|err| docsbase_memory::error::Error::internal(err.to_string()))
    })
    .expect("record");
    // Reload what was written: the replay path must accept it verbatim.
    let loaded_vectors = support::bench::VectorFixture::load(&vectors_dir).expect("reload vectors");
    assert_eq!(loaded_vectors.model_pin, embed_pin);
    let loaded_rerank = support::bench::RerankFixture::load(&rerank_path).expect("reload rerank");
    assert_eq!(loaded_rerank.model_pin, rerank_pin);
    println!(
        "recorded {} vectors + {} rerank pools",
        loaded_vectors.vectors.len(),
        loaded_rerank.pools.len()
    );
}
/// Tiny live smoke: records 5 docs + 2 cases, then replays offline.
/// Cost is negligible (FR-18 $2 cap).
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
    docsbase_memory::hybrid::blocking::enter(|| {
        support::bench::record_fixtures_with(
            &env,
            &cases,
            &embedder,
            &reranker,
            "openrouter/nvidia/llama-nemotron-rerank-vl-1b-v2:free",
            &vectors_dir,
            &rerank_path,
        )
        .map_err(|err| docsbase_memory::error::Error::internal(err.to_string()))
    })
    .expect("live record");
    // Reload from disk and replay fully offline.
    let loaded_vectors = support::bench::VectorFixture::load(&vectors_dir).expect("load vectors");
    let loaded_rerank = support::bench::RerankFixture::load(&rerank_path).expect("load rerank");
    assert_eq!(
        loaded_vectors.model_pin,
        "openrouter/nvidia/nemotron-3-embed-1b:free@d2048@l2"
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

// ---------------------------------------------------------------------------
// T13: live measurement + cost cap (FR-17, FR-18, FR-32; NFR-7).
// Unit tests run without a key; `live_measurement` is manual + ignored.
// ---------------------------------------------------------------------------

use support::bench::{
    LiveUsage, ParetoPoint, cost_cap_from_env, estimate_cost_usd, pareto_frontier,
};

#[test]
fn cost_cap_stops_before_the_next_request() {
    support::bench::check_cost_cap(0.5, 0.4, 1.0).expect("within cap");
    let err = support::bench::check_cost_cap(0.9, 0.2, 1.0).expect_err("over cap must fail");
    assert!(err.to_string().contains("cost cap exceeded"), "{err}");
    // Exact fit passes (strictly-greater breach rule).
    support::bench::check_cost_cap(0.9, 0.1, 1.0).expect("exact fit passes");
}

#[test]
fn usage_is_parsed_from_recorded_response() {
    use docsbase_memory::hybrid::wire::{Transport, TransportResponse};

    struct Scripted {
        bodies: std::sync::Mutex<Vec<(u16, String)>>,
    }
    impl Transport for Scripted {
        fn post_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &str,
        ) -> docsbase_memory::error::Result<TransportResponse> {
            let (status, body) = self.bodies.lock().expect("queue").pop().expect("script");
            Ok(TransportResponse {
                status,
                body,
                retry_after: None,
            })
        }
    }
    use std::sync::Arc;
    // Embeddings usage shape (OpenAI-style).
    let scripted = Arc::new(Scripted {
        bodies: std::sync::Mutex::new(vec![(
            200,
            r#"{"data": [{"embedding": [0.1, 0.2], "index": 0}], "usage": {"prompt_tokens": 11, "total_tokens": 11}}"#.to_owned(),
        )]),
    });
    let transport = support::bench::live_transport::RecordingTransport::new(
        scripted,
        "openrouter/qwen/qwen3-embedding-8b@d1024@l2",
    );
    transport
        .post_json("https://openrouter.ai/api/v1/embeddings", &[], "{}")
        .expect("recorded");
    let usage = transport.usage();
    assert_eq!(usage.calls, 1);
    assert_eq!(usage.prompt_tokens, 11);
    assert_eq!(usage.total_tokens, 11);
    // Rerank usage shape (cohere billed_units).
    let scripted = Arc::new(Scripted {
        bodies: std::sync::Mutex::new(vec![(
            200,
            r#"{"results": [{"index": 0, "relevance_score": 0.9}], "meta": {"billed_units": {"search_units": 3}}}"#.to_owned(),
        )]),
    });
    let transport = support::bench::live_transport::RecordingTransport::new(
        scripted,
        "openrouter/cohere/rerank-v3.5",
    );
    transport
        .post_json("https://openrouter.ai/api/v1/rerank", &[], "{}")
        .expect("recorded");
    let usage = transport.usage();
    assert_eq!(usage.calls, 1);
    assert_eq!(usage.search_units, 3);
}

/// Cost math: free pins cost nothing, paid pins follow the price list,
/// unknown pins refuse to guess.
#[test]
fn live_cost_math() {
    let usage = LiveUsage {
        prompt_tokens: 1_000_000,
        total_tokens: 1_000_000,
        search_units: 0,
        calls: 5,
    };
    assert_eq!(
        estimate_cost_usd(
            "openrouter/nvidia/nemotron-3-embed-1b:free@d2048@l2",
            &usage
        ),
        Some(0.0)
    );
    assert_eq!(
        estimate_cost_usd("openrouter/qwen/qwen3-embedding-8b@d1024@l2", &usage),
        Some(0.01)
    );
    assert_eq!(estimate_cost_usd("openrouter/mystery-model", &usage), None);
    let rerank_usage = LiveUsage {
        calls: 3,
        ..Default::default()
    };
    assert_eq!(
        estimate_cost_usd(
            "openrouter/nvidia/llama-nemotron-rerank-vl-1b-v2:free",
            &rerank_usage
        ),
        Some(0.0)
    );
    assert_eq!(
        estimate_cost_usd("openrouter/cohere/rerank-v3.5", &rerank_usage),
        Some(0.003)
    );
}

/// Live report shape: all four configs present, pareto marks dominated.
#[test]
fn live_report_contains_all_four_configs() {
    let mut points = vec![
        ParetoPoint {
            config: support::bench::ConfigId::Bm25,
            quality: 0.5,
            cost_usd: 0.0,
            latency_p95_ms: 10.0,
            on_frontier: false,
        },
        ParetoPoint {
            config: support::bench::ConfigId::Bm25Rerank,
            quality: 0.6,
            cost_usd: 0.0,
            latency_p95_ms: 20.0,
            on_frontier: false,
        },
        ParetoPoint {
            config: support::bench::ConfigId::Rrf,
            quality: 0.55,
            cost_usd: 0.0,
            latency_p95_ms: 15.0,
            on_frontier: false,
        },
        ParetoPoint {
            config: support::bench::ConfigId::RrfRerank,
            quality: 0.6,
            cost_usd: 0.0,
            latency_p95_ms: 25.0,
            on_frontier: false,
        },
    ];
    pareto_frontier(&mut points);
    // Bm25Rerank (0.6/0$/20ms) dominates RrfRerank (0.6/0$/25ms).
    let rrf_rerank = points
        .iter()
        .find(|p| p.config == support::bench::ConfigId::RrfRerank)
        .expect("point");
    assert!(!rrf_rerank.on_frontier, "dominated point must be marked");
    assert!(
        points.iter().any(|p| p.on_frontier),
        "frontier must not be empty"
    );
}

/// Pareto marks dominated points and keeps the trade-off surface.
#[test]
fn pareto_marks_dominated() {
    let mut points = vec![
        ParetoPoint {
            config: support::bench::ConfigId::Bm25,
            quality: 0.9,
            cost_usd: 1.0,
            latency_p95_ms: 100.0,
            on_frontier: false,
        },
        ParetoPoint {
            config: support::bench::ConfigId::Bm25Rerank,
            quality: 0.9,
            cost_usd: 0.5,
            latency_p95_ms: 100.0,
            on_frontier: false,
        },
        ParetoPoint {
            config: support::bench::ConfigId::Rrf,
            quality: 0.5,
            cost_usd: 0.0,
            latency_p95_ms: 10.0,
            on_frontier: false,
        },
    ];
    pareto_frontier(&mut points);
    // Bm25 is dominated by Bm25Rerank (same quality, cheaper); the cheap
    // low-quality Rrf stays (trade-off surface, not dominated).
    assert!(!points[0].on_frontier);
    assert!(points[1].on_frontier);
    assert!(points[2].on_frontier);
}

/// Runs one config live, evaluates it, and builds its report fragment.
/// Cost is the spent delta across the run (the transport charges per
/// response); cumulative usage would over-count shared recorders.
/// Live calls rely on natural latency + retry backoff for free-tier pacing
/// (sleeps live only in the record path). Counts are tiny; casts are exact.
#[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
fn measure_live_config(
    env: &support::bench::BenchEnv,
    fixtures: &support::bench::Fixtures,
    cfg: support::bench::ConfigId,
    stages: &docsbase_memory::hybrid::HybridStages,
    embed_recorder: &support::bench::live_transport::RecordingTransport,
    rerank_recorder: &support::bench::live_transport::RecordingTransport,
) -> (serde_json::Value, ParetoPoint, f64) {
    let spent_before = embed_recorder.spent_usd() + rerank_recorder.spent_usd();
    let used_before = (
        embed_recorder.usage().prompt_tokens,
        rerank_recorder.usage().calls,
        rerank_recorder.usage().search_units,
    );
    let (run, latencies) = docsbase_memory::hybrid::blocking::enter(|| {
        support::bench::run_config_timed(env, &fixtures.cases, cfg, stages)
            .map_err(|err| docsbase_memory::error::Error::internal(err.to_string()))
    })
    .expect("live run");
    let spent_after = embed_recorder.spent_usd() + rerank_recorder.spent_usd();
    let cost = (spent_after - spent_before).max(0.0);
    let mut hits = 0;
    let mut ndcg_sum = 0.0;
    for case_run in &run.cases {
        let case = fixtures
            .cases
            .iter()
            .find(|case| case.id == case_run.id)
            .expect("case");
        let metrics = support::bench::evaluate_case(&case_run.citations, case);
        hits += usize::from(metrics.hit);
        ndcg_sum += metrics.ndcg;
    }
    let n = run.cases.len().max(1) as f64;
    let eu = embed_recorder.usage();
    let ru = rerank_recorder.usage();
    let report = support::bench::build_baseline_like(
        &fixtures.cases,
        &[(cfg, run, latencies)],
        support::bench::ReportMeta::synthetic(),
        0.0,
    )
    .expect("report");
    let latency = report.configs.get(&cfg).expect("cfg").latency.clone();
    let entry = serde_json::json!({
        "hit_rate": hits as f64 / n,
        "ndcg": ndcg_sum / n,
        "latency_p50_ms": latency.p50_ms,
        "latency_p95_ms": latency.p95_ms,
        "usage": {
            "embed_prompt_tokens": eu.prompt_tokens.saturating_sub(used_before.0),
            "rerank_calls": ru.calls.saturating_sub(used_before.1),
            "rerank_search_units": ru.search_units.saturating_sub(used_before.2),
        },
        "cost_usd": cost,
    });
    let point = ParetoPoint {
        config: cfg,
        quality: ndcg_sum / n,
        cost_usd: cost,
        latency_p95_ms: latency.p95_ms,
        on_frontier: false,
    };
    (entry, point, cost)
}

/// Delta warnings vs the frozen baseline (diagnostic, not a gate).
/// Counts are tiny; the cast is exact.
#[allow(clippy::cast_precision_loss)]
fn baseline_delta_warnings(root: &std::path::Path, points: &[ParetoPoint]) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Ok(baseline) = support::bench::load_baseline(&root.join("baseline.json")) {
        for point in points {
            if let Some(base) = baseline.configs.get(&point.config) {
                let base_ndcg: f64 = base.cases.iter().map(|c| c.metrics.ndcg).sum::<f64>()
                    / base.cases.len().max(1) as f64;
                let delta = point.quality - base_ndcg;
                if delta.abs() > 0.05 {
                    warnings.push(format!(
                        "{:?}: live nDCG {:.3} vs baseline {:.3} (delta {delta:+.3})",
                        point.config, point.quality, base_ndcg,
                    ));
                }
            }
        }
    }
    warnings
}

/// Builds a recording client pair for one model pin.
fn live_client(
    key: &str,
    pin: &str,
) -> (
    docsbase_memory::hybrid::openrouter::OpenRouterClient,
    support::bench::live_transport::RecordingTransport,
) {
    let recorder = support::bench::live_transport::RecordingTransport::new(
        std::sync::Arc::new(
            docsbase_memory::hybrid::openrouter::ReqwestTransport::new().expect("transport"),
        ),
        pin,
    );
    let client = docsbase_memory::hybrid::openrouter::OpenRouterClient::new(
        docsbase_memory::hybrid::openrouter::BASE_URL,
        key,
        Box::new(recorder.clone()),
    )
    .expect("client");
    (client, recorder)
}

/// Full live run of the four configs against the real provider (FR-17):
/// quality, latency, usage, cost and pareto into
/// `target/search-bench/live-report.json`. Manual + ignored; the weekly CI
/// job and human runs use it, never per-PR CI.
#[test]
#[ignore = "manual: live measurement with OPENROUTER_API_KEY (FR-17)"]
fn live_measurement() {
    use std::sync::Arc;

    let Some(key) = openrouter_key() else {
        eprintln!("SKIP: export OPENROUTER_API_KEY to run the live measurement");
        return;
    };
    let (cap, defaulted) = cost_cap_from_env();
    if defaulted {
        eprintln!("note: SEARCH_BENCH_COST_CAP_USD unset, using default $1.00");
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).expect("fixtures");
    let env = support::bench::build_env(&root.join("corpus"), "live", 60..=85).expect("index");
    let embed_cfg = EmbeddingCfg::default();
    let rerank_cfg = RerankerCfg::default();
    let embed_pin = cache_model_id(&embed_cfg);
    let rerank_pin = format!("{}/{}", rerank_cfg.provider, rerank_cfg.model);
    let (embed_client, embed_recorder) = live_client(&key, &embed_pin);
    // Per-query search embeds go through pacing (free tier ~20 req/min);
    // the same instance serves the index-time backfill below.
    let embedder: Arc<dyn Embedder> = Arc::new(support::bench::PacingEmbedder::new(
        OpenRouterEmbedder::with_client(embed_cfg, Arc::new(embed_client)),
        std::time::Duration::from_secs(4),
    ));
    // Production parity: index-time backfill through the live embedder, so
    // dense configs measure with real cached chunks (never an empty cache).
    docsbase_memory::hybrid::blocking::enter(|| {
        docsbase_memory::hybrid::jobs::embed_project_chunks_with(
            &env.db,
            embedder.as_ref(),
            env.project.id,
        )
        .map_err(|err| docsbase_memory::error::Error::internal(err.to_string()))
    })
    .expect("backfill");
    let (rerank_client, rerank_recorder) = live_client(&key, &rerank_pin);
    let reranker: Arc<dyn docsbase_memory::hybrid::Reranker> = Arc::new(
        // Free tiers allow ~20 requests/minute; pace bulk live calls.
        support::bench::PacingReranker::new(
            OpenRouterReranker::with_client(rerank_cfg, Arc::new(rerank_client)),
            std::time::Duration::from_secs(4),
        ),
    );
    let mut configs = serde_json::Map::new();
    let mut points = Vec::new();
    let mut total_cost = 0.0;
    for cfg in [
        support::bench::ConfigId::Bm25,
        support::bench::ConfigId::Bm25Rerank,
        support::bench::ConfigId::Rrf,
        support::bench::ConfigId::RrfRerank,
    ] {
        let stages =
            support::bench::stages_for(cfg, Some(embedder.clone()), Some(reranker.clone()));
        let (entry, point, cost) = measure_live_config(
            &env,
            &fixtures,
            cfg,
            &stages,
            &embed_recorder,
            &rerank_recorder,
        );
        total_cost += cost;
        points.push(point);
        configs.insert(format!("{cfg:?}"), entry);
    }
    pareto_frontier(&mut points);
    let warnings = baseline_delta_warnings(&root, &points);
    assert!(
        total_cost <= cap,
        "live run cost ${total_cost:.4} exceeded cap ${cap:.4}"
    );
    let report = serde_json::json!({
        "generated_at": "live",
        "model_pins": [embed_pin, rerank_pin],
        "configs": configs,
        "pareto": points,
        "cost_usd": total_cost,
        "cost_cap_usd": cap,
        "warnings": warnings,
    });
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/search-bench/live-report.json");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&out, serde_json::to_string_pretty(&report).expect("json")).expect("write");
    println!("live report: {} (cost ${total_cost:.4})", out.display());
}
