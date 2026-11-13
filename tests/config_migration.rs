use docsbase_memory::config::Config;
use docsbase_memory::config::resolve::preview_sources;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::config::source::{PreviewConflict, SourceLayer};
use docsbase_memory::hybrid::config::Ranking;

fn global(text: &str) -> SourceLayer {
    SourceLayer::parse(SourceId::Global("global/config.toml".into()), text).unwrap()
}
fn project(text: &str) -> SourceLayer {
    SourceLayer::parse(SourceId::Project("project/.docsbase.toml".into()), text).unwrap()
}
const CUSTOM: &str = r#"
[hybrid]
enabled=true
ranking="rrf"
[hybrid.embedding]
model="custom/embed"
dimensions=32
normalize=false
[hybrid.reranker]
model="custom/rerank"
window=70
[hybrid.cache]
max_bytes=4096
"#;

#[test]
fn versioned_preview_preserves_v1_replacement_and_v2_inheritance() {
    let g = global(CUSTOM);
    let p = project("config_version=2\n[hybrid.reranker]\nwindow=20\n");
    let merged = preview_sources(&g, Some(&p)).unwrap();
    let cfg = &merged.effective.settings().hybrid_cfg;
    assert_eq!(cfg.embedding.as_ref().unwrap().model, "custom/embed");
    assert_eq!(cfg.embedding.as_ref().unwrap().dimensions, 32);
    assert_eq!(cfg.reranker.as_ref().unwrap().model, "custom/rerank");
    assert_eq!(cfg.reranker.as_ref().unwrap().window, 20);
    assert_eq!(cfg.ranking, Ranking::Rrf);
    assert_eq!(
        merged
            .effective
            .provenance()
            .get("hybrid.embedding.model")
            .unwrap()
            .source,
        g.source().clone()
    );
    assert_eq!(
        merged
            .effective
            .provenance()
            .get("hybrid.reranker.window")
            .unwrap()
            .source,
        p.source().clone()
    );
    assert!(!merged.runtime_activation_available);
    let p = project("[hybrid.reranker]\nwindow=20\n");
    let replaced = preview_sources(&g, Some(&p)).unwrap();
    assert!(replaced.effective.settings().hybrid_cfg.embedding.is_none());
    assert_eq!(
        replaced.effective.settings().hybrid_cfg.ranking,
        Ranking::Lexical
    );
    assert_ne!(
        replaced
            .effective
            .settings()
            .hybrid_cfg
            .reranker
            .as_ref()
            .unwrap()
            .model,
        "custom/rerank"
    );
}

#[test]
fn versioned_preview_distinguishes_inherit_disable_enable_and_bool_reset() {
    let g = global(CUSTOM);
    let disabled =
        project("config_version=2\n[hybrid]\nranking='lexical'\nembedding=false\nreranker=false\n");
    let cfg = preview_sources(&g, Some(&disabled)).unwrap();
    assert!(cfg.effective.settings().hybrid_cfg.enabled);
    assert!(cfg.effective.settings().hybrid_cfg.embedding.is_none());
    assert!(cfg.effective.settings().hybrid_cfg.reranker.is_none());
    for version in [1, 2] {
        let p = project(&format!("config_version={version}\nhybrid=true\n"));
        let cfg = preview_sources(&g, Some(&p)).unwrap();
        assert_eq!(
            cfg.effective.settings().hybrid_cfg,
            docsbase_memory::hybrid::config::HybridCfg::enabled_with_defaults()
        );
        let p = project(&format!("config_version={version}\nhybrid=false\n"));
        let cfg = preview_sources(&g, Some(&p)).unwrap();
        assert_eq!(
            cfg.effective.settings().hybrid_cfg,
            Config::default().hybrid_cfg
        );
    }
    let empty = global("config_version=2\n");
    let p = project("config_version=2\n[hybrid]\nenabled=true\n[hybrid.embedding]\n");
    let cfg = preview_sources(&empty, Some(&p)).unwrap();
    assert!(cfg.effective.settings().hybrid_cfg.embedding.is_some());
    assert!(cfg.effective.settings().hybrid_cfg.reranker.is_none());
}

#[test]
fn versioned_preview_reports_legacy_scope_conflict_under_v2_global() {
    let g = global("config_version=2\nhybrid=true\n[vector_cache]\nmax_bytes=8192\n");
    let p = project("[hybrid.cache]\nmax_bytes=1024\n");
    let preview = preview_sources(&g, Some(&p)).unwrap();
    assert_eq!(preview.legacy_vector_max_bytes, 1024);
    assert_eq!(
        preview.effective.settings().hybrid_cfg.cache.max_bytes,
        8192
    );
    assert!(preview.effective.settings().hybrid_cfg.embedding.is_none());
    assert!(matches!(
        preview.conflicts.as_slice(),
        [PreviewConflict::LegacyProjectVectorBudget { .. }]
    ));
    let p = project("hybrid=false\n");
    let preview = preview_sources(&g, Some(&p)).unwrap();
    assert!(!preview.effective.settings().hybrid_cfg.enabled);
    assert_eq!(
        preview.effective.settings().hybrid_cfg.cache.max_bytes,
        8192
    );
    assert_eq!(
        preview.conflicts,
        [] as [docsbase_memory::config::source::PreviewConflict; 0]
    );
}

#[test]
fn versioned_preview_rejects_unknown_versions_keys_scopes_and_redacts_input() {
    for text in [
        "config_version=3",
        "config_version='2'",
        "config_version=2\nunknown=1",
        "config_version=2\n[hybrid.cache]\nmax_bytes=10",
        "config_version=2\n[hybrid]\nembedding=true",
        "config_version=2\n[vector_cache]\nmax_bytes=0",
    ] {
        assert!(
            SourceLayer::parse(SourceId::Global("g".into()), text)
                .and_then(|g| preview_sources(&g, None))
                .is_err(),
            "{text}"
        );
    }
    for text in [
        "config_version=2\n[vector_cache]\nmax_bytes=5",
        "config_version=2\n[providers.openrouter]\nenabled=true",
        "config_version=2\n[search]\nactive=5",
    ] {
        assert!(
            SourceLayer::parse(SourceId::Project("p".into()), text).is_err(),
            "{text}"
        );
    }
    let err =
        SourceLayer::parse(SourceId::Global("g".into()), "api_key='secret-sentinel'").unwrap_err();
    assert!(!format!("{err:?} {err}").contains("secret-sentinel"));
    let layer = global("# secret-sentinel in a comment\nhybrid=false\n");
    assert!(!format!("{layer:?}").contains("secret-sentinel"));
}

#[test]
fn ordinary_loader_does_not_activate_preview_v2() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        "config_version=2\nhybrid=true\n",
    )
    .unwrap();
    assert!(Config::global_from(root.path()).is_err());
}

use docsbase_memory::config::migrate::{
    MigrationBlocker, ProjectObservation, VectorBudgetDecision, draft_migration,
};

fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, Option<Vec<u8>>)>,
    ) {
        let mut paths: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                out.push((path.strip_prefix(root).unwrap().to_owned(), None));
                visit(root, &path, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(std::fs::read(path).unwrap()),
                ));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn migration_draft_preserves_actual_v1_settings_and_never_writes_sources() {
    let temp = tempfile::tempdir().unwrap();
    let global_dir = temp.path().join("global");
    std::fs::create_dir(&global_dir).unwrap();
    let global_path = global_dir.join("config.toml");
    std::fs::write(&global_path, CUSTOM).unwrap();
    let mut registry = Vec::new();
    let mut observations = Vec::new();
    let mut before = Vec::new();
    for (name, text) in [
        ("one", Some("[hybrid.reranker]\nwindow=20\n")),
        ("two", Some("[hybrid.cache]\nmax_bytes=8192\n")),
        ("missing", None),
    ] {
        let root = temp.path().join(name);
        std::fs::create_dir(&root).unwrap();
        let path = root.join(".docsbase.toml");
        registry.push(path.clone());
        if let Some(text) = text {
            std::fs::write(&path, text).unwrap();
            observations.push(ProjectObservation::Loaded(
                SourceLayer::parse(SourceId::Project(path.clone()), text)
                    .unwrap()
                    .into(),
            ));
        } else {
            observations.push(ProjectObservation::Missing(path.clone()));
        }
        before.push((path, Config::load_from(&global_dir, Some(&root)).unwrap()));
    }
    std::fs::write(temp.path().join("existing-state"), b"unchanged state bytes").unwrap();
    let source = SourceLayer::parse(SourceId::Global(global_path), CUSTOM).unwrap();
    let files = snapshot(temp.path());
    let blocked = draft_migration(&source, &registry, &observations, None).unwrap();
    assert!(
        blocked
            .blockers
            .contains(&MigrationBlocker::VectorBudgetDecisionRequired)
    );
    assert_eq!(
        blocked.edits,
        [] as [docsbase_memory::config::migrate::SourceEdit; 0]
    );
    let draft = draft_migration(
        &source,
        &registry,
        &observations,
        Some(VectorBudgetDecision::UseBytes(16384)),
    )
    .unwrap();
    assert!(draft.source_proposal_complete());
    assert!(!draft.runtime_activation_available);
    assert_eq!(draft.selected_vector_max_bytes, Some(16384));
    assert_eq!(draft.edits.len(), 3);
    assert_eq!(draft.budgets.len(), 4);
    assert_ne!(
        draft.warnings,
        [] as [docsbase_memory::config::migrate::MigrationWarning; 0]
    );
    let gedit = draft
        .edits
        .iter()
        .find(|e| matches!(e.source, SourceId::Global(_)))
        .unwrap();
    let proposed_global = SourceLayer::parse(gedit.source.clone(), &gedit.after).unwrap();
    for (path, mut expected) in before {
        let proposed = draft
            .edits
            .iter()
            .find(|e| e.source == SourceId::Project(path.clone()))
            .map(|e| SourceLayer::parse(e.source.clone(), &e.after).unwrap());
        let actual = preview_sources(&proposed_global, proposed.as_ref()).unwrap();
        expected.hybrid_cfg.cache.max_bytes = 16384;
        assert_eq!(actual.effective.settings(), &expected);
        assert_eq!(
            actual.conflicts,
            [] as [docsbase_memory::config::source::PreviewConflict; 0]
        );
    }
    for edit in &draft.edits {
        assert_eq!(
            edit.expected_source_hash,
            blake3::hash(edit.before.as_bytes()).to_hex().to_string()
        );
    }
    assert_eq!(snapshot(temp.path()), files);
}

#[test]
fn migration_requires_complete_explicit_registry_observations() {
    let g = global(CUSTOM);
    let a = std::path::PathBuf::from("a/.docsbase.toml");
    let b = std::path::PathBuf::from("b/.docsbase.toml");
    let registry = vec![a.clone(), b.clone()];
    let observations = vec![ProjectObservation::Missing(a.clone())];
    let draft = draft_migration(
        &g,
        &registry,
        &observations,
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
    )
    .unwrap();
    assert_eq!(
        draft.blockers,
        vec![MigrationBlocker::UninspectedProject(b)]
    );
    assert!(!draft.source_proposal_complete());
    assert_eq!(
        draft.edits,
        [] as [docsbase_memory::config::migrate::SourceEdit; 0]
    );
    assert!(draft_migration(&g, &[a.clone(), a.clone()], &observations, None).is_err());
    assert!(
        draft_migration(
            &g,
            std::slice::from_ref(&a),
            &[
                ProjectObservation::Missing(a.clone()),
                ProjectObservation::Missing(a.clone())
            ],
            None
        )
        .is_err()
    );
    assert!(draft_migration(&g, &[], &observations, None).is_err());
    assert!(draft_migration(&g, &[], &[], Some(VectorBudgetDecision::UseBytes(0))).is_err());
}

#[test]
fn migration_preserves_existing_v2_inheritance_and_bool_shorthands() {
    let g = global("config_version=2\nhybrid=true\n[vector_cache]\nmax_bytes=4096\n");
    let partial = SourceLayer::parse(
        SourceId::Project("partial/.docsbase.toml".into()),
        "config_version=2\n[hybrid.reranker]\nwindow=20\n",
    )
    .unwrap();
    let reset = SourceLayer::parse(
        SourceId::Project("reset/.docsbase.toml".into()),
        "hybrid=false\n",
    )
    .unwrap();
    let old = SourceLayer::parse(
        SourceId::Project("old/.docsbase.toml".into()),
        "[hybrid.cache]\nmax_bytes=8192\n",
    )
    .unwrap();
    let registry = vec![
        "partial/.docsbase.toml".into(),
        "reset/.docsbase.toml".into(),
        "old/.docsbase.toml".into(),
    ];
    let draft = draft_migration(
        &g,
        &registry,
        &[
            ProjectObservation::Loaded(partial.into()),
            ProjectObservation::Loaded(reset.into()),
            ProjectObservation::Loaded(old.into()),
        ],
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
    )
    .unwrap();
    assert!(draft.source_proposal_complete());
    // Legacy bool-reset budgets are observed too; the suggestion is not just explicit keys.
    assert_eq!(draft.selected_vector_max_bytes, Some(8192));
    let edit = draft
        .edits
        .iter()
        .find(|e| e.source == SourceId::Project("partial/.docsbase.toml".into()))
        .unwrap();
    let text: toml::Value = toml::from_str(&edit.after).unwrap();
    assert!(text["hybrid"].get("embedding").is_none());
    let edit = draft
        .edits
        .iter()
        .find(|e| e.source == SourceId::Project("reset/.docsbase.toml".into()))
        .unwrap();
    let text: toml::Value = toml::from_str(&edit.after).unwrap();
    assert_eq!(text["hybrid"].as_bool(), Some(false));
    assert!(draft.scope_conflicts.iter().any(|c| matches!(
        c,
        PreviewConflict::LegacyProjectVectorBudget {
            max_bytes: 8192,
            ..
        }
    )));
}

#[test]
fn migration_v2_budget_does_not_reintroduce_legacy_defaults() {
    let g = global("config_version=2\nhybrid=true\n[vector_cache]\nmax_bytes=4096\n");
    let p = project("config_version=2\nhybrid=false\n");
    let registry: [std::path::PathBuf; 2] = [
        "project/.docsbase.toml".into(),
        "missing/.docsbase.toml".into(),
    ];
    let observations = [
        ProjectObservation::Loaded(p.into()),
        ProjectObservation::Missing(registry[1].clone()),
    ];
    let draft = draft_migration(
        &g,
        &registry,
        &observations,
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
    )
    .unwrap();
    assert_eq!(draft.suggested_vector_max_bytes, 4096);
    assert_eq!(draft.selected_vector_max_bytes, Some(4096));
    assert!(draft.budgets.iter().all(|row| row.max_bytes == 4096));
    assert_eq!(
        draft.budget_changes,
        [] as [docsbase_memory::config::migrate::BudgetChange; 0]
    );
    let reordered = draft_migration(
        &g,
        &[registry[1].clone(), registry[0].clone()],
        &[observations[1].clone(), observations[0].clone()],
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
    )
    .unwrap();
    assert_eq!(draft.edits, reordered.edits);
    assert_eq!(draft.budgets, reordered.budgets);
}

#[test]
fn preview_preserves_embedding_overrides_and_validates_each_layer() {
    let g = global(CUSTOM);
    let p = project("config_version=2\n[hybrid.embedding]\ndimensions=64\n");
    let preview = preview_sources(&g, Some(&p)).unwrap();
    let stage = preview
        .effective
        .settings()
        .hybrid_cfg
        .embedding
        .as_ref()
        .unwrap();
    assert_eq!(
        (stage.model.as_str(), stage.dimensions, stage.normalize),
        ("custom/embed", 64, false)
    );
    assert_eq!(
        preview
            .effective
            .provenance()
            .get("hybrid.embedding.dimensions")
            .unwrap()
            .source,
        p.source().clone()
    );
    assert_eq!(
        preview
            .effective
            .provenance()
            .get("hybrid.embedding.model")
            .unwrap()
            .source,
        g.source().clone()
    );
    let switch = project("config_version=2\n[hybrid.embedding]\nprovider='fastembed'\n");
    // An inherited arbitrary model/raw normalization cannot become a valid local
    // model merely by changing provider (including when fastembed is compiled).
    assert!(preview_sources(&g, Some(&switch)).is_err());
    let invalid = global("config_version=2\nmax_file_size=0\n");
    let repair = project("config_version=2\nmax_file_size=200\n");
    assert!(preview_sources(&invalid, Some(&repair)).is_err());
    assert!(preview_sources(&project(""), None).is_err());
    assert!(preview_sources(&g, Some(&global(""))).is_err());
}

#[test]
fn source_limits_and_diagnostics_cover_semantic_validation() {
    let oversized =
        " ".repeat(usize::try_from(docsbase_memory::limits::MAX_STATE_BYTES).unwrap() + 1);
    assert!(SourceLayer::parse(SourceId::Global("g".into()), &oversized).is_err());
    assert!(SourceLayer::parse(SourceId::Defaults, "").is_err());
    let g = global("config_version=2\n[hybrid.embedding]\nprovider='secret-sentinel'\n");
    let err = preview_sources(&g, None).unwrap_err();
    assert!(!format!("{err:?} {err}").contains("secret-sentinel"));
    let g = global("# secret-sentinel\nhybrid=false\n");
    assert!(draft_migration(&g, &[], &[], Some(VectorBudgetDecision::UseBytes(u64::MAX))).is_err());
    let draft = draft_migration(&g, &[], &[], Some(VectorBudgetDecision::UseBytes(2048))).unwrap();
    assert!(!format!("{draft:?}").contains("secret-sentinel"));
    assert!(draft.edits[0].before.contains("secret-sentinel"));
}

#[test]
fn versioned_preview_respects_local_build_capability_without_provisioning() {
    let g = global(
        "config_version=2\n[hybrid.embedding]\nprovider='fastembed'\nmodel='multilingual-e5-small'\n",
    );
    let result = preview_sources(&g, None);
    #[cfg(feature = "hybrid-local")]
    {
        let cfg = result.unwrap();
        let stage = cfg
            .effective
            .settings()
            .hybrid_cfg
            .embedding
            .as_ref()
            .unwrap();
        assert_eq!(stage.dimensions, 384);
        assert!(stage.normalize);
        assert!(!cfg.runtime_activation_available);
    }
    #[cfg(not(feature = "hybrid-local"))]
    assert!(result.is_err());
}

#[test]
fn preview_vector_budget_provenance_matches_source_version_and_scope() {
    use docsbase_memory::config::schema::{Scope, Unit};
    let g = global(CUSTOM);
    let legacy = preview_sources(&g, Some(&project("config_version=2\nhybrid=false\n"))).unwrap();
    assert_eq!(
        legacy
            .effective
            .provenance()
            .get("hybrid.cache.max_bytes")
            .unwrap()
            .scope,
        Scope::Project
    );
    assert!(
        legacy
            .effective
            .provenance()
            .get("vector_cache.max_bytes")
            .is_none()
    );
    for (text, expected_source) in [
        (
            "config_version=2\nhybrid=true\n[vector_cache]\nmax_bytes=4096\n",
            g.source().clone(),
        ),
        ("config_version=2\n", SourceId::Defaults),
    ] {
        let g = global(text);
        let p = project("hybrid=false\n");
        let strict = preview_sources(&g, Some(&p)).unwrap();
        let origins = strict.effective.provenance();
        let cap = origins
            .get("vector_cache.max_bytes")
            .expect("strict budget uses the v2 key");
        assert_eq!(cap.source, expected_source);
        assert_eq!(cap.scope, Scope::Daemon);
        assert_eq!(cap.unit, Unit::Bytes);
        assert!(origins.get("hybrid.cache.max_bytes").is_none());
    }
}
