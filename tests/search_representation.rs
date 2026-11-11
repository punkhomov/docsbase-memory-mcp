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

#[derive(Default)]
struct MatrixEmbedder {
    pin: Option<String>,
    calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}
impl docsbase_memory::vector_cache::Embedder for MatrixEmbedder {
    fn model_id(&self) -> &str {
        self.pin.as_deref().unwrap_or("test/matrix@d4@l2")
    }
    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        self.calls
            .lock()
            .unwrap()
            .push(("document".into(), texts.to_vec()));
        Ok(texts
            .iter()
            .map(|text| {
                let context = text.starts_with("Path:");
                let preferred = if context {
                    text.contains("alpha body")
                } else {
                    text.contains("beta body")
                };
                if preferred {
                    vec![1.0, 0.0, 0.0, 0.0]
                } else if text.contains("neutral body") {
                    vec![0.0, 1.0, 0.0, 0.0]
                } else {
                    vec![-1.0, 0.0, 0.0, 0.0]
                }
            })
            .collect())
    }
    fn embed_query(&self, text: &str) -> docsbase_memory::error::Result<Vec<f32>> {
        self.calls
            .lock()
            .unwrap()
            .push(("query".into(), vec![text.into()]));
        Ok(vec![1.0, 0.0, 0.0, 0.0])
    }
}
#[derive(Default)]
struct MatrixReranker {
    calls: std::sync::Mutex<Vec<(String, Vec<String>, usize)>>,
}
impl docsbase_memory::hybrid::Reranker for MatrixReranker {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        self.calls
            .lock()
            .unwrap()
            .push((query.into(), docs.to_vec(), top_n));
        let context = docs[0].starts_with("Path:");
        let mut scores: Vec<_> = docs
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let preferred = if context {
                    text.contains("beta body")
                } else {
                    text.contains("alpha body")
                };
                (i, if preferred { 2.0 } else { 1.0 })
            })
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        Ok(scores)
    }
}
fn matrix_corpus() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("a.md"),
        "# Alpha\n\nshared content alpha body\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("b.md"),
        "# Beta\n\nshared content beta body\n",
    )
    .unwrap();
    root
}
#[test]
#[allow(clippy::too_many_lines)] // one factorial dispatch/identity/citation proof
fn matrix_dispatches_exact_recipes_and_reuses_roles_and_warm_pools() {
    use std::sync::Arc;
    let root = matrix_corpus();
    // Three dense ranks avoid the symmetric two-document RRF tie.
    std::fs::write(root.path().join("c.md"), "# Neutral\n\nneutral body\n").unwrap();
    let env = build_env(root.path(), "matrix", 3..=3).unwrap();
    let embedder = Arc::new(MatrixEmbedder::default());
    let reranker = Arc::new(MatrixReranker::default());
    let mut queries = cases();
    let mut second = queries[0].clone();
    second.id = "same-query".into();
    queries.push(second);
    let mut same_text = queries[0].clone();
    same_text.id = "same-text-two-roles".into();
    same_text.query = env
        .chunk_texts
        .iter()
        .find(|text| text.contains("alpha body"))
        .unwrap()
        .clone();
    queries.push(same_text);
    let report = representation::run_matrix(
        &env,
        &queries,
        &representation::MatrixStages {
            embedder: embedder.clone(),
            reranker: reranker.clone(),
            rerank_pin: "test/rerank".into(),
        },
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap();
    assert_eq!(report["runs"].as_array().unwrap().len(), 24);
    assert!(report["quality"].is_null());
    assert!(report["wire_requests"].is_null());
    assert_eq!(report["method_calls"]["document"], 2);
    assert_eq!(report["method_calls"]["query"], 2);
    let calls = embedder.calls.lock().unwrap();
    let chunks = report["chunks"].as_array().unwrap();
    for (i, field) in ["raw_input", "context_input"].iter().enumerate() {
        assert_eq!(calls[i].0, "document");
        let expected: std::collections::BTreeSet<_> = chunks
            .iter()
            .map(|row| row[*field].as_str().unwrap())
            .collect();
        let actual: std::collections::BTreeSet<_> = calls[i].1.iter().map(String::as_str).collect();
        assert_eq!(expected, actual);
    }
    assert_eq!(calls.len(), 4);
    let expected_queries: std::collections::BTreeSet<_> =
        queries.iter().map(|c| c.query.clone()).collect();
    let actual_queries: std::collections::BTreeSet<_> = calls[2..]
        .iter()
        .map(|(role, texts)| {
            assert_eq!(role, "query");
            assert_eq!(texts.len(), 1);
            texts[0].clone()
        })
        .collect();
    assert_eq!(actual_queries, expected_queries);
    assert!(calls[0].1.contains(&queries[2].query));
    let recorded_reranks = reranker.calls.lock().unwrap().clone();
    for run in report["runs"].as_array().unwrap() {
        assert_eq!(run["warm_equal"], true);
        let context = run["rerank_recipe"] == "path-heading-v1";
        let expected_path = if context { "b.md" } else { "a.md" };
        assert_eq!(run["citations"][0]["path"], expected_path);
        let returned = run["citations"].as_array().unwrap();
        assert_eq!(
            returned.len(),
            run["pool"]["scores"].as_array().unwrap().len()
        );
        for (rank, citation) in returned.iter().enumerate() {
            let row = chunks
                .iter()
                .find(|row| row["path"] == citation["path"] && row["lines"] == citation["lines"])
                .unwrap();
            assert_eq!(row["heading_path"], citation["heading_path"]);
            let input_index =
                usize::try_from(run["pool"]["scores"][rank][0].as_u64().unwrap()).unwrap();
            assert_eq!(
                run["pool"]["inputs"][input_index],
                row[if context {
                    "context_input"
                } else {
                    "raw_input"
                }]
            );
        }
        let inputs = run["pool"]["inputs"].as_array().unwrap();
        assert!(!inputs.is_empty());
        let strings: Vec<_> = inputs
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect();
        assert!(recorded_reranks.iter().any(|(query, docs, top_n)| {
            query == run["pool"]["query"].as_str().unwrap()
                && docs == &strings
                && *top_n == strings.len()
        }));
        assert!(
            inputs
                .iter()
                .all(|s| s.as_str().unwrap().starts_with("Path:") == context)
        );
    }
    let keys: std::collections::BTreeSet<_> = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["pool"]["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), reranker.calls.lock().unwrap().len());
    assert_eq!(
        usize::try_from(report["method_calls"]["rerank"].as_u64().unwrap()).unwrap(),
        keys.len()
    );
    let rrf_pools: Vec<_> = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| {
            run["source"] == "RrfRerank"
                && run["rerank_recipe"] == "raw-body-v1"
                && run["case_id"] == "pilot"
        })
        .collect();
    assert_eq!(rrf_pools.len(), 2);
    assert_ne!(
        rrf_pools[0]["pool"]["inputs"],
        rrf_pools[1]["pool"]["inputs"]
    );
    let namespaces: std::collections::BTreeSet<_> = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cache_namespace"].as_str().unwrap())
        .collect();
    assert_eq!(namespaces.len(), 2);
}
#[test]
fn matrix_rejects_ambiguous_context_before_any_stage_method() {
    use std::sync::Arc;
    let root = corpus();
    let env = build_env(root.path(), "matrix-ambiguous", 2..=2).unwrap();
    let embedder = Arc::new(MatrixEmbedder::default());
    let reranker = Arc::new(MatrixReranker::default());
    let err = representation::run_matrix(
        &env,
        &cases(),
        &representation::MatrixStages {
            embedder: embedder.clone(),
            reranker: reranker.clone(),
            rerank_pin: "test/rerank".into(),
        },
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("ambiguous body/context"),
        "{err:#}"
    );
    assert!(embedder.calls.lock().unwrap().is_empty());
    assert!(reranker.calls.lock().unwrap().is_empty());
}

#[derive(Clone, Copy, Debug)]
enum MatrixFault {
    DocumentCount,
    DocumentDim,
    DocumentNan,
    DocumentError,
    QueryDim,
    QueryNan,
    QueryError,
    PartialScores,
    DuplicateScores,
    OutsideScores,
    NanScores,
    RerankError,
}
struct BrokenMatrix(MatrixFault);
impl docsbase_memory::vector_cache::Embedder for BrokenMatrix {
    fn model_id(&self) -> &'static str {
        "test/matrix@d4@l2"
    }
    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        let mut values = vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()];
        match self.0 {
            MatrixFault::DocumentCount => {
                values.pop();
            }
            MatrixFault::DocumentDim => values[0].pop().map(|_| ()).unwrap(),
            MatrixFault::DocumentNan => values[0][0] = f32::NAN,
            MatrixFault::DocumentError => {
                return Err(docsbase_memory::error::Error::internal("document failure"));
            }
            _ => {}
        }
        Ok(values)
    }
    fn embed_query(&self, _: &str) -> docsbase_memory::error::Result<Vec<f32>> {
        match self.0 {
            MatrixFault::QueryDim => Ok(vec![1.0]),
            MatrixFault::QueryNan => Ok(vec![f32::NAN, 0.0, 0.0, 0.0]),
            MatrixFault::QueryError => {
                Err(docsbase_memory::error::Error::internal("query failure"))
            }
            _ => Ok(vec![1.0, 0.0, 0.0, 0.0]),
        }
    }
}
impl docsbase_memory::hybrid::Reranker for BrokenMatrix {
    fn rerank(
        &self,
        _: &str,
        docs: &[String],
        _: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        let mut scores: Vec<_> = (0..docs.len()).map(|i| (i, 1.0)).collect();
        match self.0 {
            MatrixFault::PartialScores => {
                scores.pop();
            }
            MatrixFault::DuplicateScores => scores[1].0 = scores[0].0,
            MatrixFault::OutsideScores => scores[0].0 = docs.len(),
            MatrixFault::NanScores => scores[0].1 = f32::NAN,
            MatrixFault::RerankError => {
                return Err(docsbase_memory::error::Error::internal("rerank failure"));
            }
            _ => {}
        }
        Ok(scores)
    }
}
#[test]
fn matrix_fails_closed_on_invalid_vectors_queries_and_rerank_coverage() {
    use std::sync::Arc;
    let root = matrix_corpus();
    for fault in [
        MatrixFault::DocumentCount,
        MatrixFault::DocumentDim,
        MatrixFault::DocumentNan,
        MatrixFault::DocumentError,
        MatrixFault::QueryDim,
        MatrixFault::QueryNan,
        MatrixFault::QueryError,
        MatrixFault::PartialScores,
        MatrixFault::DuplicateScores,
        MatrixFault::OutsideScores,
        MatrixFault::NanScores,
        MatrixFault::RerankError,
    ] {
        let env = build_env(root.path(), "matrix-errors", 2..=2).unwrap();
        let stage = Arc::new(BrokenMatrix(fault));
        let error = representation::run_matrix(
            &env,
            &cases(),
            &representation::MatrixStages {
                embedder: stage.clone(),
                reranker: stage,
                rerank_pin: "test/rerank".into(),
            },
            &corpus_hash(root.path()).unwrap(),
            "test-revision",
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("matrix") || message.contains("failure"),
            "{fault:?}: {message}"
        );
        if matches!(
            fault,
            MatrixFault::DocumentCount
                | MatrixFault::DocumentDim
                | MatrixFault::DocumentNan
                | MatrixFault::DocumentError
                | MatrixFault::QueryDim
                | MatrixFault::QueryNan
                | MatrixFault::QueryError
        ) {
            assert!(
                !env.cache.path().join("vectors.db").exists(),
                "{fault:?}: vector publication before validation"
            );
        }
    }
}

#[test]
fn matrix_frozen_pilot_runs_all_pairs_without_inference() {
    use std::sync::Arc;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let fixtures = support::bench::Fixtures::load(&root).unwrap();
    let cases: Vec<_> = fixtures
        .cases
        .iter()
        .filter(|c| ["hard-08", "hn-22"].contains(&c.id.as_str()))
        .cloned()
        .collect();
    assert_eq!(cases.len(), 2);
    let env = build_env(&root.join("corpus"), "matrix-frozen-pilot", 81..=81).unwrap();
    let before = support::bench::corpus_hash(env.root.path()).unwrap();
    let embedder = Arc::new(MatrixEmbedder::default());
    let report = representation::run_matrix(
        &env,
        &cases,
        &representation::MatrixStages {
            embedder: embedder.clone(),
            reranker: Arc::new(MatrixReranker::default()),
            rerank_pin: "test/rerank".into(),
        },
        &fixtures.corpus_hash,
        "test-revision",
    )
    .unwrap();
    assert_eq!(report["stats"]["chunks"], 327);
    assert_eq!(report["runs"].as_array().unwrap().len(), 16);
    assert_eq!(report["method_calls"]["query"], 2);
    assert!(report["quality"].is_null());
    assert_eq!(
        support::bench::corpus_hash(env.root.path()).unwrap(),
        before
    );
    assert_eq!(
        report["queries"][0]["case"],
        serde_json::to_value(&cases[0]).unwrap()
    );
    assert_eq!(
        report["queries"][1]["case"],
        serde_json::to_value(&cases[1]).unwrap()
    );
}

#[test]
fn matrix_keeps_empty_lexical_pools_and_rejects_invalid_k_before_stages() {
    use std::sync::Arc;
    let root = matrix_corpus();
    let env = build_env(root.path(), "matrix-empty", 2..=2).unwrap();
    let mut queries = cases();
    queries[0].query = "nonexistentxyz".into();
    let embedder = Arc::new(MatrixEmbedder::default());
    let reranker = Arc::new(MatrixReranker::default());
    let stages = representation::MatrixStages {
        embedder: embedder.clone(),
        reranker: reranker.clone(),
        rerank_pin: "test/rerank".into(),
    };
    queries[0].k = 51;
    assert!(
        representation::run_matrix(
            &env,
            &queries,
            &stages,
            &corpus_hash(root.path()).unwrap(),
            "test-revision"
        )
        .is_err()
    );
    assert!(embedder.calls.lock().unwrap().is_empty());
    assert!(reranker.calls.lock().unwrap().is_empty());
    queries[0].k = 3;
    let report = representation::run_matrix(
        &env,
        &queries,
        &stages,
        &corpus_hash(root.path()).unwrap(),
        "test-revision",
    )
    .unwrap();
    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 8);
    for run in runs {
        if run["source"] == "Bm25Rerank" {
            assert!(run["pool"].is_null());
            assert!(run["citations"].as_array().unwrap().is_empty());
        } else {
            assert!(!run["pool"]["inputs"].as_array().unwrap().is_empty());
        }
        assert_eq!(run["warm_equal"], true);
    }
}

#[test]
fn matrix_rejects_invalid_model_pins_before_stage_methods() {
    use std::sync::Arc;
    let root = matrix_corpus();
    let env = build_env(root.path(), "matrix-pins", 2..=2).unwrap();
    let hash = corpus_hash(root.path()).unwrap();
    for pin in [
        "garbage",
        "test/embed@d4@l2",
        "/model",
        "provider/",
        " /model",
        "provider/ ",
    ] {
        let embedder = Arc::new(MatrixEmbedder::default());
        let reranker = Arc::new(MatrixReranker::default());
        let stages = representation::MatrixStages {
            embedder: embedder.clone(),
            reranker: reranker.clone(),
            rerank_pin: pin.into(),
        };
        assert!(
            representation::run_matrix(&env, &cases(), &stages, &hash, "test-revision").is_err(),
            "{pin}"
        );
        assert!(embedder.calls.lock().unwrap().is_empty());
        assert!(reranker.calls.lock().unwrap().is_empty());
    }
    for pin in [
        "garbage",
        "test/reranker",
        "/model@d4@l2",
        "provider/@d4@l2",
        "test/model@d0@l2",
    ] {
        let embedder = Arc::new(MatrixEmbedder {
            pin: Some(pin.into()),
            ..Default::default()
        });
        let reranker = Arc::new(MatrixReranker::default());
        let stages = representation::MatrixStages {
            embedder: embedder.clone(),
            reranker: reranker.clone(),
            rerank_pin: "test/rerank".into(),
        };
        assert!(
            representation::run_matrix(&env, &cases(), &stages, &hash, "test-revision").is_err(),
            "{pin}"
        );
        assert!(embedder.calls.lock().unwrap().is_empty());
        assert!(reranker.calls.lock().unwrap().is_empty());
    }
}
