//! Offline preparation for the explicitly scoped #71 inference experiment.
#[path = "support/representation.rs"]
mod representation;
mod support;

use std::path::Path;
use support::bench::{QueryCase, build_env, corpus_hash};

fn cases() -> Vec<QueryCase> {
    serde_json::from_value(serde_json::json!([{
        "id":"pilot", "query":"shared content", "class":"probe", "k":3,
        "expected":[{"path":"a.md","grade":2}], "absent":[],
        "source":"synthetic", "notes":"representation preflight", "reviewed_by":[]
    }]))
    .unwrap()
}

fn corpus() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("a.md"),
        "# Alpha\n\n## Details\n\nshared content\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("b.md"),
        "# Beta\n\n## Details\n\nshared content\n",
    )
    .unwrap();
    root
}

#[test]
fn preflight_preserves_duplicate_locations_and_original_citations() {
    let root = corpus();
    let env = build_env(root.path(), "representation", 2..=2).unwrap();
    let plan = representation::prepare(
        &env,
        &cases(),
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap();
    assert_eq!(plan["phase"], "offline-preflight");
    assert_eq!(plan["provider_requests"], 0);
    assert!(plan["quality"].is_null());
    let shared: Vec<_> = plan["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["raw_input"]
                .as_str()
                .unwrap()
                .contains("shared content")
        })
        .collect();
    assert_eq!(shared.len(), 2);
    assert_eq!(shared[0]["raw_key"], shared[1]["raw_key"]);
    assert_ne!(shared[0]["context_key"], shared[1]["context_key"]);
    assert_eq!(shared[0]["path"], "a.md");
    assert_eq!(shared[0]["heading_path"], "Alpha > Details");
    assert_eq!(shared[0]["lines"], serde_json::json!([3, 5]));
    assert_eq!(
        shared[0]["context_input"],
        format!(
            "Path: a.md\nHeading: Alpha > Details\n\n{}",
            shared[0]["raw_input"].as_str().unwrap()
        )
    );
    assert_eq!(
        plan["queries"][0]["case"],
        serde_json::to_value(&cases()[0]).unwrap()
    );
    assert_eq!(plan["matrix"].as_array().unwrap().len(), 4);
    assert!(
        plan["stats"]["unique_raw_items"].as_u64().unwrap()
            < plan["stats"]["chunks"].as_u64().unwrap()
    );
}

#[test]
fn preflight_is_stable_and_relocation_keeps_raw_and_query_identity() {
    let root = corpus();
    let prepare = |path: &Path| {
        let env = build_env(path, "representation", 2..=2).unwrap();
        representation::prepare(&env, &cases(), &corpus_hash(path).unwrap(), "test-revision")
            .unwrap()
    };
    let first = prepare(root.path());
    assert_eq!(first, prepare(root.path()));
    std::fs::rename(root.path().join("b.md"), root.path().join("c.md")).unwrap();
    let moved = prepare(root.path());
    assert_eq!(first["queries"], moved["queries"]);
    assert_eq!(first["raw_document_set_key"], moved["raw_document_set_key"]);
    assert_ne!(
        first["context_document_set_key"],
        moved["context_document_set_key"]
    );
    assert_ne!(first["chunk_set_key"], moved["chunk_set_key"]);
}

#[test]
fn preflight_rejects_wrong_corpus_and_never_clobbers_existing_output() {
    let root = corpus();
    let env = build_env(root.path(), "representation", 2..=2).unwrap();
    let err = representation::prepare(&env, &cases(), "wrong-corpus", "test-revision").unwrap_err();
    assert!(err.to_string().contains("corpus hash"), "{err}");
    assert!(
        representation::outside_source_output(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forbidden-output.json")
        )
        .is_err()
    );
    let target = root.path().join("plan.json");
    std::fs::write(&target, b"existing artifact").unwrap();
    assert!(representation::write_new(&target, &serde_json::json!({"plan":1})).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"existing artifact");
}

#[test]
#[ignore = "manual offline #71 input materialization; requires DOCSBASE_REPRESENTATION_OUT"]
fn prepare_frozen_representation_experiment() {
    let output = std::env::var_os("DOCSBASE_REPRESENTATION_OUT").expect("set output path");
    let output = representation::outside_source_output(Path::new(&output)).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).unwrap();
    let env = build_env(&root.join("corpus"), "representation-frozen", 81..=81).unwrap();
    let revision = representation::clean_revision().unwrap();
    let mut plan =
        representation::prepare(&env, &fixtures.cases, &fixtures.corpus_hash, &revision).unwrap();
    for id in ["hard-08", "hn-22"] {
        assert!(
            fixtures.cases.iter().any(|case| case.id == id),
            "missing pilot case {id}"
        );
    }
    plan["pilot_query_ids"] = serde_json::json!(["hard-08", "hn-22"]);
    representation::write_new(Path::new(&output), &plan).unwrap();
    println!("offline preflight: {}", Path::new(&output).display());
    println!("{}", plan["stats"]);
}

fn census_vectors(
    env: &support::bench::BenchEnv,
    cases: &[QueryCase],
) -> support::bench::VectorFixture {
    support::bench::VectorFixture {
        model_pin: "test/census@d4@l2".to_owned(),
        dim: 4,
        corpus_hash: corpus_hash(env.root.path()).unwrap(),
        queries_hash: "synthetic".to_owned(),
        vectors: env
            .chunk_hashes
            .iter()
            .map(|(_, hash)| (hash.clone(), vec![1.0, 0.0, 0.0, 0.0]))
            .collect(),
        query_vectors: Some(
            cases
                .iter()
                .map(|case| {
                    (
                        blake3::hash(case.query.as_bytes()).to_hex().to_string(),
                        vec![1.0, 0.0, 0.0, 0.0],
                    )
                })
                .collect(),
        ),
    }
}

#[test]
fn candidate_census_preserves_occurrences_and_matches_original_k_capture() {
    use docsbase_memory::hybrid::dense::DenseCache;
    use std::sync::{Arc, Mutex};
    let root = corpus();
    let env = build_env(root.path(), "census", 2..=2).unwrap();
    let cases = cases();
    let vectors = census_vectors(&env, &cases);
    let report = representation::collect_candidates(
        &env,
        &cases,
        &vectors,
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap();
    assert_eq!(report["phase"], "offline-candidate-census");
    assert_eq!(report["provider_requests"], 0);
    assert!(report["quality"].is_null());
    assert_eq!(report["embedding_pin"], vectors.model_pin);
    assert_eq!(
        report["queries"][0]["case"],
        serde_json::to_value(&cases[0]).unwrap()
    );
    let pools = report["candidate_pools"].as_array().unwrap();
    assert_eq!(pools.len(), 2);
    for (pool, cfg) in pools.iter().zip([
        support::bench::ConfigId::Bm25Rerank,
        support::bench::ConfigId::RrfRerank,
    ]) {
        let candidates = pool["candidates"].as_array().unwrap();
        let shared: Vec<_> = candidates
            .iter()
            .filter(|c| c["raw_input"].as_str().unwrap().contains("shared content"))
            .collect();
        assert_eq!(shared.len(), 2);
        assert_ne!(shared[0]["occurrence_key"], shared[1]["occurrence_key"]);
        assert_eq!(shared[0]["raw_key"], shared[1]["raw_key"]);
        assert_ne!(shared[0]["context_key"], shared[1]["context_key"]);
        let spy = Arc::new(support::bench::CollectingReranker {
            seen: Mutex::new(Vec::new()),
        });
        let embedder = Arc::new(support::bench::ReplayEmbedder {
            pin: vectors.model_pin.clone(),
            fixture: Arc::new(vectors.clone()),
        });
        let stages = support::bench::stages_for(cfg, Some(embedder), Some(spy.clone()));
        docsbase_memory::daemon::tools::search_docs_with(
            &stages,
            &DenseCache::new(),
            &env.db,
            env.cache.path(),
            &env.project,
            &serde_json::json!({"query":cases[0].query,"limit":cases[0].k}),
            support::bench::RERANK_WINDOW,
        )
        .unwrap();
        let expected = spy.seen.lock().unwrap()[0].1.clone();
        assert_eq!(
            candidates
                .iter()
                .map(|c| c["raw_input"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            expected
        );
    }
    let again = representation::collect_candidates(
        &env,
        &cases,
        &vectors,
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap();
    assert_eq!(report, again);
}

#[test]
fn candidate_census_fails_closed_and_keeps_empty_lexical_observations() {
    let root = corpus();
    let env = build_env(root.path(), "census-errors", 2..=2).unwrap();
    let mut cases = cases();
    cases[0].query = "nonexistentxyz".to_owned();
    let vectors = census_vectors(&env, &cases);
    let collect = |vectors: &support::bench::VectorFixture, cases: &[QueryCase]| {
        representation::collect_candidates(
            &env,
            cases,
            vectors,
            &corpus_hash(root.path()).unwrap(),
            "test-revision",
        )
    };
    let report = collect(&vectors, &cases).unwrap();
    assert_eq!(
        report["candidate_pools"][0]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(report["candidate_pools"][0]["rerank_call"], false);
    assert!(
        !report["candidate_pools"][1]["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut bad = vectors.clone();
    bad.query_vectors.as_mut().unwrap().clear();
    assert!(collect(&bad, &cases).is_err());
    let mut bad = vectors.clone();
    bad.vectors.remove(&env.chunk_hashes[0].1);
    assert!(collect(&bad, &cases).is_err());
    let mut bad = vectors.clone();
    bad.dim = 2;
    assert!(collect(&bad, &cases).is_err());
    cases[0].k = 51;
    assert!(collect(&vectors, &cases).is_err());
}

#[test]
fn frozen_candidate_census_matches_every_recorded_nonempty_pool() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).unwrap();
    let baseline = support::bench::load_baseline(&root.join("baseline.json")).unwrap();
    let replay = support::bench::replay_stages(&root, &baseline).unwrap();
    let env = build_env(&root.join("corpus"), "census-frozen", 81..=81).unwrap();
    let report = representation::collect_candidates(
        &env,
        &fixtures.cases,
        replay.vectors.as_ref().unwrap(),
        &fixtures.corpus_hash,
        "test-revision",
    )
    .unwrap();
    verify_frozen_pools(&root, &report);
}

fn verify_frozen_pools(root: &Path, report: &serde_json::Value) {
    let pin = report["rerank_pin"].as_str().unwrap();
    let recorded = support::bench::RerankFixture::load(
        &root
            .join("rerank")
            .join(format!("{}.json", support::bench::pin_slug(pin))),
    )
    .unwrap();
    let mut keys = std::collections::BTreeSet::new();
    let pools = report["candidate_pools"].as_array().unwrap();
    assert_eq!(pools.len(), 386);
    for pool in pools {
        let docs: Vec<_> = pool["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["raw_input"].as_str().unwrap().to_owned())
            .collect();
        if docs.is_empty() {
            assert_eq!(pool["rerank_call"], false);
            continue;
        }
        let fixture = recorded
            .pool(pool["query"].as_str().unwrap(), &docs)
            .unwrap();
        assert_eq!(fixture.top_n, docs.len());
        keys.insert(pool["raw_pool_key"].as_str().unwrap().to_owned());
    }
    assert_eq!(keys.len(), recorded.pools.len());
    assert_eq!(
        report["candidate_source"]["vector_format"],
        "v1-legacy-role-unknown"
    );
}

#[test]
#[ignore = "manual offline #71 candidate census; requires DOCSBASE_REPRESENTATION_CENSUS_OUT"]
fn collect_frozen_representation_candidates() {
    let out = std::env::var_os("DOCSBASE_REPRESENTATION_CENSUS_OUT").expect("set census output");
    let out = representation::outside_source_output(Path::new(&out)).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).unwrap();
    let baseline = support::bench::load_baseline(&root.join("baseline.json")).unwrap();
    let replay = support::bench::replay_stages(&root, &baseline).unwrap();
    let env = build_env(&root.join("corpus"), "census-export", 81..=81).unwrap();
    let report = representation::collect_candidates(
        &env,
        &fixtures.cases,
        replay.vectors.as_ref().unwrap(),
        &fixtures.corpus_hash,
        &representation::clean_revision().unwrap(),
    )
    .unwrap();
    verify_frozen_pools(&root, &report);
    representation::write_new(&out, &report).unwrap();
    println!("candidate census: {}", out.display());
    println!("{}", report["candidate_stats"]);
}
