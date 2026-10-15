use std::fs;
use std::path::Path;

use docsbase_memory::config::{Config, ConfigOverrides};
use tempfile::TempDir;

fn write(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("write fixture");
}

#[test]
fn defaults_when_missing() {
    let config_dir = TempDir::new().expect("tempdir");
    let config = Config::load_from(config_dir.path(), None).expect("load");

    assert_eq!(config, Config::default());
    assert_eq!(config.max_file_size, 1_048_576);
    assert_eq!(config.max_docs_per_project, 20_000);
    assert!(!config.hybrid);
}

#[test]
fn auto_index_default_false() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");
    write(project.path(), ".docsbase.toml", "max_file_size = 42\n");

    let config = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    assert!(!config.auto_index, "auto_index must stay opt-in (ADR-6)");
    assert!(config.hybrid, "global flag inherited");
    assert_eq!(config.max_file_size, 42);
}

#[test]
fn project_overrides_global() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "max_file_size = 100\nauto_index = true\nignores = [\"a/**\"]\n",
    );
    write(
        project.path(),
        ".docsbase.toml",
        "max_file_size = 200\nignores = [\"b/**\"]\n",
    );

    let config = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    assert_eq!(config.max_file_size, 200, "project wins over global");
    assert!(
        config.auto_index,
        "global value inherited when project is silent"
    );
    assert_eq!(config.ignores, vec!["b/**".to_owned()]);
}

#[test]
fn cli_overrides_project() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(project.path(), ".docsbase.toml", "max_file_size = 200\n");

    let base = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    let config = base.with_overrides(&ConfigOverrides {
        max_file_size: Some(300),
        auto_index: Some(true),
        ..ConfigOverrides::default()
    });

    assert_eq!(config.max_file_size, 300, "CLI wins over project");
    assert!(config.auto_index, "CLI can opt in explicitly");
}

#[test]
fn unknown_key_rejected() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "unknown_key = 1\n");

    let err = Config::load_from(config_dir.path(), None).expect_err("must reject");
    let text = err.to_string();
    assert!(text.contains("unknown_key"), "{text}");
}

#[test]
fn zero_limits_rejected() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "max_file_size = 0\n");

    let err = Config::load_from(config_dir.path(), None).expect_err("must reject");
    assert!(
        matches!(err, docsbase_memory::error::Error::Admission { .. }),
        "{err:?}"
    );

    let overridden = Config::default().with_overrides(&ConfigOverrides {
        max_docs_per_project: Some(0),
        ..ConfigOverrides::default()
    });
    assert!(
        overridden.validate().is_err(),
        "overrides must be revalidated"
    );
}

#[test]
fn hybrid_bool_shorthand_enables_stage_defaults() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");

    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert!(config.hybrid);
    assert!(config.hybrid_cfg.enabled);
    let embedding = config.hybrid_cfg.embedding.expect("embedding stage");
    assert_eq!(embedding.provider, "openrouter");
    assert_eq!(
        embedding.model,
        docsbase_memory::hybrid::config::DEFAULT_EMBEDDING_MODEL
    );
    assert_eq!(embedding.dimensions, 1024);
    assert!(embedding.normalize);
    let reranker = config.hybrid_cfg.reranker.expect("reranker stage");
    assert_eq!(reranker.model, "cohere/rerank-v3.5");
    assert_eq!(reranker.window, 50);
}

#[test]
fn hybrid_ranking_parses_rrf_and_defaults_to_lexical() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid]\nranking = \"rrf\"\n\n[hybrid.embedding]\ndimensions = 512\n",
    );
    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert_eq!(
        config.hybrid_cfg.ranking,
        docsbase_memory::hybrid::Ranking::Rrf
    );

    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");
    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert_eq!(
        config.hybrid_cfg.ranking,
        docsbase_memory::hybrid::Ranking::Lexical,
        "the bool shorthand keeps the default ordering"
    );
}

#[test]
fn hybrid_ranking_unknown_value_is_a_config_error() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid]\nranking = \"dense\"\n",
    );
    let err = Config::load_from(config_dir.path(), None).expect_err("unknown ranking");
    assert!(
        err.to_string().contains("parse") && err.to_string().contains("did not match"),
        "unknown ranking must fail parsing (serde untagged collapses the field name): {err}"
    );
}

#[test]
fn hybrid_ranking_rrf_requires_the_embedding_stage() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid]\nranking = \"rrf\"\n\n[hybrid.reranker]\n",
    );
    let err = Config::load_from(config_dir.path(), None).expect_err("rrf without embedding");
    assert!(
        matches!(err, docsbase_memory::error::Error::Admission { ref message } if message.contains("rrf")),
        "{err}"
    );
}

#[test]
fn hybrid_table_parses_stages_with_defaults() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        r#"
[hybrid]
enabled = true

[hybrid.embedding]
model = "qwen/qwen3-embedding-4b"
dimensions = 2048
normalize = false

[hybrid.reranker]
window = 25
"#,
    );

    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert!(config.hybrid && config.hybrid_cfg.enabled);
    let embedding = config.hybrid_cfg.embedding.expect("embedding");
    assert_eq!(embedding.provider, "openrouter", "provider defaults");
    assert_eq!(embedding.model, "qwen/qwen3-embedding-4b");
    assert_eq!(embedding.dimensions, 2048);
    assert!(!embedding.normalize);
    let reranker = config.hybrid_cfg.reranker.expect("reranker");
    assert_eq!(reranker.provider, "openrouter");
    assert_eq!(
        reranker.model,
        docsbase_memory::hybrid::config::DEFAULT_RERANK_MODEL
    );
    assert_eq!(reranker.window, 25);
}

#[test]
fn hybrid_table_defaults_enabled_true_and_sections_are_optional() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid.embedding]\ndimensions = 512\n",
    );
    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert!(config.hybrid_cfg.enabled, "table presence = enable intent");
    assert!(config.hybrid_cfg.embedding.is_some());
    assert!(
        config.hybrid_cfg.reranker.is_none(),
        "absent section = stage off"
    );

    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid]\nenabled = true\n",
    );
    let config = Config::load_from(config_dir.path(), None).expect("load");
    assert!(config.hybrid && config.hybrid_cfg.enabled);
    assert!(config.hybrid_cfg.embedding.is_none());
    assert!(config.hybrid_cfg.reranker.is_none());
}

#[test]
fn hybrid_window_and_provider_are_admission_errors() {
    for window in [5, 101] {
        let config_dir = TempDir::new().expect("tempdir");
        write(
            config_dir.path(),
            "config.toml",
            &format!("[hybrid.reranker]\nwindow = {window}\n"),
        );
        let error = Config::load_from(config_dir.path(), None).expect_err("window bounds");
        assert!(
            matches!(&error, docsbase_memory::error::Error::Admission { message } if message.contains("window")),
            "{error}"
        );
    }

    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid.embedding]\nprovider = \"cohere\"\n",
    );
    let error = Config::load_from(config_dir.path(), None).expect_err("provider allowlist");
    assert!(
        matches!(&error, docsbase_memory::error::Error::Admission { message } if message.contains("openrouter")),
        "{error}"
    );
}

#[test]
fn hybrid_table_unknown_keys_are_rejected() {
    let config_dir = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "[hybrid.embedding]\nfoo = 1\n",
    );
    let error = Config::load_from(config_dir.path(), None).expect_err("unknown key");
    assert!(
        matches!(error, docsbase_memory::error::Error::Internal { .. }),
        "unknown keys are parse errors, not admission"
    );
}

#[test]
fn project_hybrid_replaces_global_wholesale() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");
    write(
        project.path(),
        ".docsbase.toml",
        "[hybrid]\nenabled = false\n",
    );

    let config = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    assert!(!config.hybrid, "project table replaces the global value");
    assert!(config.hybrid_cfg.embedding.is_none());
    assert!(config.hybrid_cfg.reranker.is_none());
}

#[test]
fn config_payload_includes_resolved_stages() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");
    let config = Config::load_from(config_dir.path(), None).expect("load");

    let payload = docsbase_memory::cli::config_payload(&config);
    assert_eq!(payload["hybrid"], serde_json::json!(true));
    assert_eq!(payload["hybrid_config"]["enabled"], serde_json::json!(true));
    assert_eq!(
        payload["hybrid_config"]["embedding"]["model"],
        serde_json::json!(docsbase_memory::hybrid::config::DEFAULT_EMBEDDING_MODEL)
    );
    assert_eq!(
        payload["hybrid_config"]["reranker"]["window"],
        serde_json::json!(50)
    );
}
