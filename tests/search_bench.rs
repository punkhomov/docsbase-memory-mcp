//! Search benchmark offline gate — T1: fixture schema + validator.
//!
//! RED step (TDD): these tests name the T1 interfaces from `support::bench`
//! (types, loader, validator, model-pin parser, hashes, DELTA-1 adjudication
//! and lexical gates). They fail to compile/run until the harness GREEN step
//! implements them.

mod support;

use std::fs;

use serde_json::json;
use tempfile::TempDir;

use support::bench::{
    Fixtures, MIN_CASES_PER_SLICE, ModelPin, lexical_jaccard, parse_model_pin, run_legacy_parity,
    validate_model_pins, validate_queries,
};

/// Minimal fixture tree: `corpus/<rel>` files + `MANIFEST.json` + `queries.json`.
fn write_tree(root: &std::path::Path, docs: &[(&str, &str)], queries: &serde_json::Value) {
    let corpus = root.join("corpus");
    for (rel, body) in docs {
        let path = corpus.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write doc");
    }
    let manifest = json!({
        "documents": docs.iter().map(|(rel, body)| {
            json!({
                "path": rel,
                "origin": "synthetic",
                "license": "MIT",
                "frozen_at": "2026-10-19",
                "blake3": blake3_of(body),
            })
        }).collect::<Vec<_>>(),
    });
    fs::write(corpus.join("MANIFEST.json"), manifest.to_string()).expect("write manifest");
    fs::write(root.join("queries.json"), queries.to_string()).expect("write queries");
}

fn blake3_of(body: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(body.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn case(id: &str, class: &str, expected: &[&str]) -> serde_json::Value {
    json!({
        "id": id,
        "query": format!("query for {id}"),
        "class": class,
        "k": 10,
        "expected": expected.iter().map(|p| {
            json!({"path": p, "lines": [1, 2], "grade": 2})
        }).collect::<Vec<_>>(),
        "absent": [],
        "source": "test",
        "notes": "t1",
    })
}

fn load_validated(root: &std::path::Path, classes: &[&str]) -> anyhow::Result<Fixtures> {
    let fixtures = Fixtures::load(root)?;
    validate_queries(&fixtures.cases, &root.join("corpus"), classes)?;
    Ok(fixtures)
}

const CLASSES: &[&str] = &["ru_morph", "en_morph"];

fn eight_cases(class: &str, doc: &str) -> Vec<serde_json::Value> {
    (0..MIN_CASES_PER_SLICE)
        .map(|i| case(&format!("{class}-{i:02}"), class, &[doc]))
        .collect()
}

#[test]
fn validator_accepts_minimal_fixture() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let mut queries = eight_cases("ru_morph", "ru/a.md");
    queries.extend(eight_cases("en_morph", "ru/a.md"));
    write_tree(tmp.path(), &docs, &json!(queries));
    let fixtures = load_validated(tmp.path(), CLASSES).expect("valid fixture passes");
    assert_eq!(fixtures.cases.len(), 2 * MIN_CASES_PER_SLICE);
}

#[test]
fn validator_rejects_expected_absent_overlap() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([{
        "id": "overlap-01",
        "query": "overlap",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "grade": 2}],
        "absent": [{"path": "ru/a.md", "reason": "trap"}],
        "source": "test",
        "notes": "t1",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("overlap must fail");
    assert!(err.to_string().contains("overlap-01"), "{err}");
}

#[test]
fn validator_rejects_missing_path() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([case("missing-01", "ru_morph", &["ru/ghost.md"])]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("missing path must fail");
    assert!(err.to_string().contains("ru/ghost.md"), "{err}");
}

#[test]
fn validator_rejects_bad_lines() {
    // Expected with an inverted range.
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\n")];
    let queries = json!([{
        "id": "lines-01",
        "query": "lines",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "lines": [5, 2], "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "t1",
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("bad lines must fail");
    assert!(err.to_string().contains("lines-01"), "{err}");

    // Absent with a range outside the file.
    let tmp = TempDir::new().expect("tmp");
    let queries = json!([{
        "id": "lines-02",
        "query": "lines",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "grade": 2}],
        "absent": [{"path": "ru/a.md", "lines": [1, 99], "reason": "trap"}],
        "source": "test",
        "notes": "t1",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("absent bad lines must fail");
    assert!(err.to_string().contains("lines-02"), "{err}");
}

#[test]
fn validator_rejects_bad_grade_and_k() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let grade = json!([{
        "id": "grade-01",
        "query": "grade",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "grade": 3}],
        "absent": [],
        "source": "test",
        "notes": "t1",
    }]);
    write_tree(tmp.path(), &docs, &grade);
    let err = load_validated(tmp.path(), CLASSES).expect_err("bad grade must fail");
    assert!(err.to_string().contains("grade-01"), "{err}");

    let tmp = TempDir::new().expect("tmp");
    let k = json!([{
        "id": "k-01",
        "query": "k",
        "class": "ru_morph",
        "k": 2,
        "expected": [{"path": "ru/a.md", "grade": 1}],
        "absent": [],
        "source": "test",
        "notes": "t1",
    }]);
    write_tree(tmp.path(), &docs, &k);
    let err = load_validated(tmp.path(), CLASSES).expect_err("bad k must fail");
    assert!(err.to_string().contains("k-01"), "{err}");
}

#[test]
fn validator_rejects_too_few_slice_cases() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([case("thin-01", "ru_morph", &["ru/a.md"])]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("thin slice must fail");
    assert!(err.to_string().contains("ru_morph"), "{err}");
}

#[test]
fn validator_rejects_duplicate_ids() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([
        case("dup-01", "ru_morph", &["ru/a.md"]),
        case("dup-01", "en_morph", &["ru/a.md"]),
    ]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("duplicate ids must fail");
    assert!(err.to_string().contains("dup-01"), "{err}");
}

#[test]
fn validator_rejects_unknown_class() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([case("class-01", "klingon", &["ru/a.md"])]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("unknown class must fail");
    assert!(err.to_string().contains("klingon"), "{err}");
}

#[test]
fn validator_rejects_mixed_model_pins() {
    let same_a =
        parse_model_pin("openrouter/qwen/qwen3-embedding-8b@d1024@l2").expect("parse pin a");
    let same_b =
        parse_model_pin("openrouter/qwen/qwen3-embedding-8b@d1024@l2").expect("parse pin b");
    let other =
        parse_model_pin("openrouter/qwen/qwen3-embedding-0.6b@d1024@l2").expect("parse pin other");
    assert!(matches!(same_a, ModelPin::Embedding { .. }));
    let rerank = parse_model_pin("openrouter/cohere/rerank-v3.5").expect("parse rerank pin");
    assert!(matches!(rerank, ModelPin::Rerank { .. }));
    validate_model_pins(&[same_a.clone(), same_b]).expect("identical pins pass");
    let err = validate_model_pins(&[same_a, other]).expect_err("mixed pins must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn validator_rejects_path_escape() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([case("escape-01", "ru_morph", &["../evil.md"])]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("path escape must fail");
    assert!(err.to_string().contains("escape-01"), "{err}");
}

#[test]
fn validator_rejects_hash_mismatch() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!(eight_cases("ru_morph", "ru/a.md"));
    write_tree(tmp.path(), &docs, &queries);
    // Corrupt the file after the manifest recorded its hash.
    fs::write(tmp.path().join("corpus").join("ru/a.md"), "# CHANGED\n").expect("corrupt");
    let err = load_validated(tmp.path(), CLASSES).expect_err("hash mismatch must fail");
    assert!(err.to_string().contains("ru/a.md"), "{err}");
}

#[test]
fn validator_rejects_absent_without_reason() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([{
        "id": "reason-01",
        "query": "trap",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "grade": 2}],
        "absent": [{"path": "ru/a.md"}],
        "source": "test",
        "notes": "t1",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    // Fires on the missing reason (overlap would also fire — either way the
    // trap is unusable, but the message must name the case).
    let err = load_validated(tmp.path(), CLASSES).expect_err("reason-less absent must fail");
    assert!(err.to_string().contains("reason-01"), "{err}");
}

#[test]
fn validator_rejects_missing_second_reviewer() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("sem/a.md", "# A\n\nquantum entanglement paradox\n")];
    let queries = json!([{
        "id": "review-01",
        "query": "quantum entanglement paradox",
        "class": "semantic",
        "k": 10,
        "expected": [{"path": "sem/a.md", "lines": [3, 3], "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "single reviewer is not adjudication",
        "reviewed_by": ["ann"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), &["semantic"]).expect_err("one reviewer must fail");
    assert!(err.to_string().contains("review-01"), "{err}");
}

#[test]
fn validator_rejects_missing_reviewer() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("sem/a.md", "# A\n\nquantum entanglement paradox\n")];
    let queries = json!([{
        "id": "review-02",
        "query": "quantum entanglement paradox",
        "class": "semantic",
        "k": 10,
        "expected": [{"path": "sem/a.md", "lines": [3, 3], "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "no reviewer at all",
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), &["semantic"]).expect_err("no reviewer must fail");
    assert!(err.to_string().contains("review-02"), "{err}");
}

#[test]
fn validator_rejects_missing_notes() {
    // Grade 1 without a rationale.
    let tmp = TempDir::new().expect("tmp");
    let docs = [("ru/a.md", "# A\n\nline one\nline two\n")];
    let queries = json!([{
        "id": "notes-01",
        "query": "partial",
        "class": "ru_morph",
        "k": 10,
        "expected": [{"path": "ru/a.md", "grade": 1}],
        "absent": [],
        "source": "test",
        "notes": "",
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), CLASSES).expect_err("grade 1 needs notes");
    assert!(err.to_string().contains("notes-01"), "{err}");

    // Critical class without notes, even at grade 2.
    let tmp = TempDir::new().expect("tmp");
    let docs = [("sem/a.md", "# A\n\nquantum entanglement paradox\n")];
    let queries = json!([{
        "id": "notes-02",
        "query": "quantum entanglement paradox",
        "class": "semantic",
        "k": 10,
        "expected": [{"path": "sem/a.md", "lines": [3, 3], "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), &["semantic"]).expect_err("critical class needs notes");
    assert!(err.to_string().contains("notes-02"), "{err}");
}

#[test]
fn validator_rejects_semantic_lexical_jaccard() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("sem/a.md", "# A\n\nalpha beta gamma delta epsilon\n")];
    let queries = json!([{
        "id": "sem-01",
        "query": "alpha beta gamma delta",
        "class": "semantic",
        "k": 10,
        "expected": [{"path": "sem/a.md", "lines": [3, 3], "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "shares lexicon with the span",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), &["semantic"]).expect_err("lexical semantic must fail");
    let msg = err.to_string();
    assert!(msg.contains("sem-01"), "{err}");
    assert!(msg.contains("lexical_jaccard"), "{err}");
}

#[test]
fn validator_rejects_semantic_without_lines() {
    let tmp = TempDir::new().expect("tmp");
    let docs = [("sem/a.md", "# A\n\nquantum entanglement paradox\n")];
    let queries = json!([{
        "id": "sem-02",
        "query": "quantum entanglement paradox",
        "class": "semantic",
        "k": 10,
        "expected": [{"path": "sem/a.md", "grade": 2}],
        "absent": [],
        "source": "test",
        "notes": "span required for the jaccard",
        "reviewed_by": ["ann", "bob"],
    }]);
    write_tree(tmp.path(), &docs, &queries);
    let err = load_validated(tmp.path(), &["semantic"]).expect_err("semantic needs lines");
    assert!(err.to_string().contains("sem-02"), "{err}");
}

#[test]
fn lexical_jaccard_matches_hand_computed() {
    // {alpha,beta} vs {alpha,beta,gamma} = 2/3.
    let partial = lexical_jaccard("alpha beta", "alpha beta gamma");
    assert!((partial - 2.0 / 3.0).abs() < 1e-9, "{partial}");
    // Identical sets.
    assert!((lexical_jaccard("alpha beta", "alpha beta") - 1.0).abs() < 1e-9);
    // Disjoint sets.
    assert!(lexical_jaccard("alpha beta", "delta epsilon").abs() < 1e-9);
}

#[test]
fn legacy_parity_matches_old_baseline() {
    let parity = run_legacy_parity().expect("legacy parity runs on the frozen corpus");
    assert_eq!(parity.cases.len(), 12, "all legacy cases ran");
    assert!(
        parity.matches,
        "hit/absent_hit must match tests/fixtures/search-bench/legacy-baseline.json: {:?} (frozen copy of the retired search-quality/baseline.json)",
        parity.cases
    );
}

fn bench_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench")
}

#[test]
fn query_group_a_is_valid() {
    let root = bench_root();
    let fixtures = Fixtures::load(&root).expect("group A+B fixtures load");
    for (class, min) in [("ru_morph", 12), ("en_morph", 10), ("identifier", 10)] {
        let count = fixtures.cases.iter().filter(|c| c.class == class).count();
        assert!(
            count >= min,
            "slice {class} has {count} cases, need >={min}"
        );
    }
    // Slice-standalone validation: group A stays fail-closed on its own
    // cases even as later groups extend the file.
    let group_a: Vec<_> = fixtures
        .cases
        .iter()
        .filter(|c| ["ru_morph", "en_morph", "identifier"].contains(&c.class.as_str()))
        .cloned()
        .collect();
    validate_queries(
        &group_a,
        &root.join("corpus"),
        &["ru_morph", "en_morph", "identifier"],
    )
    .expect("group A validates");
}

#[test]
fn query_group_b_is_valid() {
    let root = bench_root();
    let fixtures = Fixtures::load(&root).expect("group A+B fixtures load");
    for (class, min) in [("phrase", 10), ("cjk", 10), ("arabic", 10)] {
        let count = fixtures.cases.iter().filter(|c| c.class == class).count();
        assert!(
            count >= min,
            "slice {class} has {count} cases, need >={min}"
        );
    }
    // Slice-standalone validation for group B, same pattern as group A.
    let group_b: Vec<_> = fixtures
        .cases
        .iter()
        .filter(|c| {
            [
                "ru_morph",
                "en_morph",
                "identifier",
                "phrase",
                "cjk",
                "arabic",
            ]
            .contains(&c.class.as_str())
        })
        .cloned()
        .collect();
    validate_queries(
        &group_b,
        &root.join("corpus"),
        &[
            "ru_morph",
            "en_morph",
            "identifier",
            "phrase",
            "cjk",
            "arabic",
        ],
    )
    .expect("groups A+B validate");
}

/// Helper for authoring: prints `path blake3` for every corpus document to
/// fill `corpus/MANIFEST.json`. Ignored by default; never part of the gate.
#[test]
#[ignore = "authoring helper, prints corpus hashes for MANIFEST.json"]
fn print_corpus_hashes() {
    use support::bench::blake3_file;
    let corpus = bench_root().join("corpus");
    let mut paths = Vec::new();
    let mut stack = vec![corpus.clone()];
    while let Some(dir) = stack.pop() {
        let mut names: Vec<_> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        names.sort();
        for path in names {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                paths.push(path);
            }
        }
    }
    for path in paths {
        let rel = path.strip_prefix(&corpus).expect("under corpus");
        println!("{} {}", rel.display(), blake3_file(&path).expect("hash"));
    }
}

#[test]
fn query_group_c_is_valid() {
    let root = bench_root();
    let fixtures = Fixtures::load(&root).expect("group A+B+C fixtures load");
    for (class, min) in [("mixed_lang", 10), ("semantic", 14), ("hard_negative", 14)] {
        let count = fixtures.cases.iter().filter(|c| c.class == class).count();
        assert!(
            count >= min,
            "slice {class} has {count} cases, need >={min}"
        );
    }
    // Full validation across groups A, B and C (incl. DELTA-1 adjudication
    // gates and the semantic Jaccard ceiling).
    validate_queries(
        &fixtures.cases,
        &root.join("corpus"),
        &[
            "ru_morph",
            "en_morph",
            "identifier",
            "phrase",
            "cjk",
            "arabic",
            "mixed_lang",
            "semantic",
            "hard_negative",
            "multi_relevant",
            "long_doc_boundary",
        ],
    )
    .expect("groups A+B+C validate");
}

#[test]
fn fixtures_are_valid() {
    let root = bench_root();
    let fixtures = Fixtures::load(&root).expect("full fixtures load");
    // Corpus size bounds (FR-1): frozen docs plus authored content.
    let docs = fixtures.manifest.documents.len();
    assert!(
        (60..=85).contains(&docs),
        "corpus has {docs} docs, need 60..=85"
    );
    assert!(
        (120..=200).contains(&fixtures.cases.len()),
        "corpus has {} cases, need 120..=200",
        fixtures.cases.len()
    );
    for class in [
        "ru_morph",
        "en_morph",
        "identifier",
        "phrase",
        "cjk",
        "arabic",
        "mixed_lang",
        "semantic",
        "hard_negative",
        "multi_relevant",
        "long_doc_boundary",
    ] {
        let count = fixtures.cases.iter().filter(|c| c.class == class).count();
        assert!(count >= 8, "slice {class} has {count} cases, need >=8");
    }
    // Full DELTA-1 validation over every case.
    validate_queries(
        &fixtures.cases,
        &root.join("corpus"),
        &[
            "ru_morph",
            "en_morph",
            "identifier",
            "phrase",
            "cjk",
            "arabic",
            "mixed_lang",
            "semantic",
            "hard_negative",
            "multi_relevant",
            "long_doc_boundary",
        ],
    )
    .expect("full corpus validates");
    // Chunk-count bound (FR-1): runs the real indexer over the frozen corpus.
    let env = support::bench::build_env(&root.join("corpus"), "search-bench", 60..=85)
        .expect("index corpus");
    let chunks = env.chunk_hashes.len();
    assert!(
        (200..=400).contains(&chunks),
        "corpus has {chunks} chunks, need 200..=400"
    );
}

use std::sync::{Arc, Mutex};

use docsbase_memory::hybrid::Reranker;
use docsbase_memory::vector_cache::Embedder;
use support::bench::{
    ConfigId, RerankFixture, RerankPool, Usage, VectorFixture, preload_vectors,
    record_fixtures_with, run_config, stages_for,
};

fn tiny_corpus(root: &std::path::Path) {
    // Graded term frequency: doc A mentions the query terms most.
    // Each doc is one chunk (single line, no headings).
    fs::create_dir_all(root.join("corpus")).expect("mkdir");
    fs::write(
        root.join("corpus/a.md"),
        "alpha alpha alpha alpha alpha beta gamma.\n",
    )
    .expect("a");
    fs::write(root.join("corpus/b.md"), "alpha alpha beta gamma.\n").expect("b");
    fs::write(root.join("corpus/c.md"), "delta epsilon zeta.\n").expect("c");
    let manifest = json!({"documents": [
        {"path": "a.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": blake3_of("alpha alpha alpha alpha alpha beta gamma.\n")},
        {"path": "b.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": blake3_of("alpha alpha beta gamma.\n")},
        {"path": "c.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": blake3_of("delta epsilon zeta.\n")},
    ]});
    fs::write(root.join("corpus/MANIFEST.json"), manifest.to_string()).expect("manifest");
    fs::write(root.join("queries.json"), "[]").expect("queries");
}

/// Deterministic fake vectors from text bytes (test-only, dim 4).
fn fake_vector(text: &str, dim: usize) -> Vec<f32> {
    let digest = blake3_of_bytes(text.as_bytes());
    (0..dim)
        .map(|i| f32::from(digest[i % digest.len()]) / 255.0)
        .collect()
}

fn blake3_of_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    hasher.finalize().as_bytes().to_vec()
}

struct FakeEmbedder {
    dim: usize,
}

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &'static str {
        "test/fake@d4@l2"
    }

    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| fake_vector(t, self.dim)).collect())
    }
}

#[test]
fn replay_embedder_missing_hash_is_error() {
    let tmp = TempDir::new().expect("tmp");
    let fixture = VectorFixture {
        model_pin: "test/fake@d4@l2".to_owned(),
        dim: 4,
        corpus_hash: "test".to_owned(),
        queries_hash: "test".to_owned(),
        vectors: [("known".to_owned(), vec![1.0, 0.0, 0.0, 0.0])]
            .into_iter()
            .collect(),
    };
    fixture.write(&tmp.path().join("vectors")).expect("write");
    let loaded = VectorFixture::load(&tmp.path().join("vectors")).expect("load");
    let embedder = support::bench::ReplayEmbedder {
        pin: "test/fake@d4@l2".to_owned(),
        fixture: Arc::new(loaded),
    };
    let err = embedder
        .embed(&["unknown".to_owned()])
        .expect_err("missing hash must fail");
    assert!(err.to_string().contains("unknown"), "{err}");
}

#[test]
fn replay_reranker_requires_exact_candidate_set() {
    use support::bench::ReplayReranker;
    let tmp = TempDir::new().expect("tmp");
    let fixture = RerankFixture {
        model_pin: "test/rerank".to_owned(),
        pools: [(
            support::bench::RerankKey {
                query_hash: blake3_of("q"),
                candidate_hashes: vec![blake3_of("a"), blake3_of("b")],
            },
            RerankPool {
                top_n: 2,
                results: vec![(0, 0.9), (1, 0.1)],
                usage: None,
            },
        )]
        .into_iter()
        .collect(),
    };
    fixture
        .write(&tmp.path().join("rerank.json"))
        .expect("write");
    let loaded = RerankFixture::load(&tmp.path().join("rerank.json")).expect("load");
    let reranker = ReplayReranker {
        fixture: Arc::new(loaded),
    };
    let ok = reranker
        .rerank("q", &["a".to_owned(), "b".to_owned()], 2)
        .expect("exact set passes");
    assert_eq!(ok, vec![(0, 0.9), (1, 0.1)]);
    // Same docs, different order: a different candidate set.
    let err = rerank_call(&reranker, "q", &["b".to_owned(), "a".to_owned()], 2)
        .expect_err("reordered set must fail");
    assert!(!err.to_string().is_empty());
    // Same docs, different top_n: the recorded window changed.
    let err = rerank_call(&reranker, "q", &["a".to_owned(), "b".to_owned()], 1)
        .expect_err("top_n mismatch must fail");
    assert!(!err.to_string().is_empty());
    // Superset: extra candidates change the set.
    let err = rerank_call(
        &reranker,
        "q",
        &["a".to_owned(), "b".to_owned(), "c".to_owned()],
        3,
    )
    .expect_err("superset must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn replay_reranker_rejects_out_of_range_index() {
    use support::bench::ReplayReranker;
    let fixture = RerankFixture {
        model_pin: "test/rerank".to_owned(),
        pools: [(
            support::bench::RerankKey {
                query_hash: blake3_of("q"),
                candidate_hashes: vec![blake3_of("a"), blake3_of("b")],
            },
            RerankPool {
                top_n: 2,
                results: vec![(5, 0.9)],
                usage: Some(Usage {
                    tokens: None,
                    search_units: Some(1),
                }),
            },
        )]
        .into_iter()
        .collect(),
    };
    let reranker = ReplayReranker {
        fixture: Arc::new(fixture),
    };
    let err = rerank_call(&reranker, "q", &["a".to_owned(), "b".to_owned()], 2)
        .expect_err("out-of-range index must fail");
    assert!(err.to_string().contains('5'), "{err}");
}

fn rerank_call(
    reranker: &impl Reranker,
    query: &str,
    docs: &[String],
    top_n: usize,
) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
    reranker.rerank(query, docs, top_n)
}

struct ProbeReranker {
    seen: Mutex<Vec<usize>>,
}

impl Reranker for ProbeReranker {
    fn rerank(
        &self,
        _query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        assert_eq!(top_n, docs.len(), "provider ranks exactly what it receives");
        self.seen.lock().expect("lock").push(docs.len());
        Ok((0..docs.len()).map(|i| (i, 1.0)).collect())
    }
}

#[test]
fn run_config_uses_fetch_window_fifty_for_hybrid() {
    let tmp = TempDir::new().expect("tmp");
    fs::create_dir_all(tmp.path().join("corpus")).expect("mkdir");
    for i in 0..60 {
        fs::write(
            tmp.path().join(format!("corpus/d{i:02}.md")),
            format!("alpha filler terms document number {i}.\n"),
        )
        .expect("doc");
    }
    let manifest_docs: Vec<_> = (0..60)
        .map(|i: u32| {
            let body = format!("alpha filler terms document number {i}.\n");
            json!({"path": format!("d{i:02}.md"), "origin": "synthetic",
                   "license": "MIT", "frozen_at": "2026-10-19", "blake3": blake3_of(&body)})
        })
        .collect();
    fs::write(
        tmp.path().join("corpus/MANIFEST.json"),
        json!({"documents": manifest_docs}).to_string(),
    )
    .expect("manifest");
    fs::write(tmp.path().join("queries.json"), "[]").expect("queries");
    let env = support::bench::build_env(&tmp.path().join("corpus"), "probe", 60..=60)
        .expect("index probe corpus");
    let probe = Arc::new(ProbeReranker {
        seen: Mutex::new(Vec::new()),
    });
    let fake = Arc::new(FakeEmbedder { dim: 4 });
    let case = support::bench::QueryCase {
        id: "probe-01".to_owned(),
        query: "alpha".to_owned(),
        class: "probe".to_owned(),
        k: 3,
        expected: vec![],
        absent: vec![],
        source: "test".to_owned(),
        notes: "window probe".to_owned(),
        reviewed_by: vec![],
    };
    for cfg in [ConfigId::Bm25Rerank, ConfigId::RrfRerank] {
        let stages = stages_for(cfg, Some(fake.clone()), Some(probe.clone()));
        run_config(&env, &[case.clone()], cfg, &stages).expect("run");
    }
    let seen = probe.seen.lock().expect("lock");
    // Warm-up + case call per hybrid config, each seeing the full window.
    assert_eq!(seen.len(), 4, "warm-up + case call per hybrid config");
    assert!(
        seen.iter().all(|&n| n == 50),
        "hybrid fetch window is 50, got {seen:?}"
    );
}

/// Recording reranker: captures (query, docs), returns reversed identity so
/// the replayed run must show the reversed order (proves rerank applied).
struct RecordingReranker {
    seen: Mutex<Vec<(String, Vec<String>)>>,
}

impl Reranker for RecordingReranker {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        _top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        self.seen
            .lock()
            .expect("lock")
            .push((query.to_owned(), docs.to_vec()));
        let n = docs.len();
        // Small deterministic scores; the count is what the test asserts.
        #[allow(clippy::cast_precision_loss)]
        let ranked: Vec<(usize, f32)> = (0..n).rev().map(|i| (i, 1.0 - i as f32 * 0.01)).collect();
        Ok(ranked)
    }
}

fn probe_case(id: &str, query: &str, k: usize) -> support::bench::QueryCase {
    support::bench::QueryCase {
        id: id.to_owned(),
        query: query.to_owned(),
        class: "probe".to_owned(),
        k,
        expected: vec![],
        absent: vec![],
        source: "test".to_owned(),
        notes: "citation probe".to_owned(),
        reviewed_by: vec![],
    }
}

fn write_vector_fixture(
    tmp: &std::path::Path,
    texts: &[String],
    pin: &str,
) -> support::bench::VectorFixture {
    let mut vectors = std::collections::HashMap::new();
    for text in texts {
        vectors.insert(blake3_of(text), fake_vector(text, 4));
    }
    let fixture = VectorFixture {
        model_pin: pin.to_owned(),
        dim: 4,
        corpus_hash: "test".to_owned(),
        queries_hash: "test".to_owned(),
        vectors,
    };
    fixture.write(&tmp.join("vectors")).expect("write vectors");
    VectorFixture::load(&tmp.join("vectors")).expect("load vectors")
}

fn record_rerank_pools(
    env: &support::bench::BenchEnv,
    case: &support::bench::QueryCase,
    fake: &Arc<FakeEmbedder>,
) -> RerankFixture {
    let recorder = Arc::new(RecordingReranker {
        seen: Mutex::new(Vec::new()),
    });
    for cfg in [ConfigId::Bm25Rerank, ConfigId::RrfRerank] {
        let stages = stages_for(cfg, Some(fake.clone()), Some(recorder.clone()));
        run_config(env, std::slice::from_ref(case), cfg, &stages).expect("record run");
    }
    let mut pools = std::collections::HashMap::new();
    for (query, docs) in recorder.seen.lock().expect("lock").iter() {
        let key = support::bench::RerankKey {
            query_hash: blake3_of(query),
            candidate_hashes: docs.iter().map(|d| blake3_of(d)).collect(),
        };
        let n = docs.len();
        #[allow(clippy::cast_precision_loss)]
        let results: Vec<(usize, f32)> = (0..n).rev().map(|i| (i, 1.0 - i as f32 * 0.01)).collect();
        pools.insert(
            key,
            RerankPool {
                top_n: n,
                results,
                usage: None,
            },
        );
    }
    let rr = RerankFixture {
        model_pin: "test/rerank".to_owned(),
        pools,
    };
    rr.write(&env.cache.path().join("rerank.json"))
        .expect("write pools");
    RerankFixture::load(&env.cache.path().join("rerank.json")).expect("load pools")
}

#[test]
fn run_config_returns_citations_for_four_configs() {
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env = support::bench::build_env(&tmp.path().join("corpus"), "citations", 3..=3)
        .expect("index citation corpus");
    // Fixture vectors cover every chunk text plus the query texts.
    let mut texts = docsbase_memory::store::repo::chunk_texts(env.db.connection(), env.project.id)
        .expect("chunk texts");
    texts.push("alpha beta".to_owned());
    let pin = "test/fake@d4@l2".to_owned();
    let loaded = write_vector_fixture(tmp.path(), &texts, &pin);
    preload_vectors(&env, &loaded).expect("preload");
    let fake = Arc::new(FakeEmbedder { dim: 4 });
    let case = probe_case("cite-01", "alpha beta", 10);
    let pools = record_rerank_pools(&env, &case, &fake);
    let replay_embedder = Arc::new(support::bench::ReplayEmbedder {
        pin,
        fixture: Arc::new(loaded),
    });
    let replay_reranker = Arc::new(support::bench::ReplayReranker {
        fixture: Arc::new(pools),
    });
    let no_rerank_embedder = replay_embedder.clone() as Arc<dyn Embedder>;
    let both = replay_reranker.clone() as Arc<dyn Reranker>;
    let cfgs = [
        (
            ConfigId::Bm25,
            stages_for(ConfigId::Bm25, None, None),
            "bm25 must cite",
        ),
        (
            ConfigId::Bm25Rerank,
            stages_for(ConfigId::Bm25Rerank, None, Some(both.clone())),
            "bm25+rerank must cite",
        ),
        (
            ConfigId::Rrf,
            stages_for(ConfigId::Rrf, Some(no_rerank_embedder.clone()), None),
            "rrf must cite",
        ),
        (
            ConfigId::RrfRerank,
            stages_for(ConfigId::RrfRerank, Some(no_rerank_embedder), Some(both)),
            "rrf+rerank must cite",
        ),
    ];
    let mut tops = Vec::new();
    for (cfg, stages, msg) in &cfgs {
        let run = run_config(&env, &[case.clone()], *cfg, stages).expect("run");
        assert_eq!(run.cases.len(), 1);
        let citations = &run.cases[0].citations;
        assert!(!citations.is_empty(), "{msg}");
        tops.push(citations[0].path.clone());
    }
    // The recorded (reversed) order proves the rerank stage ran through replay.
    assert_ne!(tops[0], tops[1], "rerank must reorder Bm25 hits");
}

use support::bench::{Citation, QueryCase, evaluate_case};

fn citation(id: u64, path: &str, start: u32, end: u32) -> Citation {
    Citation {
        chunk_id: id,
        path: path.to_owned(),
        heading_path: "H".to_owned(),
        lines: (start, end),
    }
}

type ExpectedSpec<'a> = Vec<(&'a str, Option<(u32, u32)>, u8)>;

fn t8_case(id: &str, expected: ExpectedSpec) -> QueryCase {
    QueryCase {
        id: id.to_owned(),
        query: "q".to_owned(),
        class: "probe".to_owned(),
        k: 10,
        expected: expected
            .into_iter()
            .map(|(path, lines, grade)| support::bench::ExpectedHit {
                path: path.to_owned(),
                lines: lines.map(|(start, end)| support::bench::LineRange { start, end }),
                grade,
            })
            .collect(),
        absent: vec![],
        source: "test".to_owned(),
        notes: "t8".to_owned(),
        reviewed_by: vec![],
    }
}

/// Hand-computed constants stay separator-free to match machine output.
#[allow(clippy::unreadable_literal)]
#[test]
fn ndcg_matches_hand_computed_example() {
    // Ranks: X(1) miss, A(2, grade 2), B(3, grade 1).
    // DCG = 3/log2(3) + 1/log2(4); IDCG = 3/log2(2) + 1/log2(3).
    let case = t8_case("ndcg-01", vec![("a.md", None, 2), ("b.md", None, 1)]);
    let hits = vec![
        citation(1, "x.md", 1, 1),
        citation(2, "a.md", 1, 1),
        citation(3, "b.md", 1, 1),
    ];
    let metrics = evaluate_case(&hits, &case);
    assert!(metrics.hit);
    let expected = 0.6590018048024133;
    assert!(
        (metrics.ndcg - expected).abs() < 1e-9,
        "ndcg={} expected={}",
        metrics.ndcg,
        expected
    );
}

#[test]
fn ndcg_suppresses_duplicate_gain() {
    // The second chunk matching the same expected doc adds no gain.
    let case = t8_case("ndcg-02", vec![("a.md", None, 2)]);
    let hits = vec![
        citation(1, "a.md", 1, 1),
        citation(2, "a.md", 2, 2),
        citation(3, "x.md", 1, 1),
    ];
    let metrics = evaluate_case(&hits, &case);
    assert!((metrics.ndcg - 1.0).abs() < 1e-12, "ndcg={}", metrics.ndcg);
}

/// Hand-computed constants mirror the design literals verbatim (spec §5.1);
/// readability lints are off for the same reason as on the table itself.
#[allow(clippy::unreadable_literal, clippy::excessive_precision)]
#[test]
fn ndcg_discount_table_is_checked_in() {
    use support::bench::NDCG_DISCOUNT;
    // Exact fractions (powers of two): rank 1, 3, 7, 15.
    assert!((NDCG_DISCOUNT[0] - 1.0).abs() < 1e-12);
    assert!((NDCG_DISCOUNT[2] - 0.5).abs() < 1e-12);
    assert!((NDCG_DISCOUNT[6] - 1.0 / 3.0).abs() < 1e-12);
    assert!((NDCG_DISCOUNT[14] - 0.25).abs() < 1e-12);
    // Irrational spots, verbatim design literals (spot-checked by hand).
    assert!((NDCG_DISCOUNT[1] - 0.63092975357145753).abs() < 1e-15);
    assert!((NDCG_DISCOUNT[9] - 0.28906482631788788).abs() < 1e-15);
    assert!((NDCG_DISCOUNT[19] - 0.22767024869695299).abs() < 1e-15);
}

#[test]
fn recall_and_mrr_multi_relevant() {
    let case = t8_case(
        "rm-01",
        vec![("a.md", None, 2), ("b.md", None, 1), ("c.md", None, 1)],
    );
    let hits = vec![
        citation(1, "x.md", 1, 1),
        citation(2, "b.md", 1, 1),
        citation(3, "a.md", 1, 1),
    ];
    let metrics = evaluate_case(&hits, &case);
    assert!(metrics.hit);
    assert!(
        (metrics.recall - 2.0 / 3.0).abs() < 1e-12,
        "recall={}",
        metrics.recall
    );
    assert!((metrics.mrr - 0.5).abs() < 1e-12, "mrr={}", metrics.mrr);
}

#[test]
fn citation_overlap_uses_lines() {
    let case = t8_case("co-01", vec![("a.md", Some((5, 8)), 2)]);
    let hits = vec![
        citation(1, "a.md", 5, 6),
        citation(2, "a.md", 20, 30),
        citation(3, "b.md", 1, 2),
    ];
    let metrics = evaluate_case(&hits, &case);
    assert!(
        (metrics.citation_overlap - 1.0 / 3.0).abs() < 1e-12,
        "overlap={}",
        metrics.citation_overlap
    );
}

#[test]
fn rank1_forbidden_only() {
    use support::bench::AbsentHit;
    let mut no_lines = t8_case("fh-01", vec![("a.md", None, 2)]);
    no_lines.absent = vec![AbsentHit {
        path: "d.md".to_owned(),
        lines: None,
        reason: "trap".to_owned(),
    }];
    // Forbidden doc without lines: only rank 1 counts (DELTA-3).
    let hits = vec![citation(1, "d.md", 1, 1), citation(2, "a.md", 1, 1)];
    let metrics = evaluate_case(&hits, &no_lines);
    assert!(metrics.hit);
    assert!(metrics.forbidden_hit);
    let clean = evaluate_case(&[citation(1, "a.md", 1, 1)], &no_lines);
    assert!(!clean.forbidden_hit);
    // Same trap at rank 2: visible to the reading agent, not a failure.
    let below = vec![citation(1, "a.md", 1, 1), citation(2, "d.md", 1, 1)];
    assert!(!evaluate_case(&below, &no_lines).forbidden_hit);
    // Forbidden doc WITH lines: only an intersecting range at rank 1 counts.
    let mut ranged = t8_case("fh-02", vec![("a.md", None, 2)]);
    ranged.absent = vec![AbsentHit {
        path: "d.md".to_owned(),
        lines: Some(support::bench::LineRange { start: 10, end: 20 }),
        reason: "trap".to_owned(),
    }];
    let overlapping = vec![citation(1, "d.md", 15, 25), citation(2, "a.md", 1, 1)];
    assert!(evaluate_case(&overlapping, &ranged).forbidden_hit);
    let disjoint = vec![citation(1, "a.md", 1, 1), citation(2, "d.md", 15, 25)];
    assert!(!evaluate_case(&disjoint, &ranged).forbidden_hit);
}

/// Trap movement below rank 1 must persist in the report (DELTA-3): a green
/// run still shows traps climbing toward the top, not just failures.
#[test]
fn trap_diagnostics_persist_in_report() {
    use support::bench::AbsentHit;
    let mut base = toy_baseline();
    base.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("bm25")
        .cases[0]
        .ranks = vec![toy_citation("a.md", 1, 1), toy_citation("d.md", 1, 1)];
    base.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("bm25")
        .cases[0]
        .id = "td-01".to_owned();
    let mut case = t8_case("td-01", vec![("a.md", None, 2)]);
    case.absent = vec![AbsentHit {
        path: "d.md".to_owned(),
        lines: None,
        reason: "trap".to_owned(),
    }];
    let mut report = support::bench::build_report(&base, None);
    assert!(
        report.trap_diagnostics.is_empty(),
        "diagnostics attach explicitly, never by default"
    );
    support::bench::attach_trap_diagnostics(&mut report, std::slice::from_ref(&case));
    assert!(
        report
            .trap_diagnostics
            .iter()
            .any(|warning| warning.contains("d.md") && warning.contains("rank 2")),
        "trap at rank 2 must be recorded: {:?}",
        report.trap_diagnostics
    );
    let quality = support::bench::quality_json(&report);
    assert!(
        quality.contains("trap_diagnostics") && quality.contains("d.md"),
        "quality JSON must carry the diagnostics"
    );
}

#[test]
fn duplicate_path_ratio_counts_repeats() {
    let case = t8_case("dup-01", vec![("a.md", None, 2), ("b.md", None, 1)]);
    let hits = vec![
        citation(1, "a.md", 1, 1),
        citation(2, "a.md", 2, 2),
        citation(3, "b.md", 1, 1),
        citation(4, "a.md", 3, 3),
    ];
    let metrics = evaluate_case(&hits, &case);
    assert!(
        (metrics.duplicate_path_ratio - 0.5).abs() < 1e-12,
        "dup={}",
        metrics.duplicate_path_ratio
    );
}

/// Hand-computed constants stay separator-free to match machine output.
#[allow(clippy::unreadable_literal)]
#[test]
fn early_metrics_match_hand_computed() {
    use support::bench::early_metrics;
    // X(1), Y(2) miss; A(3, grade 2), B(4, grade 1); k=10.
    let case = t8_case("em-01", vec![("a.md", None, 2), ("b.md", None, 1)]);
    let hits = vec![
        citation(1, "x.md", 1, 1),
        citation(2, "y.md", 1, 1),
        citation(3, "a.md", 1, 1),
        citation(4, "b.md", 1, 1),
    ];
    let early = early_metrics(&hits, &case);
    assert!(early.hit_at_3);
    assert!((early.recall_at_3 - 0.5).abs() < 1e-12);
    assert!(
        (early.ndcg_at_3 - 0.41311732856427996).abs() < 1e-9,
        "{}",
        early.ndcg_at_3
    );
}

#[test]
fn outcome_matrix_counts_per_slice() {
    use support::bench::{OutcomeCounts, classify_case, outcome_matrix};
    assert_eq!(
        classify_case(&[true, true]),
        support::bench::CasePattern::AllHit
    );
    assert_eq!(
        classify_case(&[false, false]),
        support::bench::CasePattern::AllMiss
    );
    assert_eq!(
        classify_case(&[true, false]),
        support::bench::CasePattern::Mixed
    );
    let matrix = outcome_matrix(&[
        ("ru", support::bench::CasePattern::AllHit),
        ("ru", support::bench::CasePattern::Mixed),
        ("en", support::bench::CasePattern::AllMiss),
    ]);
    assert_eq!(
        matrix.by_slice["ru"],
        OutcomeCounts {
            all_hit: 1,
            all_miss: 0,
            mixed: 1
        }
    );
    assert_eq!(
        matrix.by_slice["en"],
        OutcomeCounts {
            all_hit: 0,
            all_miss: 1,
            mixed: 0
        }
    );
}

#[test]
fn bootstrap_is_deterministic() {
    use support::bench::bootstrap_ci;
    let deltas = vec![0.1, 0.2, 0.3];
    let first = bootstrap_ci(&deltas);
    let second = bootstrap_ci(&deltas);
    assert_eq!(first, second, "seeded bootstrap must be identical");
    assert!(first.0 <= first.1, "CI lower <= upper");
    // Constant deltas collapse the interval.
    let flat = bootstrap_ci(&[0.5, 0.5, 0.5]);
    assert!((flat.0 - 0.5).abs() < 1e-12 && (flat.1 - 0.5).abs() < 1e-12);
}

#[test]
fn aggregate_slices_matches_hand_computed() {
    use support::bench::aggregate_slices;
    // Case 1: hit, full recall, nDCG 1.0. Case 2: total miss.
    let one = t8_case("agg-01", vec![("a.md", None, 2)]);
    let two = t8_case("agg-02", vec![("b.md", None, 1)]);
    let m1 = evaluate_case(&[citation(1, "a.md", 1, 1)], &one);
    let m2 = evaluate_case(&[citation(1, "x.md", 1, 1)], &two);
    let slices = aggregate_slices([("probe", &m1), ("probe", &m2)]);
    let report = &slices["probe"];
    assert_eq!(report.cases, 2);
    for (actual, expected) in [
        (report.hit_rate, 0.5),
        (report.recall, 0.5),
        (report.mrr, 0.5),
        (report.ndcg, 0.5),
        (report.citation_overlap, 0.5),
        (report.duplicate_path_ratio, 0.0),
    ] {
        assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
    }
    assert!((report.forbidden_rate - 0.0).abs() < 1e-12);
}

#[test]
fn delta_report_counts_wins_losses() {
    use support::bench::delta_report;
    // Two cases: one improved past tolerance, one tied.
    let delta = delta_report(&[0.5, 0.5], &[0.5, 0.8], 0.9, 0.9, 0.1).expect("delta");
    assert_eq!((delta.wins, delta.losses, delta.ties), (1, 0, 1));
    assert!(
        (delta.ndcg_delta - 0.15).abs() < 1e-9,
        "{}",
        delta.ndcg_delta
    );
    assert!((delta.hit_rate_delta - 0.0).abs() < 1e-12);
    assert!(delta.ndcg_ci95.0 <= delta.ndcg_ci95.1);
}

#[test]
#[allow(clippy::unreadable_literal, clippy::excessive_precision)]
fn gain_requires_line_overlap_when_expected_has_lines() {
    use support::bench::AbsentHit;
    // Same doc, disjoint chunk: no gain, no hit — the citation is wrong
    // even though the path matches (design §5.6, FR-9).
    let mut case = t8_case("ln-01", vec![("a.md", Some((5, 8)), 2)]);
    case.absent = vec![AbsentHit {
        path: "d.md".to_owned(),
        lines: None,
        reason: "trap".to_owned(),
    }];
    let wrong_chunk = vec![citation(1, "a.md", 20, 30), citation(2, "d.md", 1, 1)];
    let miss = evaluate_case(&wrong_chunk, &case);
    assert!(!miss.hit);
    assert!(miss.ndcg.abs() < 1e-12, "ndcg={}", miss.ndcg);
    assert!(
        miss.citation_overlap.abs() < 1e-12,
        "overlap={}",
        miss.citation_overlap
    );
    // Trap at rank 2 is not a failure under DELTA-3 (rank-1 rule).
    assert!(!miss.forbidden_hit);
    // Intersecting chunk: full gain at its rank.
    let right_chunk = vec![citation(1, "x.md", 1, 1), citation(2, "a.md", 5, 6)];
    let hit = evaluate_case(&right_chunk, &case);
    assert!(hit.hit);
    assert!(
        (hit.ndcg - 0.63092975357145753).abs() < 1e-12,
        "ndcg={}",
        hit.ndcg
    );
    assert!(!hit.forbidden_hit);
}

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Deterministic fake embedder with an optional one-shot failure and an
/// additive bias (lets two pins share texts without sharing vectors).
struct FailingEmbedder {
    dim: usize,
    bias: f32,
    fail_after_calls: AtomicUsize,
}

impl FailingEmbedder {
    fn working(dim: usize, bias: f32) -> Self {
        Self {
            dim,
            bias,
            fail_after_calls: AtomicUsize::new(usize::MAX),
        }
    }

    fn failing(dim: usize) -> Self {
        Self {
            dim,
            bias: 0.0,
            fail_after_calls: AtomicUsize::new(0),
        }
    }
}

impl Embedder for FailingEmbedder {
    fn model_id(&self) -> &'static str {
        "test/fake@d4@l2"
    }

    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        let remaining = self.fail_after_calls.fetch_sub(1, Ordering::SeqCst);
        if remaining == 0 {
            return Err(docsbase_memory::error::Error::internal(
                "injected embed failure",
            ));
        }
        Ok(texts
            .iter()
            .map(|text| {
                fake_vector(text, self.dim)
                    .iter()
                    .map(|v| v + self.bias)
                    .collect()
            })
            .collect())
    }
}

struct FailingReranker {
    fail: AtomicBool,
}

impl docsbase_memory::hybrid::Reranker for FailingReranker {
    fn rerank(
        &self,
        _query: &str,
        docs: &[String],
        _top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(docsbase_memory::error::Error::internal(
                "injected rerank failure",
            ));
        }
        Ok((0..docs.len()).map(|i| (i, 1.0)).collect())
    }
}

fn record_case() -> support::bench::QueryCase {
    probe_case("rec-01", "alpha beta", 10)
}

#[test]
fn record_mode_writes_replayable_fixtures() {
    use support::bench::{RerankFixture, VectorFixture};
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env = support::bench::build_env(&tmp.path().join("corpus"), "record", 3..=3)
        .expect("index corpus");
    let case = record_case();
    // Direct run with fake stages.
    let fake_embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    let fake_reranker = Arc::new(FailingReranker {
        fail: AtomicBool::new(false),
    });
    let direct_bm25 = run_config(
        &env,
        &[case.clone()],
        ConfigId::Bm25Rerank,
        &stages_for(
            ConfigId::Bm25Rerank,
            Some(fake_embedder.clone()),
            Some(fake_reranker.clone() as Arc<dyn docsbase_memory::hybrid::Reranker>),
        ),
    )
    .expect("direct run");
    // Record, then replay from disk.
    let vectors_dir = tmp.path().join("vectors");
    let rerank_path = tmp.path().join("rerank.json");
    record_fixtures_with(
        &env,
        &[case.clone()],
        &fake_embedder,
        fake_reranker.as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect("record");
    let loaded_vectors = VectorFixture::load(&vectors_dir).expect("load vectors");
    let loaded_rerank = RerankFixture::load(&rerank_path).expect("load rerank");
    let replay = run_config(
        &env,
        &[case.clone()],
        ConfigId::Bm25Rerank,
        &stages_for(
            ConfigId::Bm25Rerank,
            Some(Arc::new(support::bench::ReplayEmbedder {
                pin: "test/fake@d4@l2".to_owned(),
                fixture: Arc::new(loaded_vectors),
            }) as Arc<dyn Embedder>),
            Some(Arc::new(support::bench::ReplayReranker {
                fixture: Arc::new(loaded_rerank),
            }) as Arc<dyn docsbase_memory::hybrid::Reranker>),
        ),
    )
    .expect("replay run");
    assert_eq!(
        direct_bm25.cases[0].citations, replay.cases[0].citations,
        "replay must reproduce the recorded run"
    );
    assert!(!replay.cases[0].citations.is_empty());
}

#[test]
fn record_mode_is_atomic_on_failure() {
    use support::bench::{RerankFixture, VectorFixture};
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env = support::bench::build_env(&tmp.path().join("corpus"), "atomic", 3..=3)
        .expect("index corpus");
    let case = record_case();
    let vectors_dir = tmp.path().join("vectors");
    let rerank_path = tmp.path().join("rerank.json");
    // Phase 1: a complete record (baseline state).
    let emb: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    record_fixtures_with(
        &env,
        &[case.clone()],
        &emb,
        Arc::new(FailingReranker {
            fail: AtomicBool::new(false),
        })
        .as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect("baseline record");
    let manifest_before = fs::read(vectors_dir.join("manifest.json")).expect("manifest");
    let rerank_before = fs::read(&rerank_path).expect("rerank json");
    // Phase 2a: embedder fails before any write — files must be untouched.
    let emb_fail: Arc<dyn Embedder> = Arc::new(FailingEmbedder::failing(4));
    let err = record_fixtures_with(
        &env,
        &[case.clone()],
        &emb_fail,
        Arc::new(FailingReranker {
            fail: AtomicBool::new(false),
        })
        .as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect_err("failing embedder must fail the record");
    assert!(err.to_string().contains("injected embed failure"), "{err}");
    assert_eq!(
        fs::read(vectors_dir.join("manifest.json")).expect("manifest"),
        manifest_before,
        "failed record must not touch the manifest"
    );
    // Phase 2b: embedder works, reranker fails after the vector commit.
    let emb2: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    let err = record_fixtures_with(
        &env,
        &[case.clone()],
        &emb2,
        Arc::new(FailingReranker {
            fail: AtomicBool::new(true),
        })
        .as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect_err("failing reranker must fail the record");
    assert!(err.to_string().contains("injected rerank failure"), "{err}");
    // The committed vectors stay valid and loadable; the rerank file is
    // untouched (written only on full success); no temp files leak.
    let vectors = VectorFixture::load(&vectors_dir).expect("vectors stay valid");
    assert!(
        vectors
            .get("alpha alpha alpha alpha alpha beta gamma.\n")
            .is_ok()
    );
    assert_eq!(
        fs::read(&rerank_path).expect("rerank json"),
        rerank_before,
        "failed rerank must not touch the rerank file"
    );
    assert!(RerankFixture::load(&rerank_path).is_ok());
    for entry in [&vectors_dir, tmp.path()]
        .iter()
        .flat_map(|d| fs::read_dir(d).expect("ls"))
    {
        let is_tmp = entry
            .expect("entry")
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"));
        assert!(!is_tmp, "temp file leaked");
    }
}

#[test]
fn record_mode_does_not_mix_model_pins() {
    use support::bench::VectorFixture;
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env =
        support::bench::build_env(&tmp.path().join("corpus"), "pins", 3..=3).expect("index corpus");
    let case = record_case();
    let vectors_dir = tmp.path().join("vectors");
    let rerank_path = tmp.path().join("rerank.json");
    let fake = Arc::new(FailingReranker {
        fail: AtomicBool::new(false),
    });
    let emb_a: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    record_fixtures_with(
        &env,
        &[case.clone()],
        &emb_a,
        fake.as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect("record A");
    let emb_b: Arc<dyn Embedder> = Arc::new(FailingEmbedder {
        dim: 4,
        bias: 100.0,
        fail_after_calls: AtomicUsize::new(usize::MAX),
    });
    record_fixtures_with(
        &env,
        &[case],
        &emb_b,
        fake.as_ref(),
        "test/rerank",
        &vectors_dir,
        &rerank_path,
    )
    .expect("record B");
    let loaded = VectorFixture::load(&vectors_dir).expect("load");
    let text = "alpha alpha alpha alpha alpha beta gamma.\n";
    let got = loaded.get(text).expect("vector present");
    assert!(
        got.iter().all(|v| *v >= 100.0),
        "second pin must fully replace the first: {got:?}"
    );
}

// ---------------------------------------------------------------------------
// T10: baseline, gate, determinism, latency, report (RED)
// ---------------------------------------------------------------------------

use support::bench::{
    Baseline, CaseReport, ConfigId as BenchConfigId, ConfigReport, DiscriminationReport,
    FailureKind, LatencyReport, Report,
};

fn toy_citation(path: &str, start: u32, end: u32) -> support::bench::Citation {
    support::bench::Citation {
        chunk_id: 7,
        path: path.to_owned(),
        heading_path: String::new(),
        lines: (start, end),
    }
}

fn toy_case_report(id: &str, hit: bool, ndcg: f64, forbidden: bool) -> CaseReport {
    CaseReport {
        id: id.to_owned(),
        class: "probe".to_owned(),
        k: 10,
        ranks: vec![toy_citation("a.md", 1, 2)],
        metrics: support::bench::CaseMetrics {
            hit,
            recall: if hit { 1.0 } else { 0.0 },
            mrr: if hit { 1.0 } else { 0.0 },
            ndcg,
            citation_overlap: 1.0,
            forbidden_hit: forbidden,
            duplicate_path_ratio: 0.0,
        },
        early: support::bench::EarlyMetrics {
            hit_at_3: hit,
            recall_at_3: if hit { 1.0 } else { 0.0 },
            ndcg_at_3: ndcg,
        },
    }
}

fn toy_slice(hit_rate: f64, ndcg: f64) -> support::bench::SliceReport {
    support::bench::SliceReport {
        cases: 2,
        hit_rate,
        recall: hit_rate,
        mrr: hit_rate,
        ndcg,
        citation_overlap: 1.0,
        forbidden_rate: 0.0,
        duplicate_path_ratio: 0.0,
    }
}

fn toy_baseline() -> Baseline {
    let mut slices = std::collections::BTreeMap::new();
    slices.insert("probe".to_owned(), toy_slice(1.0, 0.9));
    let mut configs = std::collections::BTreeMap::new();
    configs.insert(
        BenchConfigId::Bm25,
        ConfigReport {
            cases: vec![
                toy_case_report("t10-01", true, 0.9, false),
                toy_case_report("t10-02", true, 0.9, false),
            ],
            slices,
            latency: LatencyReport {
                p50_ms: 10.0,
                p95_ms: 20.0,
            },
        },
    );
    Baseline {
        bench_version: support::bench::BENCH_VERSION,
        fixture_version: support::bench::FIXTURE_VERSION,
        corpus_hash: "corpus".to_owned(),
        legacy_corpus_hash: "legacy".to_owned(),
        queries_hash: "queries".to_owned(),
        model_pins: vec!["test/fake@d4@l2".to_owned()],
        generated_at: "t0".to_owned(),
        index_build_ms: 0.0,
        configs,
        legacy_parity: support::bench::LegacyParity {
            cases: vec![],
            matches: true,
        },
        discrimination: DiscriminationReport {
            saturated_total: 0,
            saturated_ratio: 0.0,
            non_saturated_by_slice: std::collections::BTreeMap::default(),
            semantic_disagreements: 3,
            rerank_disagreements: 1,
            semantic_max_lexical_jaccard: 0.1,
            lexical_tokenizer_version: docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
        },
        outcome_matrix: support::bench::OutcomeMatrix::default(),
        known_failures: Vec::new(),
    }
}

fn toy_current() -> Report {
    support::bench::build_report(&toy_baseline(), None)
}

fn gate_has_kind(current: &Report, base: &Baseline, kind: &FailureKind) -> bool {
    support::bench::gate_failures(current, base)
        .iter()
        .any(|f| std::mem::discriminant(&f.kind) == std::mem::discriminant(kind))
}

#[test]
fn gate_detects_each_failure_kind() {
    // Clean pair: no failures.
    let base = toy_baseline();
    assert!(
        support::bench::gate_failures(&toy_current(), &base).is_empty(),
        "identical run must pass the gate"
    );
    // (1) version mismatch.
    let mut bad = toy_baseline();
    bad.fixture_version += 1;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::VersionMismatch),
        "fixture_version drift must fail"
    );
    // (1b) hash mismatch.
    let mut bad = toy_baseline();
    bad.corpus_hash = "other".to_owned();
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::HashMismatch),
        "corpus hash drift must fail"
    );
}

#[test]
fn gate_detects_hit_and_forbidden_failures() {
    let base = toy_baseline();
    // (2) forbidden hit.
    let mut bad = toy_baseline();
    bad.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0]
        .metrics
        .forbidden_hit = true;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::ForbiddenHit),
        "forbidden_hit must fail"
    );
    // (3) hit true -> false.
    let mut bad = toy_baseline();
    let case = &mut bad
        .configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0];
    case.metrics.hit = false;
    case.metrics.ndcg = 0.0;
    case.metrics.recall = 0.0;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::HitDrop),
        "hit drop must fail"
    );
}

#[test]
fn gate_detects_ndcg_failures() {
    let base = toy_baseline();
    // (4) per-case nDCG drop beyond tolerance.
    let mut bad = toy_baseline();
    bad.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0]
        .metrics
        .ndcg = 0.5;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::NdcgDrop),
        "nDCG drop 0.4 must fail"
    );
    // ...but a 0.01 wobble passes.
    let mut ok = toy_baseline();
    ok.configs.get_mut(&BenchConfigId::Bm25).expect("cfg").cases[0]
        .metrics
        .ndcg = 0.89;
    let current = support::bench::build_report(&ok, None);
    assert!(
        !gate_has_kind(&current, &base, &FailureKind::NdcgDrop),
        "nDCG wobble within tolerance must pass"
    );
    // (6) slice nDCG drop beyond tolerance.
    let mut bad = toy_baseline();
    bad.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .slices
        .get_mut("probe")
        .expect("slice")
        .ndcg = 0.5;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::SliceNdcgDrop),
        "slice nDCG drop must fail"
    );
}

#[test]
fn gate_detects_slice_and_drift_failures() {
    let base = toy_baseline();
    // (5) slice hit_rate drop.
    let mut bad = toy_baseline();
    bad.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .slices
        .get_mut("probe")
        .expect("slice")
        .hit_rate = 0.5;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::SliceHitRateDrop),
        "slice hit_rate drop must fail"
    );
    // (7a) missing config.
    let mut bad = toy_baseline();
    bad.configs.remove(&BenchConfigId::Bm25);
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::MissingConfig),
        "missing config must fail"
    );
    // (7b) case set drift.
    let mut bad = toy_baseline();
    bad.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases
        .pop();
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::CaseDrift),
        "case set drift must fail"
    );
}

#[test]
fn gate_detects_legacy_and_version_drift() {
    let mut base = toy_baseline();
    base.legacy_parity = support::bench::LegacyParity {
        cases: vec![support::bench::LegacyCaseReport {
            id: "legacy-01".to_owned(),
            query: "q".to_owned(),
            hit: true,
            absent_hit: false,
            ndcg: 1.0,
        }],
        matches: true,
    };
    let mut bad = base.clone();
    bad.legacy_parity.cases[0].hit = false;
    bad.legacy_parity.matches = false;
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &base, &FailureKind::LegacyMismatch),
        "legacy hit drift must fail"
    );
    // Version and identity-field drift.
    for mutate in [
        (|b: &mut Baseline| b.bench_version += 1) as fn(&mut Baseline),
        (|b: &mut Baseline| b.queries_hash = "other".to_owned()),
        (|b: &mut Baseline| b.legacy_corpus_hash = "other".to_owned()),
        (|b: &mut Baseline| b.model_pins = vec!["other".to_owned()]),
    ] {
        let mut bad = toy_baseline();
        mutate(&mut bad);
        let current = support::bench::build_report(&bad, None);
        assert!(
            !support::bench::gate_failures(&current, &toy_baseline()).is_empty(),
            "identity drift must fail"
        );
    }
    // Extra config is drift, not a silent extension.
    let mut bad = toy_baseline();
    bad.configs.insert(
        support::bench::ConfigId::Rrf,
        bad.configs.get(&BenchConfigId::Bm25).expect("cfg").clone(),
    );
    let current = support::bench::build_report(&bad, None);
    assert!(
        gate_has_kind(&current, &toy_baseline(), &FailureKind::CaseDrift),
        "extra config must fail as drift"
    );
}

#[test]
fn gate_boundary_values_pass() {
    let base = toy_baseline();
    // Exactly the tolerance: not "beyond", must pass.
    let mut edge = toy_baseline();
    edge.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0]
        .metrics
        .ndcg = 0.88;
    let current = support::bench::build_report(&edge, None);
    assert!(
        support::bench::gate_failures(&current, &base).is_empty(),
        "exact 0.02 nDCG drop must pass"
    );
    // Timestamps never gate.
    let mut restamped = toy_baseline();
    restamped.generated_at = "whenever".to_owned();
    let current = support::bench::build_report(&restamped, None);
    assert!(
        support::bench::gate_failures(&current, &base).is_empty(),
        "generated_at drift must not fail"
    );
}

#[test]
fn gate_tolerates_known_failures_but_not_new_ones() {
    use support::bench::{Failure, FailureKind, gate_new_failures};
    let known_trap = |id: &str| Failure {
        config: Some(BenchConfigId::Bm25),
        slice: Some("hard_negative".to_owned()),
        case_id: Some(id.to_owned()),
        kind: FailureKind::ForbiddenHit,
        expected: "no forbidden hit".to_owned(),
        actual: "forbidden_hit = true".to_owned(),
    };
    let known = vec![known_trap("hn-99")];
    // Same failure again: no regression, no fix.
    let (new, fixed) = gate_new_failures(&[known_trap("hn-99")], &known);
    assert!(new.is_empty() && fixed.is_empty());
    // A different trap: regression.
    let (new, _) = gate_new_failures(&[known_trap("hn-99"), known_trap("hn-100")], &known);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].case_id.as_deref(), Some("hn-100"));
    // Trap gone: fixed (reported, not failed).
    let (new, fixed) = gate_new_failures(&[], &known);
    assert!(new.is_empty());
    assert_eq!(fixed.len(), 1);
    // Production strings round-trip: a real gate Failure tolerates itself.
    let mut base = toy_baseline();
    base.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0]
        .metrics
        .forbidden_hit = true;
    let current = support::bench::build_report(&base, None);
    let real: Vec<support::bench::Failure> =
        support::bench::gate_failures(&current, &toy_baseline());
    assert!(!real.is_empty());
    let (new, _) = gate_new_failures(&real, &real);
    assert!(new.is_empty(), "gate output must tolerate itself");
}

#[test]
fn gate_allows_improvement_without_baseline_update() {
    let base = toy_baseline();
    let mut better = toy_baseline();
    let case = &mut better
        .configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .cases[0];
    case.metrics.ndcg = 1.0;
    better
        .configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .slices
        .get_mut("probe")
        .expect("slice")
        .ndcg = 0.95;
    let current = support::bench::build_report(&better, None);
    assert!(
        support::bench::gate_failures(&current, &base).is_empty(),
        "pure improvement must pass without a baseline update"
    );
}

#[test]
fn gate_rejects_sabotaged_reranker() {
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env =
        support::bench::build_env(&tmp.path().join("corpus"), "sabotage", 3..=3).expect("index");
    // k = 1 and the answer is BM25 top-1 (b.md wins on length norm):
    // any reorder flips the hit.
    let case = sabotage_case("alpha beta", 1, &["b.md"]);
    let embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    let good = Arc::new(FailingReranker {
        fail: AtomicBool::new(false),
    });
    let cfg = support::bench::ConfigId::Bm25Rerank;
    let stages = support::bench::stages_for(
        cfg,
        Some(embedder.clone()),
        Some(good.clone() as Arc<dyn docsbase_memory::hybrid::Reranker>),
    );
    let (run, lat) =
        support::bench::run_config_timed(&env, &[case.clone()], cfg, &stages).expect("good run");
    let baseline_like = support::bench::build_baseline_like(
        &[case.clone()],
        &[(cfg, run, lat)],
        support::bench::ReportMeta::synthetic(),
        0.0,
    )
    .expect("baseline-like");
    assert!(
        baseline_like.configs.get(&cfg).expect("cfg").cases[0]
            .metrics
            .hit,
        "good run must hit b.md at k=1"
    );
    // Saboteur: reranker that inverts the candidate order.
    let evil = Arc::new(ReversingReranker);
    let evil_stages = support::bench::stages_for(
        cfg,
        Some(embedder.clone()),
        Some(evil as Arc<dyn docsbase_memory::hybrid::Reranker>),
    );
    let (evil_run, evil_lat) =
        support::bench::run_config_timed(&env, &[case.clone()], cfg, &evil_stages)
            .expect("evil run");
    let evil_like = support::bench::build_baseline_like(
        &[case],
        &[(cfg, evil_run, evil_lat)],
        support::bench::ReportMeta::synthetic(),
        0.0,
    )
    .expect("evil baseline-like");
    let current = support::bench::build_report(&evil_like, None);
    assert!(
        !support::bench::gate_failures(&current, &baseline_like).is_empty(),
        "reversed rerank must trip the gate"
    );
}

/// Three docs with identical BM25 (same terms, same length): dense decides.
/// A one-slot vector rotation permutes dense similarities without a
/// symmetric tie, so the RRF order deterministically changes.
fn tie_corpus(root: &std::path::Path) {
    fs::create_dir_all(root.join("corpus")).expect("mkdir");
    fs::write(root.join("corpus/a.md"), "alpha beta gamma.\n").expect("a");
    fs::write(root.join("corpus/b.md"), "alpha beta delta.\n").expect("b");
    fs::write(root.join("corpus/c.md"), "alpha beta epsilon.\n").expect("c");
    let manifest = json!({"documents": [
        {"path": "a.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": "00"},
        {"path": "b.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": "00"},
        {"path": "c.md", "origin": "synthetic", "license": "MIT",
         "frozen_at": "2026-10-19", "blake3": "00"},
    ]});
    fs::write(
        root.join("corpus/MANIFEST.json"),
        serde_json::to_string(&manifest).expect("manifest"),
    )
    .expect("manifest");
}

fn sabotage_case(query: &str, k: usize, paths: &[&str]) -> support::bench::QueryCase {
    support::bench::QueryCase {
        id: "sabotage-01".to_owned(),
        query: query.to_owned(),
        class: "probe".to_owned(),
        k,
        expected: paths
            .iter()
            .map(|path| support::bench::ExpectedHit {
                path: (*path).to_owned(),
                lines: None,
                grade: 2,
            })
            .collect(),
        absent: vec![],
        source: "test".to_owned(),
        notes: "sabotage probe".to_owned(),
        reviewed_by: vec![],
    }
}

/// Preloads the vector cache with fake vectors for every chunk; with
/// `rotate`, the batch is shifted by one slot (deterministic sabotage that
/// permutes dense similarities while the single-text query embedding is
/// unaffected).
fn make_fixture(env: &support::bench::BenchEnv, rotate: bool) -> VectorFixture {
    let texts = docsbase_memory::store::repo::chunk_texts(env.db.connection(), env.project.id)
        .expect("chunk texts");
    let mut vecs: Vec<Vec<f32>> = texts.iter().map(|text| fake_vector(text, 4)).collect();
    if rotate {
        vecs.rotate_right(1);
    }
    VectorFixture {
        model_pin: "test/fake@d4@l2".to_owned(),
        dim: 4,
        corpus_hash: "test".to_owned(),
        queries_hash: "test".to_owned(),
        vectors: texts
            .iter()
            .zip(vecs)
            .map(|(text, vec)| (blake3_of(text), vec))
            .collect(),
    }
}

fn preload_fixture(env: &support::bench::BenchEnv, rotate: bool) {
    let fixture = make_fixture(env, rotate);
    support::bench::preload_vectors(env, &fixture).expect("preload");
}

#[test]
fn observe_pools_is_deterministic() {
    let tmp = TempDir::new().expect("tmp");
    tiny_corpus(tmp.path());
    let env =
        support::bench::build_env(&tmp.path().join("corpus"), "observe", 3..=3).expect("index");
    let case = record_case();
    let mut fixture = make_fixture(&env, false);
    // The query embedding also goes through the fixture (as in record).
    fixture
        .vectors
        .insert(blake3_of(&case.query), fake_vector(&case.query, 4));
    support::bench::preload_vectors(&env, &fixture).expect("preload");
    let replay: Arc<dyn Embedder> = Arc::new(support::bench::ReplayEmbedder {
        pin: fixture.model_pin.clone(),
        fixture: Arc::new(fixture),
    });
    let reranker = FailingReranker {
        fail: AtomicBool::new(false),
    };
    let case = record_case();
    let first = support::bench::observe_rerank_pools(
        &env,
        &[case.clone()],
        &replay,
        &reranker,
        "test/rerank",
    )
    .expect("observe once");
    let second =
        support::bench::observe_rerank_pools(&env, &[case], &replay, &reranker, "test/rerank")
            .expect("observe twice");
    let keys = |pools: &std::collections::HashMap<
        support::bench::RerankKey,
        support::bench::RerankPool,
    >| {
        pools
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>()
    };
    assert_eq!(
        keys(&first),
        keys(&second),
        "observation through fixture vectors must be deterministic"
    );
    assert_eq!(
        first.len(),
        2,
        "warm-up must dedup against the real call (one pool per config)"
    );
}

#[test]
fn gate_rejects_shuffled_embedder() {
    let tmp = TempDir::new().expect("tmp");
    tie_corpus(tmp.path());
    let good_env = support::bench::build_env(&tmp.path().join("corpus"), "shuffle-good", 3..=3)
        .expect("index");
    preload_fixture(&good_env, false);
    let evil_env = support::bench::build_env(&tmp.path().join("corpus"), "shuffle-evil", 3..=3)
        .expect("index");
    preload_fixture(&evil_env, true);
    let good: Arc<dyn Embedder> = Arc::new(FailingEmbedder::working(4, 0.0));
    let cfg = support::bench::ConfigId::Rrf;
    let stages = support::bench::stages_for(cfg, Some(good.clone()), None);
    // Self-calibrating: the good run's top-2 set becomes the expected set
    // (fixed blake3 vectors make this deterministic across runs/hosts).
    let probe = sabotage_case("alpha beta", 2, &["a.md"]);
    let (probe_run, _) =
        support::bench::run_config_timed(&good_env, &[probe], cfg, &stages).expect("probe run");
    let top2: Vec<String> = probe_run.cases[0]
        .citations
        .iter()
        .take(2)
        .map(|citation| citation.path.clone())
        .collect();
    assert_eq!(top2.len(), 2, "good run must return 2 citations at k=2");
    let case = sabotage_case("alpha beta", 2, &[top2[0].as_str(), top2[1].as_str()]);
    let (run, lat) = support::bench::run_config_timed(&good_env, &[case.clone()], cfg, &stages)
        .expect("good run");
    let baseline_like = support::bench::build_baseline_like(
        &[case.clone()],
        &[(cfg, run, lat)],
        support::bench::ReportMeta::synthetic(),
        0.0,
    )
    .expect("baseline-like");
    assert!(
        baseline_like.configs.get(&cfg).expect("cfg").cases[0]
            .metrics
            .hit,
        "good run must hit its own top-2"
    );
    // Rotated chunk vectors permute dense similarities; the BM25-tied RRF
    // order changes and the expected set breaks.
    let evil_stages = support::bench::stages_for(cfg, Some(good), None);
    let (evil_run, evil_lat) =
        support::bench::run_config_timed(&evil_env, &[case.clone()], cfg, &evil_stages)
            .expect("evil run");
    let evil_top2: Vec<String> = evil_run.cases[0]
        .citations
        .iter()
        .take(2)
        .map(|citation| citation.path.clone())
        .collect();
    assert_ne!(
        evil_top2, top2,
        "rotated embeddings must change the RRF top-2 on a BM25-tied triple"
    );
    let evil_like = support::bench::build_baseline_like(
        &[case],
        &[(cfg, evil_run, evil_lat)],
        support::bench::ReportMeta::synthetic(),
        0.0,
    )
    .expect("evil baseline-like");
    let current = support::bench::build_report(&evil_like, None);
    assert!(
        !support::bench::gate_failures(&current, &baseline_like).is_empty(),
        "rotated embeddings must break RRF ranking and trip the gate"
    );
}

/// Reranker that inverts whatever order the collector observed.
struct ReversingReranker;

#[allow(clippy::cast_precision_loss)]
impl docsbase_memory::hybrid::Reranker for ReversingReranker {
    fn rerank(
        &self,
        _query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        Ok((0..docs.len())
            .rev()
            .take(top_n)
            .enumerate()
            .map(|(rank, i)| (i, 100.0 - rank as f32))
            .collect())
    }
}

#[test]
fn evaluate_is_deterministic() {
    let base = toy_baseline();
    let first = support::bench::quality_json(&support::bench::build_report(&base, None));
    let second = support::bench::quality_json(&support::bench::build_report(&base, None));
    assert_eq!(first, second, "quality JSON must be byte-identical");
    assert!(
        !first.contains("p50_ms") && !first.contains("generated_at"),
        "quality JSON must exclude latency and timestamps"
    );
}

#[test]
fn latency_budget_math() {
    for (baseline, want) in [(100.0, 155.0), (400.0, 500.0), (0.0, 5.0)] {
        let got = support::bench::latency_allowed_ms(baseline);
        assert!(
            (got - want).abs() < 1e-9,
            "latency_allowed_ms({baseline}) = {got}, want {want}"
        );
    }
    let base = toy_baseline();
    let mut slow = toy_baseline();
    slow.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .latency
        .p95_ms = 156.0;
    let current = support::bench::build_report(&slow, None);
    assert!(
        !support::bench::latency_failures(&current, &base).is_empty(),
        "p95 156 > 20*1.5+5=35 must breach"
    );
    let mut ok = toy_baseline();
    ok.configs
        .get_mut(&BenchConfigId::Bm25)
        .expect("cfg")
        .latency
        .p95_ms = 34.9;
    let current = support::bench::build_report(&ok, None);
    assert!(
        support::bench::latency_failures(&current, &base).is_empty(),
        "p95 within budget must pass"
    );
}

#[test]
fn baseline_preserves_timestamp_when_unchanged() {
    let dir = TempDir::new().expect("tmp");
    let path = dir.path().join("baseline.json");
    support::bench::write_baseline(&toy_baseline(), &path).expect("first write");
    let before = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    std::thread::sleep(std::time::Duration::from_millis(5));
    support::bench::write_baseline(&toy_baseline(), &path).expect("second write");
    let after = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    assert_eq!(before, after, "identical baseline must not touch the file");
    let mut changed = toy_baseline();
    changed.model_pins.push("other".to_owned());
    support::bench::write_baseline(&changed, &path).expect("changed write");
    let reloaded = support::bench::load_baseline(&path).expect("reload");
    assert_eq!(
        reloaded.model_pins.len(),
        2,
        "changed baseline must persist"
    );
    // SC-9: timestamp/index-build churn alone must not rewrite the file.
    let mtime = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut restamped = toy_baseline();
    restamped.model_pins.push("other".to_owned());
    restamped.generated_at = "t1".to_owned();
    restamped.index_build_ms = 123.0;
    support::bench::write_baseline(&restamped, &path).expect("restamp write");
    let after_restamp = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    assert_eq!(
        mtime, after_restamp,
        "timestamp-only drift must not touch the file"
    );
    let reread = support::bench::load_baseline(&path).expect("reread");
    assert_eq!(
        reread.generated_at, "t0",
        "skipped write keeps the old timestamp"
    );
}

#[test]
fn discrimination_detects_each_failure() {
    let ok = DiscriminationReport {
        saturated_total: 10,
        saturated_ratio: 0.5,
        non_saturated_by_slice: [("semantic".to_owned(), 2)].into_iter().collect(),
        semantic_disagreements: 3,
        rerank_disagreements: 1,
        semantic_max_lexical_jaccard: 0.4,
        lexical_tokenizer_version: docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
    };
    assert!(
        support::bench::discrimination_failures(&ok).is_empty(),
        "healthy discrimination must pass"
    );
    let mut bad = ok.clone();
    bad.saturated_ratio = 0.8;
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "saturated_ratio 0.8 must fail"
    );
    let mut bad = ok.clone();
    bad.non_saturated_by_slice.clear();
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "slice without non-saturated case must fail"
    );
    let mut bad = ok.clone();
    bad.semantic_disagreements = 2;
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "2 semantic disagreements must fail"
    );
    let mut bad = ok.clone();
    bad.rerank_disagreements = 0;
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "0 rerank disagreements must fail"
    );
    let mut bad = ok.clone();
    bad.semantic_max_lexical_jaccard = 0.6;
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "jaccard 0.6 must fail"
    );
    bad = ok.clone();
    bad.lexical_tokenizer_version = docsbase_memory::index::tokenizer::TOKENIZER_VERSION - 1;
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "stale tokenizer version must fail"
    );
    // A complete map with a fully-saturated slice fails the coverage rule.
    let mut bad = ok.clone();
    bad.non_saturated_by_slice.insert("semantic".to_owned(), 0);
    assert!(
        !support::bench::discrimination_failures(&bad).is_empty(),
        "fully-saturated slice must fail coverage"
    );
}

#[test]
fn vector_rewrite_sweeps_orphan_bins() {
    let tmp = TempDir::new().expect("tmp");
    let dir = tmp.path();
    let fixture = |value: f32| VectorFixture {
        model_pin: "test/fake@d4@l2".to_owned(),
        dim: 1,
        corpus_hash: "c".to_owned(),
        queries_hash: "q".to_owned(),
        vectors: [("deadbeef".to_owned(), vec![value])].into_iter().collect(),
    };
    fixture(1.0).write(dir).expect("write v1");
    fixture(2.0).write(dir).expect("write v2");
    let names: Vec<String> = fs::read_dir(dir)
        .expect("ls")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|name| std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bin")))
            .count(),
        1,
        "exactly one live bin must remain: {names:?}"
    );
    assert!(
        names.contains(&"manifest.json".to_owned()),
        "manifest must remain: {names:?}"
    );
    let loaded = VectorFixture::load(dir).expect("reload");
    assert_eq!(loaded.vectors["deadbeef"], vec![2.0]);
}

#[test]
fn missing_baseline_is_hard_fail() {
    let dir = TempDir::new().expect("tmp");
    let err = support::bench::load_baseline(&dir.path().join("baseline.json"))
        .expect_err("absent baseline must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn missing_vectors_is_hard_fail() {
    let dir = TempDir::new().expect("tmp");
    let err = support::bench::VectorFixture::load(&dir.path().join("vectors"))
        .expect_err("absent vectors must fail");
    assert!(!err.to_string().is_empty());
}

/// Full offline gate against the recorded baseline (enabled in T11).
#[test]
fn metrics_match_baseline() {
    let root = bench_root();
    let baseline = support::bench::load_baseline(&root.join("baseline.json")).expect("baseline");
    let fixtures = Fixtures::load(&root).expect("fixtures");
    let env = support::bench::build_env(&root.join("corpus"), "gate", 60..=85).expect("index");
    let mut runs = Vec::new();
    for cfg in [
        support::bench::ConfigId::Bm25,
        support::bench::ConfigId::Bm25Rerank,
        support::bench::ConfigId::Rrf,
        support::bench::ConfigId::RrfRerank,
    ] {
        let replay = support::bench::replay_stages(&root, &baseline).expect("replay stages");
        // Chunk vectors live on disk: without the preload every dense
        // config silently degrades to its lexical base.
        if let Some(fixture) = &replay.vectors {
            support::bench::preload_vectors(&env, fixture).expect("preload");
        }
        let stages = support::bench::stages_for(cfg, replay.embedder, replay.reranker);
        let (run, lat) =
            support::bench::run_config_timed(&env, &fixtures.cases, cfg, &stages).expect("run");
        runs.push((cfg, run, lat));
    }
    let current_like = support::bench::build_baseline_like(
        &fixtures.cases,
        &runs,
        support::bench::ReportMeta::from_baseline(&baseline),
        baseline.discrimination.semantic_max_lexical_jaccard,
    )
    .expect("current");
    let mut current = support::bench::build_report(&current_like, Some(&baseline));
    // Trap placements below rank 1 are diagnostics for release review
    // (DELTA-3), not failures. They persist in the report artifact, so trap
    // movement toward the top stays visible on green runs too.
    support::bench::attach_trap_diagnostics(&mut current, &fixtures.cases);
    let report_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/search-bench/gate-report.json");
    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir gate report dir");
    }
    std::fs::write(
        &report_path,
        serde_json::to_string_pretty(&current).expect("serialize gate report"),
    )
    .expect("write gate report");
    println!(
        "trap diagnostics: {} recorded -> {}",
        current.trap_diagnostics.len(),
        report_path.display()
    );
    let failures = support::bench::gate_failures(&current, &baseline);
    // DELTA-3: fail only on NEW failures (regressions vs the known set);
    // fixed known failures are reported as improvements.
    let (new_failures, fixed) =
        support::bench::gate_new_failures(&failures, &baseline.known_failures);
    assert!(
        new_failures.is_empty(),
        "new gate failures: {new_failures:?}\ntrap diagnostics:\n{}",
        current.trap_diagnostics.join("\n")
    );
    if !fixed.is_empty() {
        println!("fixed since baseline: {fixed:?}");
    }
    if cfg!(target_os = "linux") {
        let latency = support::bench::latency_failures(&current, &baseline);
        assert!(latency.is_empty(), "latency failures: {latency:?}");
    }
}

/// Regenerates `baseline.json` from the recorded fixtures (manual, T11 runs
/// it after legacy parity; T12 re-runs after hardening).
#[test]
#[ignore = "manual: rewrites baseline.json; run only deliberately (FR-15)"]
fn regenerate_baseline() {
    let root = bench_root();
    let parity = support::bench::run_legacy_parity().expect("legacy parity");
    assert!(
        parity.matches,
        "baseline requires green legacy parity (FR-15)"
    );
    let baseline = support::bench::regenerate_from_fixtures(&root).expect("regenerate");
    let failures = support::bench::discrimination_failures(&baseline.discrimination);
    assert!(
        failures.is_empty(),
        "baseline must discriminate before it is accepted (FR-28): {failures:?}"
    );
    support::bench::write_baseline(&baseline, &root.join("baseline.json")).expect("write");
}

/// The benchmark must discriminate (FR-28): reads the frozen baseline.
/// RED in T12 until the baseline is recorded and passes.
#[test]
fn benchmark_is_discriminating() {
    let root = bench_root();
    let baseline = support::bench::load_baseline(&root.join("baseline.json")).expect("baseline");
    let failures = support::bench::discrimination_failures(&baseline.discrimination);
    assert!(
        failures.is_empty(),
        "benchmark must discriminate (FR-28): {failures:?}"
    );
}

#[test]
fn pacing_reranker_spaces_calls() {
    let reranker = support::bench::PacingReranker::new(
        FailingReranker {
            fail: AtomicBool::new(false),
        },
        std::time::Duration::from_millis(120),
    );
    let docs = vec!["a".to_owned(), "b".to_owned()];
    let started = std::time::Instant::now();
    reranker.rerank("q", &docs, 2).expect("first");
    reranker.rerank("q", &docs, 2).expect("second");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(100),
        "second call must wait out the interval"
    );
}
