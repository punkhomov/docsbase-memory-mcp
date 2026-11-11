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
