#![cfg(feature = "hybrid-remote")]
//! Live hybrid smoke and golden rerank measurement (T5). Ignored by default:
//! needs `OPENROUTER_API_KEY` and real network.
//!
//! ```sh
//! cargo test --locked --features hybrid-remote --test hybrid_live -- --ignored --nocapture
//! ```
//!
//! The measurement is a report, not a gate (NFR-1): it prints hit@3 and
//! nDCG@3 for BM25 vs BM25+rerank over a representative subset of the
//! search-quality golden cases (kept in sync by hand with
//! `tests/search_quality.rs::CASES`).

use std::fs;
use std::path::Path;

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::run_project_index;
use docsbase_memory::hybrid::config::DEFAULT_RERANK_WINDOW;
use docsbase_memory::hybrid::{HybridCfg, HybridStages, Reranker, blocking, from_config};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::ProjectStatus;
use docsbase_memory::vector_cache::Embedder;
use tempfile::TempDir;

const CASES: &[(&str, &str)] = &[
    ("замена", "ru/binary-replace.md"),
    ("заменой", "ru/binary-replace.md"),
    ("каталоги", "ru/system-dirs.md"),
    ("atomic replace", "en/atomic-replace.md"),
    ("system directories", "en/system-dirs.md"),
    ("assessment_plan_id", "api/assessment-plan.md"),
    ("MAX_FRAME_BYTES", "api/frame-limits.md"),
    ("\"plan id\"", "plans/plan-lifecycle.md"),
    ("東京", "cjk/airport.md"),
    ("مطار", "ar/airport.md"),
];

fn api_key() -> String {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => panic!("OPENROUTER_API_KEY is required for the live tests"),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else if entry.path().extension().is_some_and(|ext| ext == "md") {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}

#[test]
#[ignore = "live: needs OPENROUTER_API_KEY and network"]
fn live_smoke_embed_and_rerank() {
    let _key = api_key();
    blocking::enter(|| {
        let embedder = docsbase_memory::hybrid::openrouter::OpenRouterEmbedder::new(
            docsbase_memory::hybrid::EmbeddingCfg::default(),
        )
        .expect("real embedder");
        let texts = vec![
            "Настройка демона: путь к сокету и разрешения.".to_owned(),
            "Banana bread recipe with walnuts.".to_owned(),
            "关于索引与搜索的配置".to_owned(),
        ];
        let vectors = embedder.embed(&texts).expect("live embed");
        assert_eq!(vectors.len(), 3);
        for vector in &vectors {
            assert_eq!(vector.len(), 1024);
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "qwen3 vectors are normalized");
        }

        let reranker = docsbase_memory::hybrid::openrouter::OpenRouterReranker::new(
            docsbase_memory::hybrid::RerankerCfg::default(),
        )
        .expect("real reranker");
        let docs = vec![
            "Настройка демона: путь к сокету, разрешения и состояние сессии.".to_owned(),
            "Banana bread recipe with walnuts.".to_owned(),
            "Индекс и поиск: tantivy, чанки и golden-наборы.".to_owned(),
            "关于索引与搜索的配置".to_owned(),
        ];
        let ranked = reranker
            .rerank("как настроить сокет демона", &docs, 2)
            .expect("live rerank");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, 0, "the relevant document ranks first");
        Ok(())
    })
    .expect("sandboxed hybrid calls");
}

#[test]
#[ignore = "live: needs OPENROUTER_API_KEY and network (embedding pass + rerank calls)"]
fn live_golden_rerank_measurement() {
    let _key = api_key();

    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    let fixtures = Path::new("tests/fixtures/search-quality");
    for class in ["ru", "en", "api", "cjk", "ar", "plans", "docs"] {
        copy_tree(&fixtures.join(class), &root.path().join(class));
    }

    let config = Config {
        hybrid: true,
        hybrid_cfg: HybridCfg::enabled_with_defaults(),
        ..Config::default()
    };

    let mut db = Db::open(cache.path()).expect("db");
    let project = registry::ensure_project(&mut db, &root.path().canonicalize().expect("canon"))
        .expect("project");
    run_project_index(&mut db, cache.path(), &project, &config).expect("live index + embed");
    let mut project = project;
    project.status = ProjectStatus::Indexed;

    let stages = from_config(&config.hybrid_cfg, true).expect("stages");
    assert!(stages.embedder.is_some() && stages.reranker.is_some());

    let mut bm25_hits = 0usize;
    let mut rerank_hits = 0usize;
    let mut bm25_ndcg = 0.0f64;
    let mut rerank_ndcg = 0.0f64;
    for (query, expected) in CASES {
        let args = serde_json::json!({ "query": query, "limit": 3 });
        let bm25 = docsbase_memory::daemon::tools::search_docs_with(
            &HybridStages::default(),
            &db,
            cache.path(),
            &project,
            &args,
            DEFAULT_RERANK_WINDOW,
        )
        .expect("bm25 search");
        let reranked = docsbase_memory::daemon::tools::search_docs_with(
            &stages,
            &db,
            cache.path(),
            &project,
            &args,
            DEFAULT_RERANK_WINDOW,
        )
        .expect("reranked search");

        let rank_of = |value: &serde_json::Value| -> Option<usize> {
            value
                .as_array()
                .expect("array")
                .iter()
                .position(|row| row["path"].as_str() == Some(*expected))
                .map(|index| index + 1)
        };
        let bm25_rank = rank_of(&bm25);
        let rerank_rank = rank_of(&reranked);
        if bm25_rank.is_some() {
            bm25_hits += 1;
        }
        if rerank_rank.is_some() {
            rerank_hits += 1;
        }
        let ndcg = |rank: Option<usize>| -> f64 {
            rank.map_or(0.0, |rank| {
                #[allow(clippy::cast_precision_loss)]
                let r = rank as f64;
                1.0 / (r + 1.0).log2()
            })
        };
        bm25_ndcg += ndcg(bm25_rank);
        rerank_ndcg += ndcg(rerank_rank);
        println!("{query:>22}  bm25={bm25_rank:?}  rerank={rerank_rank:?}  ({expected})");
    }
    #[allow(clippy::cast_precision_loss)]
    let n = CASES.len() as f64;
    let bm25_mean = bm25_ndcg / n;
    let rerank_mean = rerank_ndcg / n;
    println!(
        "hit@3: bm25 {bm25_hits}/{n} -> rerank {rerank_hits}/{n}; nDCG@3: bm25 {bm25_mean:.3} -> rerank {rerank_mean:.3}"
    );
    // Sanity, not a quality gate (NFR-1): the live pipeline must reproduce
    // most golden hits with plain BM25.
    assert!(
        bm25_hits + 2 >= CASES.len(),
        "live BM25 sanity: {bm25_hits}/{} hits",
        CASES.len()
    );
}
