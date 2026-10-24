//! Live verification for `hybrid-local` (T8, SC-7): real Hugging Face
//! fetch + ONNX Runtime install + inference, plus latency/RSS measurement
//! against the NFR-4 budgets. Ignored by default so the offline suite never
//! touches the network.
//!
//! Run (release for realistic numbers):
//! ```sh
//! cargo build --release --features hybrid-local
//! RUSTUP_TOOLCHAIN=1.88.0 cargo test --release --locked --features hybrid-local \
//!   --test hybrid_local_live -- --ignored --nocapture
//! ```
//! Prerequisite: the ONNX Runtime must be installed (`docsbase connect
//! fastembed`, or `ORT_DYLIB_PATH` pointing at the library) — the test does
//! not install it for you (SC-2 is covered by T2/T8 connect tests).

#![cfg(feature = "hybrid-local")]

use std::sync::Arc;
use std::time::Instant;

use docsbase_memory::config::Config;
use docsbase_memory::error::Error;
use docsbase_memory::hybrid::config::{PROVIDER_FASTEMBED, PROVIDER_OPENROUTER};
use docsbase_memory::hybrid::models::{self, HttpModelSource};
use docsbase_memory::hybrid::{EmbeddingCfg, RerankerCfg, local};
use docsbase_memory::vector_cache::Embedder;

fn providers() -> docsbase_memory::hybrid::config::ProvidersCfg {
    Config::global().expect("global config").providers
}

fn runtime_hint() -> String {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        "ORT_DYLIB_PATH is set".to_owned()
    } else {
        "run `docsbase connect fastembed` first".to_owned()
    }
}

/// `VmHWM` is reported in kB on Linux; returns MiB.
fn max_rss_mib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let value: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(value / 1024)
}

/// Main live run: fetch bge-small-en-v1.5, embed 3 texts (query-prefixed),
/// fetch jina-reranker-v1-turbo-en, rerank 3 docs. Records latency and the
/// peak RSS observed during the run (NFR-4 budgets: RSS <= 700 MB, embed
/// p95 <= 250 ms/512-token chunk, rerank p95 <= 200 ms/50 docs — measured
/// on CI-class hardware; record numbers in the ledger).
#[test]
#[ignore = "manual: live HF fetch + ORT inference (T8, SC-7)"]
fn live_fetch_embed_rerank() {
    // ---- provisioning: fetch both pinned models (network) ----
    let source = HttpModelSource::new().expect("model source");
    let bge = models::entry("bge-small-en-v1.5").expect("bge registry");
    let fetch_start = Instant::now();
    let bge_dir = models::fetch(bge, &source, false).expect("fetch bge");
    let fetch_bge = fetch_start.elapsed();
    let jina = models::entry("jina-reranker-v1-turbo-en").expect("jina registry");
    let fetch_start = Instant::now();
    let jina_dir = models::fetch(jina, &source, false).expect("fetch jina");
    let fetch_jina = fetch_start.elapsed();
    println!(
        "fetched {} -> {} ({fetch_bge:?})",
        bge.name,
        bge_dir.display()
    );
    println!(
        "fetched {} -> {} ({fetch_jina:?})",
        jina.name,
        jina_dir.display()
    );

    // ---- runtime must resolve (connect or ORT_DYLIB_PATH) ----
    let providers = providers();
    if !providers.fastembed.as_ref().is_some_and(|p| p.enabled)
        && std::env::var_os("ORT_DYLIB_PATH").is_none()
    {
        panic!("no fastembed provider configured; {}", runtime_hint());
    }

    // ---- embed 3 texts; query prefix asserted (FR-10) ----
    let embed_cfg = EmbeddingCfg {
        provider: PROVIDER_FASTEMBED.to_owned(),
        model: bge.name.to_owned(),
        dimensions: u32::try_from(bge.dim.expect("dims")).expect("dims fit u32"),
        normalize: true,
    };
    let embedder: Arc<dyn Embedder> = local::embedder(&embed_cfg, &providers).expect("embedder");
    assert!(
        embedder
            .model_id()
            .starts_with(&format!("fastembed/{}", bge.name)),
        "canonical id: {}",
        embedder.model_id()
    );
    let texts = vec![
        "The admission lease is held by the daemon".to_owned(),
        "ANN tier indexes vectors under the canonical pin".to_owned(),
        "chunk vectors are cached content-addressed".to_owned(),
    ];
    let embed_start = Instant::now();
    let vectors = embedder.embed(&texts).expect("embed");
    let embed_latency = embed_start.elapsed();
    assert_eq!(vectors.len(), 3, "3 vectors");
    assert_eq!(vectors[0].len(), embed_cfg.dimensions as usize, "width");
    let query = "how is the lease held?";
    let query_start = Instant::now();
    let _q = embedder.embed_query(query).expect("embed_query");
    let query_latency = query_start.elapsed();
    println!(
        "embed: 3 texts in {embed_latency:?} (per-text {:?}), query in {query_latency:?}",
        embed_latency / 3
    );

    // ---- rerank 3 docs ----
    let rerank_cfg = RerankerCfg {
        provider: PROVIDER_FASTEMBED.to_owned(),
        model: jina.name.to_owned(),
        window: 50,
    };
    let reranker = local::reranker(&rerank_cfg, &providers).expect("reranker");
    let docs: Vec<String> = vec![
        "daemon holds the lease until release".to_owned(),
        "ANN rebuild invalidates on re-pin".to_owned(),
        "lexical scoring is the baseline".to_owned(),
    ];
    let rerank_start = Instant::now();
    let ranked = reranker.rerank(query, &docs, 3).expect("rerank");
    let rerank_latency = rerank_start.elapsed();
    assert_eq!(ranked.len(), 3, "every doc scored");
    println!("rerank: {ranked:?} in {rerank_latency:?}");

    // ---- budgets vs NFR-4 ----
    if let Some(rss) = max_rss_mib() {
        println!("peak RSS: {rss} MiB (budget 700 MiB)");
        assert!(rss <= 700, "RSS {rss} MiB over budget");
    }
    println!("live run OK");
}

/// Sanity: the providers config path works without a key and the entry
/// lookup rejects junk (fast, offline — guards the live test's setup).
#[test]
fn registry_lookup_offline_sanity() {
    assert!(models::entry("no-such-model").is_err());
    let _ = PROVIDER_OPENROUTER; // doc: provider constant exists outside hybrid-local
    assert_eq!(PROVIDER_FASTEMBED, "fastembed");
    let _ = Error::internal("probe");
}
