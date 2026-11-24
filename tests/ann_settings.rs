//! `CF8a`: owner resolution and the real built/persisted ANN consumer.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::resolve_component;
use docsbase_memory::config::schema::{
    ArtifactFacts, ComponentImpact, Scope, SettingsSpec, SourceId, Unit,
};
use docsbase_memory::error::Error;
use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::ann_settings::{AnnSearchPatch, AnnSearchSettings, AnnSearchSpec};
use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore};

#[test]
fn default_policy_preserves_beams_and_records_project_scope() {
    let resolved = resolve_component::<AnnSearchSpec>(&DefaultsContext::standalone(), []).unwrap();
    let settings = resolved.settings();
    assert_eq!(
        (
            settings.ef_base(),
            settings.ef_max(),
            settings.fetch_multiplier()
        ),
        (64, 4096, 4)
    );
    for (fetch, beam) in [(0, 64), (10, 64), (50, 200), (1000, 4000), (4096, 4096)] {
        assert_eq!(settings.beam_for(fetch).unwrap(), beam);
    }
    let origins: Vec<_> = resolved.provenance().iter().collect();
    assert_eq!(origins.len(), 3);
    for (key, origin) in origins {
        assert!(key.starts_with("ann.search."));
        assert_eq!(origin.source, SourceId::Defaults);
        assert_eq!(origin.scope, Scope::Project);
        assert_eq!(origin.unit, Unit::Count);
    }
}

#[test]
fn layered_policy_tracks_each_source_without_changing_the_parent() {
    let context = DefaultsContext::standalone();
    let global = SourceId::Global("global.toml".into());
    let project = SourceId::Project(".docsbase.toml".into());
    let resolved = resolve_component::<AnnSearchSpec>(
        &context,
        [
            (
                global.clone(),
                AnnSearchPatch {
                    ef_base: Some(8),
                    ef_max: Some(128),
                    ..Default::default()
                },
            ),
            (
                project.clone(),
                AnnSearchPatch {
                    fetch_multiplier: Some(2),
                    ..Default::default()
                },
            ),
        ],
    )
    .unwrap();
    assert_eq!(resolved.settings().beam_for(7).unwrap(), 14);
    assert_eq!(
        resolved
            .provenance()
            .get("ann.search.ef_base")
            .unwrap()
            .source,
        global
    );
    assert_eq!(
        resolved
            .provenance()
            .get("ann.search.ef_max")
            .unwrap()
            .source,
        global
    );
    assert_eq!(
        resolved
            .provenance()
            .get("ann.search.fetch_multiplier")
            .unwrap()
            .source,
        project
    );
    let defaults = resolve_component::<AnnSearchSpec>(&context, []).unwrap();
    assert_eq!(defaults.settings().beam_for(7).unwrap(), 64);
    assert_eq!(
        AnnSearchSpec::impact(None, resolved.settings(), &ArtifactFacts::default()),
        ComponentImpact::InitializeRuntime
    );
    assert_eq!(
        AnnSearchSpec::impact(
            Some(resolved.settings()),
            resolved.settings(),
            &ArtifactFacts::default()
        ),
        ComponentImpact::Unchanged
    );
    assert_eq!(
        AnnSearchSpec::impact(
            Some(defaults.settings()),
            resolved.settings(),
            &ArtifactFacts::default()
        ),
        ComponentImpact::ReconfigureRuntime
    );
}

#[test]
fn invalid_domains_are_rejected_by_constructor_and_resolver() {
    for (base, max, multiplier) in [
        (0, 64, 4),
        (1, 0, 4),
        (65, 64, 4),
        (1, 64, 0),
        (1, 4097, 4),
        (1, usize::MAX, 1),
    ] {
        assert!(matches!(
            AnnSearchSettings::new(base, max, multiplier),
            Err(Error::Admission { .. })
        ));
        let resolved = resolve_component::<AnnSearchSpec>(
            &DefaultsContext::standalone(),
            [(
                SourceId::Cli,
                AnnSearchPatch {
                    ef_base: Some(base),
                    ef_max: Some(max),
                    fetch_multiplier: Some(multiplier),
                },
            )],
        );
        assert!(matches!(resolved, Err(Error::Admission { .. })));
    }
}

#[test]
fn beam_saturates_without_wrapping_and_refuses_fetch_above_policy() {
    let settings = AnnSearchSettings::new(1, 128, usize::MAX).unwrap();
    assert_eq!(settings.beam_for(0).unwrap(), 1);
    assert_eq!(settings.beam_for(1).unwrap(), 128);
    assert_eq!(settings.beam_for(128).unwrap(), 128);
    assert!(matches!(
        settings.beam_for(129),
        Err(Error::Admission { .. })
    ));
    assert!(matches!(
        settings.beam_for(usize::MAX),
        Err(Error::Admission { .. })
    ));
    let tight = AnnSearchSettings::new(1, 1, 1).unwrap();
    assert_eq!(tight.beam_for(1).unwrap(), 1);
}

fn rows() -> Vec<(u64, Arc<[f32]>)> {
    vec![
        (3, Arc::from([1.0, 0.0])),
        (7, Arc::from([0.0, 1.0])),
        (9, Arc::from([-1.0, 0.0])),
    ]
}
fn persisted_files(dir: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file());
            (entry.file_name(), std::fs::read(entry.path()).unwrap())
        })
        .collect()
}

#[test]
fn built_and_loaded_search_apply_policy_without_rewriting_graph() {
    let data = rows();
    let built = AnnIndex::build(&data, 2).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = AnnStore::new(temp.path().to_path_buf());
    let key = AnnKey {
        project_id: 1,
        generation: 2,
        model_id: "test@d2".into(),
        dim: 2,
        chunk_count: data.len(),
        chunk_set_digest: "cf8a".into(),
        format_version: ANN_FORMAT_VERSION,
    };
    let slot = built.dump(&store, &key).unwrap();
    let before = persisted_files(&slot.dir);
    let loaded = AnnIndex::load(&slot).unwrap();
    let defaults = resolve_component::<AnnSearchSpec>(&DefaultsContext::standalone(), []).unwrap();
    let tight = AnnSearchSettings::new(1, 2, 1).unwrap();
    for graph in [&built, &loaded] {
        let ordinary = graph.search(&[1.0, 0.0], 2).unwrap();
        assert_eq!(
            ordinary,
            graph
                .search_with_settings(&[1.0, 0.0], 2, defaults.settings())
                .unwrap()
        );
        let hits = graph.search_with_settings(&[1.0, 0.0], 1, &tight).unwrap();
        assert_eq!(hits, vec![(3, 1.0)]);
        // Small over-cap fetch tests admission without risking an OOM mutation.
        assert!(matches!(
            graph.search_with_settings(&[1.0, 0.0], 3, &tight),
            Err(Error::Admission { .. })
        ));
        // The legacy wrapper historically allows fetch above its soft ef cap.
        assert_ne!(
            graph.search(&[1.0, 0.0], 4097).unwrap(),
            Vec::<(u64, f32)>::new()
        );
        assert!(
            graph
                .search_with_settings(&[1.0, 0.0], 4097, defaults.settings())
                .is_err()
        );
        assert_eq!(
            graph.search_with_settings(&[1.0, 0.0], 0, &tight).unwrap(),
            Vec::<(u64, f32)>::new()
        );
        assert!(graph.search_with_settings(&[], 0, &tight).is_err());
        assert!(graph.search_with_settings(&[1.0], 0, &tight).is_err());
    }
    assert_eq!(persisted_files(&slot.dir), before);
    // Drop loaded readers before the temporary store on Windows.
    drop(loaded);
}

#[test]
fn direct_trait_overlay_cannot_escape_invalid_settings_or_partial_origins() {
    let resolved = resolve_component::<AnnSearchSpec>(&DefaultsContext::standalone(), []).unwrap();
    let defaults = *resolved.settings();
    let mut settings = defaults;
    let mut origins = resolved.provenance().clone();
    let before = origins.clone();
    let result = AnnSearchSpec::overlay(
        &mut settings,
        AnnSearchPatch {
            ef_base: Some(1),
            ef_max: Some(4097),
            ..Default::default()
        },
        &defaults,
        &SourceId::Cli,
        &mut origins,
    );
    assert!(matches!(result, Err(Error::Admission { .. })));
    assert_eq!(settings, defaults);
    assert_eq!(origins, before);
}
