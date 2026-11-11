//! Manual external evaluation; regular tests use only tiny local BEIR inputs.
mod support;
use docsbase_memory::index::QueryMode;
use std::path::Path;
use support::external::{ExternalOptions, evaluate_directory};

fn tiny_dataset() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("qrels")).unwrap();
    std::fs::write(
        dir.path().join("dataset.json"),
        serde_json::to_vec(&serde_json::json!({
            "dataset":"tiny", "dataset_version":"v1", "source_url":"https://example.org/tiny",
            "downloaded_at":"2026-11-08T00:00:00Z", "license_note":"Synthetic test data, MIT"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("corpus.jsonl"),
        [
            serde_json::json!({"_id":"z","title":"Granite","text":"granite mineral"}),
            serde_json::json!({"_id":"../../../escape","title":"Quartz","text":"quartz crystal"}),
            serde_json::json!({"_id":"C:\\odd/../file","title":"Quartz","text":"quartz mineral"}),
        ]
        .map(|v| v.to_string())
        .join("\n"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("queries.jsonl"),
        [
            serde_json::json!({"_id":"q2","text":"granite"}),
            serde_json::json!({"_id":"unused","text":"not selected"}),
            serde_json::json!({"_id":"zero","text":"unanswerable"}),
            serde_json::json!({"_id":"q1","text":"quartz"}),
        ]
        .map(|v| v.to_string())
        .join("\n"),
    )
    .unwrap();
    std::fs::write(dir.path().join("qrels/test.tsv"),
        "query-id\tcorpus-id\tscore\nq2\tz\t1\nq1\t../../../escape\t2\nq1\tC:\\odd/../file\t1\nq1\tz\t0\nzero\tz\t0\n").unwrap();
    dir
}

fn run(dir: &Path) -> anyhow::Result<serde_json::Value> {
    evaluate_directory(dir, &ExternalOptions::default()).map(|r| serde_json::to_value(r).unwrap())
}

#[test]
fn beir_runs_real_pipeline_with_safe_ids_and_split_inventory() {
    let dir = tiny_dataset();
    let before = std::fs::read(dir.path().join("corpus.jsonl")).unwrap();
    let first = run(dir.path()).unwrap();
    let second = run(dir.path()).unwrap();
    assert_eq!(first["quality"], second["quality"]);
    assert_eq!(first["track"], "external");
    assert_eq!(first["advisory"], true);
    let p = &first["provenance"];
    assert_eq!(p["document_count"], 3);
    assert_eq!(p["query_count"], 4);
    assert_eq!(p["evaluated_query_ids"], serde_json::json!(["q1", "q2"]));
    assert_eq!(
        p["excluded_query_ids"],
        serde_json::json!(["unused", "zero"])
    );
    assert_eq!(p["zero_only_query_ids"], serde_json::json!(["zero"]));
    for name in [
        "corpus_hash",
        "queries_hash",
        "qrels_hash",
        "derived_corpus_hash",
        "harness_source_hash",
    ] {
        assert_eq!(p[name].as_str().unwrap().len(), 64, "{name}");
    }
    assert_eq!(first["cost"]["spent_usd"], 0.0);
    assert_eq!(first["cost"]["provider_requests"], 0);
    let cases = first["quality"]["Bm25"]["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    assert!(cases.iter().all(|c| c["metrics"]["hit"] == true));
    assert_eq!(cases[0]["metrics"]["recall"], 1.0);
    let ids: Vec<_> = cases[0]["ranks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["document_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"../../../escape"));
    assert!(ids.contains(&"C:\\odd/../file"));
    assert_eq!(
        before,
        std::fs::read(dir.path().join("corpus.jsonl")).unwrap()
    );
}

#[test]
fn beir_rejects_invalid_references_duplicate_ids_and_malformed_input() {
    for (file, suffix) in [
        ("corpus.jsonl", "\n{\"_id\":\"z\",\"text\":\"duplicate\"}"),
        ("queries.jsonl", "\n{\"_id\":\"q1\",\"text\":\"duplicate\"}"),
        ("corpus.jsonl", "\n{\"_id\":\"\",\"text\":\"empty id\"}"),
        ("queries.jsonl", "\n{\"_id\":\"bad\",\"text\":\"  \"}"),
        ("qrels/test.tsv", "missing\tz\t1\n"),
        ("qrels/test.tsv", "q1\tmissing\t1\n"),
        ("qrels/test.tsv", "q1\tz\t0\n"),
        ("qrels/test.tsv", "q2\t../../../escape\t-1\n"),
        ("qrels/test.tsv", "bad row\n"),
    ] {
        let dir = tiny_dataset();
        let p = dir.path().join(file);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.extend_from_slice(suffix.as_bytes());
        std::fs::write(p, bytes).unwrap();
        assert!(run(dir.path()).is_err(), "accepted {file}: {suffix}");
    }
    for file in [
        "corpus.jsonl",
        "queries.jsonl",
        "dataset.json",
        "qrels/test.tsv",
    ] {
        let dir = tiny_dataset();
        std::fs::write(dir.path().join(file), "").unwrap();
        assert!(run(dir.path()).is_err(), "accepted empty {file}");
    }
    let dir = tiny_dataset();
    std::fs::write(
        dir.path().join("qrels/test.tsv"),
        "query-id\tcorpus-id\tscore\nq1\tz\t0\n",
    )
    .unwrap();
    assert!(run(dir.path()).is_err(), "no positives cannot be evaluated");
}

#[test]
fn beir_provenance_tracks_each_raw_input_and_options_fail_closed() {
    let dir = tiny_dataset();
    let original = run(dir.path()).unwrap();
    let p = dir.path().join("queries.jsonl");
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push('\n');
    std::fs::write(&p, text).unwrap();
    let changed = run(dir.path()).unwrap();
    assert_ne!(
        original["provenance"]["queries_hash"],
        changed["provenance"]["queries_hash"]
    );
    assert_eq!(original["quality"], changed["quality"]);
    for options in [
        ExternalOptions {
            split: "../test".into(),
            ..ExternalOptions::default()
        },
        ExternalOptions {
            k: 0,
            ..ExternalOptions::default()
        },
        ExternalOptions {
            k: 21,
            ..ExternalOptions::default()
        },
        ExternalOptions {
            configs: vec![],
            ..ExternalOptions::default()
        },
        ExternalOptions {
            configs: vec![support::bench::ConfigId::Rrf],
            ..ExternalOptions::default()
        },
        ExternalOptions {
            cost_cap_usd: f64::NAN,
            ..ExternalOptions::default()
        },
    ] {
        assert!(evaluate_directory(dir.path(), &options).is_err());
    }
}

#[test]
#[ignore = "manual: DOCSBASE_EXTERNAL_DIR and DOCSBASE_EXTERNAL_REPORT are required; advisory only"]
fn external_measurement() {
    support::external::measurement_from_env().expect("external measurement");
}

#[test]
fn external_metrics_use_document_gain_once_and_idcg_at_k() {
    use support::bench::Citation;
    use support::external::{ExternalDataset, metrics::evaluate};
    let dir = tiny_dataset();
    let data = ExternalDataset::load(dir.path(), "test", 10).unwrap();
    let cite = |id: &str| Citation {
        chunk_id: 1,
        path: data
            .path_to_id
            .iter()
            .find(|(_, d)| *d == id)
            .unwrap()
            .0
            .clone(),
        heading_path: String::new(),
        lines: (1, 2),
    };
    let a = cite("../../../escape");
    let b = cite("C:\\odd/../file");
    let z = cite("z");
    let one = evaluate(&data, "q1", std::slice::from_ref(&a), 1).unwrap();
    assert!(
        (one.metrics.ndcg - 1.0).abs() < 1e-12,
        "IDCG is truncated at k"
    );
    assert!((one.metrics.recall - 0.5).abs() < 1e-12);
    let dup = evaluate(&data, "q1", &[a.clone(), a.clone(), b], 3).unwrap();
    assert_eq!(dup.metrics.duplicate_documents, 1);
    assert!((dup.metrics.recall - 1.0).abs() < 1e-12);
    assert!((dup.metrics.ndcg - (1.0 + 0.5) / (1.0 + 1.0 / 3f64.log2())).abs() < 1e-12);
    let negative = evaluate(&data, "q1", &[z.clone(), a.clone()], 2).unwrap();
    assert_eq!(negative.metrics.top1_judgement, "explicit_nonrelevant");
    let unjudged = evaluate(&data, "q2", &[a, z], 2).unwrap();
    assert_eq!(unjudged.metrics.top1_judgement, "unjudged");
    for path in data.path_to_id.keys() {
        assert_eq!(path.len(), 67);
        assert!(data.root.path().join(path).is_file());
        assert_eq!(Path::new(path).components().count(), 1);
    }
}

struct TestEmbedder;
impl docsbase_memory::vector_cache::Embedder for TestEmbedder {
    fn model_id(&self) -> &'static str {
        "test/embed@d2@raw"
    }
    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}
struct TestReranker;
impl docsbase_memory::hybrid::Reranker for TestReranker {
    fn rerank(
        &self,
        _query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        assert_eq!(docs.len(), top_n);
        Ok((0..docs.len()).map(|i| (i, 1.0)).collect())
    }
}

#[test]
fn explicit_replay_runs_all_modes_and_missing_content_fails_closed() {
    use support::bench::{self, ConfigId};
    use support::external::ExternalDataset;
    let dir = tiny_dataset();
    let data = ExternalDataset::load(dir.path(), "test", 10).unwrap();
    let env = bench::build_env(data.root.path(), "tiny recording", 3..=3).unwrap();
    let replay = tempfile::tempdir().unwrap();
    let vectors = replay
        .path()
        .join("vectors")
        .join(bench::pin_slug("test/embed@d2@raw"));
    let rerank = replay
        .path()
        .join("rerank")
        .join(format!("{}.json", bench::pin_slug("test/rerank")));
    let embedder: std::sync::Arc<dyn docsbase_memory::vector_cache::Embedder> =
        std::sync::Arc::new(TestEmbedder);
    bench::record_fixtures_with(
        &env,
        &data.cases,
        &embedder,
        &TestReranker,
        "test/rerank",
        &vectors,
        &rerank,
    )
    .unwrap();
    let identity = serde_json::json!({"derived_corpus_hash":data.derived_corpus_hash,"queries_hash":data.queries_hash,
        "adapter_version":support::external::ADAPTER_VERSION,"tokenizer_version":docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
        "embedding_model_pin":"test/embed@d2@raw","rerank_model_pin":"test/rerank"});
    let path = replay.path().join("identity.json");
    std::fs::write(&path, identity.to_string()).unwrap();
    let options = ExternalOptions {
        configs: vec![
            ConfigId::Bm25,
            ConfigId::Bm25Rerank,
            ConfigId::Rrf,
            ConfigId::RrfRerank,
        ],
        replay_dir: Some(replay.path().into()),
        ..ExternalOptions::default()
    };
    let result = evaluate_directory(dir.path(), &options).unwrap();
    assert_eq!(result.quality.len(), 4);
    assert_eq!(result.provenance.model_pins.len(), 2);
    assert_eq!(
        result.provenance.embedding_vector_format,
        Some("v2-role-aware-raw")
    );
    assert_eq!(result.cost.provider_requests, 0);
    let dense_only = ExternalOptions {
        configs: vec![ConfigId::Rrf],
        replay_dir: Some(replay.path().into()),
        ..ExternalOptions::default()
    };
    assert!(
        evaluate_directory(dir.path(), &dense_only).is_err(),
        "every external report must retain the free BM25 baseline even with valid replay"
    );

    let mut broken = identity.clone();
    broken["tokenizer_version"] = serde_json::json!(0);
    std::fs::write(&path, broken.to_string()).unwrap();
    assert!(
        evaluate_directory(dir.path(), &options)
            .unwrap_err()
            .to_string()
            .contains("tokenizer")
    );
    std::fs::write(&path, identity.to_string()).unwrap();
    let original = std::fs::read(&rerank).unwrap();
    assert_incomplete_scores_rejected(dir.path(), &options, &rerank, &original);

    std::fs::write(
        &rerank,
        serde_json::json!({"model_pin":"test/rerank","pools":[]}).to_string(),
    )
    .unwrap();
    assert!(
        evaluate_directory(dir.path(), &options).is_err(),
        "missing scores must not downgrade to BM25"
    );
    std::fs::write(&rerank, original).unwrap();
    let saved = bench::VectorFixture::load(&vectors).unwrap();
    let mut missing_queries = saved.clone();
    missing_queries.query_vectors.as_mut().unwrap().clear();
    missing_queries.write(&vectors).unwrap();
    assert!(
        evaluate_directory(dir.path(), &options).is_err(),
        "missing query role must not use document rows or downgrade"
    );
    saved.write(&vectors).unwrap();
    let mut fixture = bench::VectorFixture::load(&vectors).unwrap();
    fixture.vectors.remove(&env.chunk_hashes[0].1).unwrap();
    fixture.write(&vectors).unwrap();
    assert!(
        evaluate_directory(dir.path(), &options).is_err(),
        "missing chunk vectors must not degrade dense results"
    );
}

#[test]
fn report_writer_protects_sources_frozen_fixtures_and_existing_reports() {
    let dir = tiny_dataset();
    let report = evaluate_directory(dir.path(), &ExternalOptions::default()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("report.json");
    support::external::write_report(&report, &path, dir.path()).unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(support::external::write_report(&report, &path, dir.path()).is_err());
    assert_eq!(original, std::fs::read(&path).unwrap());
    assert!(
        support::external::write_report(&report, &dir.path().join("queries.jsonl"), dir.path())
            .is_err()
    );
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/search-bench/blocked-report.json");
    assert!(support::external::write_report(&report, &fixture, dir.path()).is_err());
    assert!(!fixture.exists());
    assert!(support::external::parse_configs("Bm25,unknown").is_err());
}

#[test]
fn query_syntax_errors_remain_in_denominator_with_explicit_case_errors() {
    let dir = tiny_dataset();
    let p = dir.path().join("queries.jsonl");
    let text = std::fs::read_to_string(&p).unwrap();
    let broken = serde_json::json!({"_id":"broken","text":"The minor G allele of FOXO3 is related to more severe symptoms of Crohn's Disease."});
    std::fs::write(p, format!("{text}\n{broken}\n")).unwrap();
    let p = dir.path().join("qrels/test.tsv");
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(p, format!("{text}broken\tz\t1\n")).unwrap();
    let options = ExternalOptions {
        query_mode: QueryMode::Syntax,
        ..ExternalOptions::default()
    };
    let report = serde_json::to_value(evaluate_directory(dir.path(), &options).unwrap()).unwrap();
    assert_eq!(report["provenance"]["query_mode"], "syntax");
    let summary = &report["quality"]["Bm25"]["summary"];
    assert_eq!(summary["queries"], 3);
    assert_eq!(summary["query_errors"], 1);
    assert_eq!(summary["hits"], 2);
    assert_eq!(summary["misses"], 1);
    let cases = report["quality"]["Bm25"]["cases"].as_array().unwrap();
    assert_eq!(cases[0]["id"], "broken");
    assert!(cases[0]["error"].as_str().unwrap().contains("Syntax Error"));
    assert_eq!(cases[0]["metrics"]["hit"], false);
}

#[test]
fn each_raw_input_hash_changes_without_relabeling_unchanged_quality() {
    let dir = tiny_dataset();
    let before = run(dir.path()).unwrap();
    for (file, field) in [
        ("corpus.jsonl", "corpus_hash"),
        ("qrels/test.tsv", "qrels_hash"),
    ] {
        let p = dir.path().join(file);
        let bytes = std::fs::read(&p).unwrap();
        let mut changed = bytes.clone();
        changed.push(b'\n');
        std::fs::write(&p, changed).unwrap();
        let after = run(dir.path()).unwrap();
        assert_ne!(before["provenance"][field], after["provenance"][field]);
        assert_eq!(
            before["provenance"]["derived_corpus_hash"],
            after["provenance"]["derived_corpus_hash"]
        );
        assert_eq!(before["quality"], after["quality"]);
        std::fs::write(p, bytes).unwrap();
    }
    let data = support::external::ExternalDataset::load(dir.path(), "test", 10).unwrap();
    let wrong = ExternalOptions {
        split: "train".into(),
        ..ExternalOptions::default()
    };
    assert!(support::external::run_external(&data, &wrong).is_err());
}

fn assert_incomplete_scores_rejected(
    dir: &Path,
    options: &ExternalOptions,
    rerank: &Path,
    original: &[u8],
) {
    let valid_pools: serde_json::Value = serde_json::from_slice(original).unwrap();
    for corruption in ["empty", "partial", "duplicate"] {
        let mut broken = valid_pools.clone();
        let mut changed = false;
        for pool in broken["pools"].as_array_mut().unwrap() {
            let scores = pool["results"].as_array_mut().unwrap();
            match corruption {
                "empty" => {
                    scores.clear();
                    changed = true;
                }
                "partial" => {
                    scores.pop();
                    changed = true;
                }
                "duplicate" if scores.len() > 1 => {
                    scores[1] = scores[0].clone();
                    changed = true;
                }
                _ => {}
            }
        }
        assert!(changed);
        std::fs::write(rerank, broken.to_string()).unwrap();
        assert!(
            evaluate_directory(dir, options).is_err(),
            "{corruption} matched pool must not retain original ordering under a rerank label"
        );
    }
    std::fs::write(rerank, original).unwrap();
}

#[test]
fn literal_measurement_keeps_original_query_and_full_denominator_without_parser_errors() {
    let dir = tiny_dataset();
    let query =
        "The minor G allele of FOXO3 is related to more severe symptoms of Crohn's Disease.";
    let p = dir.path().join("queries.jsonl");
    let queries = std::fs::read_to_string(&p).unwrap();
    let row = serde_json::json!({"_id":"broken","text":query});
    std::fs::write(&p, format!("{queries}\n{row}\n")).unwrap();
    let p = dir.path().join("qrels/test.tsv");
    let qrels = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{qrels}broken\tz\t1\n")).unwrap();
    let p = dir.path().join("corpus.jsonl");
    let documents: Vec<String> = std::fs::read_to_string(&p)
        .unwrap()
        .lines()
        .map(|line| {
            let mut doc: serde_json::Value = serde_json::from_str(line).unwrap();
            if doc["_id"] == "z" {
                doc["text"] = serde_json::json!(format!("granite mineral {query}"));
            }
            doc.to_string()
        })
        .collect();
    std::fs::write(&p, documents.join("\n")).unwrap();
    let raw_queries = std::fs::read(dir.path().join("queries.jsonl")).unwrap();
    let syntax = run(dir.path()).unwrap();
    assert_eq!(syntax["provenance"]["query_mode"], "syntax");
    assert_eq!(syntax["quality"]["Bm25"]["summary"]["query_errors"], 1);
    let options = ExternalOptions {
        query_mode: QueryMode::Literal,
        ..ExternalOptions::default()
    };
    let literal = serde_json::to_value(evaluate_directory(dir.path(), &options).unwrap()).unwrap();
    assert_eq!(literal["provenance"]["query_mode"], "literal");
    for field in [
        "corpus_hash",
        "queries_hash",
        "qrels_hash",
        "derived_corpus_hash",
        "evaluated_query_ids",
    ] {
        assert_eq!(
            syntax["provenance"][field], literal["provenance"][field],
            "{field}"
        );
    }
    let summary = &literal["quality"]["Bm25"]["summary"];
    assert_eq!(summary["queries"], 3);
    assert_eq!(summary["query_errors"], 0);
    assert_eq!(summary["hits"], 3);
    assert_eq!(summary["misses"], 0);
    let cases = literal["quality"]["Bm25"]["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 3);
    assert_eq!(cases[0]["id"], "broken");
    assert!(cases.iter().all(|case| case["error"].is_null()));
    assert_eq!(literal["cost"]["spent_usd"], 0.0);
    assert_eq!(literal["cost"]["provider_requests"], 0);
    assert_eq!(
        raw_queries,
        std::fs::read(dir.path().join("queries.jsonl")).unwrap()
    );
}

#[test]
fn measurement_environment_selects_mode_and_rejects_invalid_values() {
    // A child process exercises the real manual entry point without mutating
    // process-wide environment in a parallel test process.
    let data = tiny_dataset();
    let output = tempfile::tempdir().unwrap();
    for (id, mode) in [
        Some("auto"),
        Some(""),
        None,
        Some("syntax"),
        Some("literal"),
    ]
    .into_iter()
    .enumerate()
    {
        let report = output.path().join(format!("report-{id}.json"));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "external_measurement",
                "--nocapture",
            ])
            .env("DOCSBASE_EXTERNAL_DIR", data.path())
            .env("DOCSBASE_EXTERNAL_REPORT", &report)
            .env("DOCSBASE_EXTERNAL_CONFIGS", "Bm25")
            .env("DOCSBASE_EXTERNAL_SPLIT", "test")
            .env("DOCSBASE_EXTERNAL_K", "10")
            .env("DOCSBASE_BENCH_COST_CAP_USD", "0")
            .env_remove("DOCSBASE_EXTERNAL_REPLAY_DIR")
            .env_remove("DOCSBASE_EXTERNAL_QUERY_MODE");
        if let Some(mode) = mode {
            command.env("DOCSBASE_EXTERNAL_QUERY_MODE", mode);
        }
        let result = command.output().unwrap();
        if matches!(mode, Some("auto" | "")) {
            assert!(
                !result.status.success(),
                "invalid mode must fail: {result:?}"
            );
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(error.contains("invalid external query mode"), "{error}");
            assert!(!report.exists(), "no misleading report after invalid mode");
        } else {
            assert!(result.status.success(), "{result:?}");
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
            assert_eq!(report["provenance"]["query_mode"], mode.unwrap_or("syntax"));
            assert_eq!(report["quality"]["Bm25"]["summary"]["queries"], 2);
            assert_eq!(report["cost"]["provider_requests"], 0);
            assert_eq!(report["cost"]["spent_usd"], 0.0);
        }
    }
}
