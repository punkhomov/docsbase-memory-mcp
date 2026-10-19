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
    // Full validation across groups A and B.
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
