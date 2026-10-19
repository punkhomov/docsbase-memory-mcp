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
        "hit/absent_hit must match tests/fixtures/search-quality/baseline.json: {:?}",
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
        ],
    )
    .expect("groups A+B+C validate");
}

#[test]
#[ignore = "blocked on DELTA-2: chunk bound 500..=1000 unreachable (measured 132); un-ignore after T6 content + bound fix"]
fn fixtures_are_valid() {
    let root = bench_root();
    let fixtures = Fixtures::load(&root).expect("full fixtures load");
    // Corpus size bounds (FR-1): frozen docs plus authored content.
    let docs = fixtures.manifest.documents.len();
    assert!(
        (60..=80).contains(&docs),
        "corpus has {docs} docs, need 60..=80"
    );
    assert!(
        (120..=150).contains(&fixtures.cases.len()),
        "corpus has {} cases, need 120..=150",
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
    let env = support::bench::build_env(&root.join("corpus"), "search-bench", 60..=80)
        .expect("index corpus");
    let chunks = env.chunk_hashes.len();
    assert!(
        (500..=1000).contains(&chunks),
        "corpus has {chunks} chunks, need 500..=1000"
    );
}

use std::sync::{Arc, Mutex};

use docsbase_memory::hybrid::Reranker;
use docsbase_memory::vector_cache::Embedder;
use support::bench::{
    ConfigId, RerankFixture, RerankPool, Usage, VectorFixture, preload_vectors, run_config,
    stages_for,
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
