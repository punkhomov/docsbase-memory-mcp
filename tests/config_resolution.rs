use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use docsbase_memory::config::bootstrap::{BootstrapContext, DefaultsContext, EntrypointFacts};
use docsbase_memory::config::resolve::{Resolution, resolve_component};
use docsbase_memory::config::schema::{
    ArtifactFacts, Capabilities, ComponentImpact, FieldSpec, Provenance, Scope, SettingsSpec,
    SourceId, Unit,
};
use docsbase_memory::config::{Config, ConfigOverrides};
use docsbase_memory::hybrid::config::{HybridPatch, HybridSpec};
use docsbase_memory::store::settings::{DbPatch, DbSpec, ReadPoolPatch, ReadPoolSpec};
use tempfile::TempDir;

fn layer(dir: &TempDir, name: &str, text: &str) {
    std::fs::write(dir.path().join(name), text).expect("fixture");
}

#[test]
fn v1_hybrid_layer_replaces_whole_value() {
    let global = TempDir::new().expect("global");
    let project = TempDir::new().expect("project");
    layer(&global, "config.toml", "hybrid = true\n");
    layer(
        &project,
        ".docsbase.toml",
        "[hybrid.cache]\nmax_bytes = 4096\n",
    );
    let resolved = docsbase_memory::config::resolve::load_from(global.path(), Some(project.path()))
        .expect("resolve");
    assert!(resolved.settings().hybrid_cfg.enabled);
    assert!(resolved.settings().hybrid_cfg.embedding.is_none());
    assert!(resolved.settings().hybrid_cfg.reranker.is_none());
    assert_eq!(resolved.settings().hybrid_cfg.cache.max_bytes, 4096);
    assert_eq!(
        &Config::load_from(global.path(), Some(project.path())).expect("legacy"),
        resolved.settings()
    );
    assert_eq!(
        resolved
            .provenance()
            .get("hybrid.cache.max_bytes")
            .expect("origin")
            .source,
        SourceId::Project(project.path().join(".docsbase.toml"))
    );
}

#[test]
fn absent_and_disabled_stage_are_distinct() {
    let context = DefaultsContext::standalone();
    let mut inherited = Resolution::<HybridSpec>::new(&context);
    inherited
        .apply(
            &SourceId::Global("global.toml".into()),
            HybridPatch::Bool(true),
        )
        .expect("global");
    let inherited = inherited.finish().expect("inherited");
    assert!(inherited.settings().embedding.is_some());
    let mut disabled = Resolution::<HybridSpec>::from_resolved(&context, &inherited);
    disabled
        .apply(
            &SourceId::Project("project.toml".into()),
            HybridPatch::Bool(false),
        )
        .expect("disable");
    let disabled = disabled.finish().expect("disabled");
    assert!(!disabled.settings().enabled);
    assert!(disabled.settings().embedding.is_none());
}

#[test]
fn project_cannot_set_daemon_budget() {
    let ctx = DefaultsContext::standalone();
    let patch = DbPatch {
        write_busy_timeout: Some(Duration::from_secs(9)),
        ..DbPatch::default()
    };
    let global = resolve_component::<DbSpec>(
        &ctx,
        [(SourceId::Global("global.toml".into()), patch.clone())],
    )
    .expect("machine-owned setting");
    assert_eq!(global.settings().write_busy_timeout, Duration::from_secs(9));
    let error =
        resolve_component::<DbSpec>(&ctx, [(SourceId::Project("project.toml".into()), patch)])
            .expect_err("project cannot configure daemon resource");
    assert!(error.to_string().contains("scope"));
    assert_eq!(
        global
            .provenance()
            .get("store.write_busy_timeout")
            .expect("origin")
            .unit,
        Unit::Milliseconds
    );
}

#[test]
fn component_defaults_are_resolved_once() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    struct CountSpec;
    impl SettingsSpec for CountSpec {
        type Patch = u32;
        type Settings = u32;
        type Defaults = u32;
        const SCOPE: Scope = Scope::Project;
        const FIELDS: &'static [FieldSpec] =
            &[FieldSpec::new("count", Scope::Project, Unit::Count)];
        fn defaults(_: &DefaultsContext) -> u32 {
            CALLS.fetch_add(1, Ordering::SeqCst);
            7
        }
        fn initial(defaults: &u32) -> u32 {
            *defaults
        }
        fn overlay(
            value: &mut u32,
            patch: u32,
            _: &u32,
            source: &SourceId,
            origins: &mut Provenance,
        ) -> docsbase_memory::error::Result<()> {
            *value += patch;
            origins.set("count", source)
        }
        fn validate(_: &u32, _: &Capabilities) -> docsbase_memory::error::Result<()> {
            Ok(())
        }
        fn impact(_: Option<&u32>, _: &u32, _: &ArtifactFacts) -> ComponentImpact {
            ComponentImpact::Unchanged
        }
    }
    CALLS.store(0, Ordering::SeqCst);
    let resolved = resolve_component::<CountSpec>(
        &DefaultsContext::standalone(),
        [
            (SourceId::Global("g".into()), 2),
            (SourceId::Project("p".into()), 3),
        ],
    )
    .expect("resolve once");
    assert_eq!(*resolved.settings(), 12);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}

#[test]
fn invalid_env_fails_before_runtime() {
    let mut reads = Vec::new();
    let error = BootstrapContext::capture_with(4, EntrypointFacts::standalone(), |name| {
        reads.push(name);
        (name == "TOKIO_WORKER_THREADS").then(|| "secret-invalid-number".into())
    })
    .expect_err("invalid worker count must be rejected before runtime construction");
    assert!(error.to_string().contains("TOKIO_WORKER_THREADS"));
    assert!(!error.to_string().contains("secret-invalid-number"));
    assert!(!reads.contains(&"OPENROUTER_API_KEY"));
}

#[test]
fn hybrid_true_uses_both_default_stages() {
    let resolved = resolve_component::<HybridSpec>(
        &DefaultsContext::standalone(),
        [(SourceId::Cli, HybridPatch::Bool(true))],
    )
    .expect("enabled");
    assert!(resolved.settings().enabled);
    assert!(resolved.settings().embedding.is_some());
    assert!(resolved.settings().reranker.is_some());
    assert_eq!(
        resolved.settings(),
        &Config::default()
            .with_overrides(&ConfigOverrides {
                hybrid: Some(true),
                ..ConfigOverrides::default()
            })
            .hybrid_cfg
    );
}

#[test]
fn hybrid_false_clears_both_stages() {
    let resolved = resolve_component::<HybridSpec>(
        &DefaultsContext::standalone(),
        [
            (SourceId::Global("g".into()), HybridPatch::Bool(true)),
            (SourceId::Project("p".into()), HybridPatch::Bool(false)),
        ],
    )
    .expect("disabled");
    assert!(!resolved.settings().enabled);
    assert!(resolved.settings().embedding.is_none());
    assert!(resolved.settings().reranker.is_none());
}

#[test]
fn v1_table_replace_uses_empty_stage_table_base() {
    let global = TempDir::new().expect("global");
    let project = TempDir::new().expect("project");
    layer(&global, "config.toml", "hybrid = true\n");
    layer(&project, ".docsbase.toml", "[hybrid]\nenabled = true\n");
    let cfg = Config::load_from(global.path(), Some(project.path())).expect("table replacement");
    assert!(cfg.hybrid_cfg.enabled);
    assert!(cfg.hybrid_cfg.embedding.is_none());
    assert!(cfg.hybrid_cfg.reranker.is_none());
}

#[test]
fn db_and_pool_defaults_match_existing_policies() {
    let ctx = DefaultsContext::standalone();
    let db = resolve_component::<DbSpec>(&ctx, []).expect("db settings");
    assert_eq!(db.settings().write_busy_timeout, Duration::from_secs(30));
    assert_eq!(db.settings().read_busy_timeout, Duration::from_secs(5));
    assert_eq!(db.settings().wal_attempts, 3);
    assert_eq!(db.settings().wal_retry_delay, Duration::from_millis(100));
    let pool = resolve_component::<ReadPoolSpec>(&ctx, []).expect("pool settings");
    assert_eq!(pool.settings().max_connections, 4);
    assert!(
        resolve_component::<ReadPoolSpec>(
            &ctx,
            [(
                SourceId::Cli,
                ReadPoolPatch {
                    max_connections: Some(0)
                }
            )]
        )
        .is_err()
    );
}

#[test]
fn oversized_db_busy_timeout_is_admission_before_rusqlite_can_panic() {
    let ctx = DefaultsContext::standalone();
    for write in [true, false] {
        let oversized = Some(Duration::from_millis(i32::MAX as u64 + 1));
        let patch = if write {
            DbPatch {
                write_busy_timeout: oversized,
                ..DbPatch::default()
            }
        } else {
            DbPatch {
                read_busy_timeout: oversized,
                ..DbPatch::default()
            }
        };
        let error = resolve_component::<DbSpec>(&ctx, [(SourceId::Cli, patch)])
            .expect_err("rusqlite would panic on this timeout");
        assert!(error.to_string().contains("busy_timeout"));
    }
}

#[test]
fn invalid_global_cannot_be_healed_by_a_project_override() {
    for (global_text, project_text, expected) in [
        ("max_file_size = 0", "max_file_size = 8192", "max_file_size"),
        (
            "[hybrid]\nranking = 'rrf'",
            "hybrid = true",
            "requires the embedding",
        ),
    ] {
        let global = TempDir::new().expect("global");
        let project = TempDir::new().expect("project");
        layer(&global, "config.toml", global_text);
        layer(&project, ".docsbase.toml", project_text);
        let error =
            docsbase_memory::config::resolve::load_from(global.path(), Some(project.path()))
                .expect_err("v1 validates global first");
        assert!(error.to_string().contains(expected));
        assert!(Config::load_from(global.path(), Some(project.path())).is_err());
    }
}

#[test]
fn typed_project_retains_global_origins_and_legacy_snapshot_is_honest() {
    let global = TempDir::new().expect("global");
    let project = TempDir::new().expect("project");
    layer(
        &global,
        "config.toml",
        "max_file_size = 4096\nhybrid = true",
    );
    layer(&project, ".docsbase.toml", "auto_index = true");
    let snapshot = docsbase_memory::config::resolve::global_from(global.path()).expect("global");
    let effective =
        docsbase_memory::config::resolve::for_project(&snapshot, project.path()).expect("project");
    assert_eq!(
        effective
            .provenance()
            .get("max_file_size")
            .expect("origin")
            .source,
        SourceId::Global(global.path().join("config.toml"))
    );
    assert_eq!(
        effective
            .provenance()
            .get("auto_index")
            .expect("origin")
            .source,
        SourceId::Project(project.path().join(".docsbase.toml"))
    );
    assert_eq!(
        effective
            .provenance()
            .get("max_docs_per_project")
            .expect("origin")
            .source,
        SourceId::Defaults
    );
    assert!(!snapshot.settings().auto_index, "snapshot stays immutable");
    let supplied = Resolution::<HybridSpec>::from_provided(
        &DefaultsContext::standalone(),
        &snapshot.settings().hybrid_cfg,
    )
    .finish()
    .expect("provided");
    assert!(
        supplied
            .provenance()
            .iter()
            .all(|(_, field)| field.source == SourceId::ProvidedSnapshot)
    );
}

#[test]
fn failed_patch_does_not_partly_replace_settings_or_provenance() {
    use docsbase_memory::hybrid::config::{EmbeddingPatch, HybridTablePatch};
    let mut resolution = Resolution::<HybridSpec>::new(&DefaultsContext::standalone());
    resolution
        .apply(&SourceId::Global("global".into()), HybridPatch::Bool(true))
        .expect("global");
    assert!(
        resolution
            .apply(
                &SourceId::Project("project".into()),
                HybridPatch::Table(HybridTablePatch {
                    enabled: Some(false),
                    embedding: Some(EmbeddingPatch {
                        provider: Some("unknown".into()),
                        ..EmbeddingPatch::default()
                    }),
                    ..HybridTablePatch::default()
                })
            )
            .is_err()
    );
    let resolved = resolution.finish().expect("previous good value");
    assert!(resolved.settings().enabled);
    assert!(
        resolved
            .provenance()
            .iter()
            .all(|(_, field)| field.source == SourceId::Global("global".into()))
    );
}

#[test]
fn dense_policy_defaults_overrides_and_zero_disable_semantics() {
    use docsbase_memory::hybrid::dense::DenseCacheLimits;
    use docsbase_memory::hybrid::dense_settings::{DensePatch, DenseSpec};
    let ctx = DefaultsContext::standalone();
    let resolved = resolve_component::<DenseSpec>(&ctx, []).expect("dense defaults");
    let policy = resolved.settings();
    let legacy = DenseCacheLimits::default();
    assert_eq!(policy.max_vectors, legacy.max_vectors);
    assert_eq!(policy.max_bytes, legacy.max_bytes);
    assert_eq!(policy.max_projects, legacy.max_projects);
    assert_eq!(policy.min_vectors, legacy.min_vectors);
    assert_eq!(policy.build_backoff, legacy.build_backoff);
    assert_eq!(policy.vector_cache_max_bytes, legacy.vector_cache_max_bytes);
    assert_eq!(policy.max_ann_disk_bytes, legacy.max_ann_disk_bytes);
    assert_eq!(
        policy.query_cache_max_entries,
        docsbase_memory::hybrid::dense::QUERY_CACHE_MAX_ENTRIES
    );
    assert_eq!(
        policy.query_cache_max_bytes,
        docsbase_memory::hybrid::dense::QUERY_CACHE_MAX_BYTES
    );
    let overridden = resolve_component::<DenseSpec>(
        &ctx,
        [(
            SourceId::Cli,
            DensePatch {
                max_bytes: Some(8192),
                max_ann_disk_bytes: Some(0),
                build_backoff: Some(Duration::ZERO),
                ..DensePatch::default()
            },
        )],
    )
    .expect("zero disk budget means cleanup; no retry delay is valid");
    assert_eq!(overridden.settings().max_bytes, 8192);
    assert_eq!(overridden.settings().max_ann_disk_bytes, 0);
    assert_eq!(overridden.settings().build_backoff, Duration::ZERO);
    assert_eq!(overridden.settings().max_vectors, policy.max_vectors);
    assert!(
        resolve_component::<DenseSpec>(
            &ctx,
            [(
                SourceId::Project("project".into()),
                DensePatch {
                    max_bytes: Some(8192),
                    ..DensePatch::default()
                }
            )]
        )
        .is_err()
    );
}

#[test]
fn no_inactive_storage_or_dense_keys_are_accepted_in_files() {
    for text in [
        "[store]\nwal_attempts = 5",
        "[dense]\nmax_bytes = 8192",
        "[read_pool]\nmax_connections = 2",
    ] {
        let global = TempDir::new().expect("global");
        layer(&global, "config.toml", text);
        assert!(
            Config::global_from(global.path()).is_err(),
            "not advertised until real consumers: {text}"
        );
    }
}

#[test]
fn bootstrap_captures_only_whitelisted_inputs_and_never_secrets() {
    let mut reads = Vec::new();
    let captured = BootstrapContext::capture_with(6, EntrypointFacts::standalone(), |key| {
        reads.push(key);
        match key {
            "RAYON_NUM_THREADS" => Some("2".into()),
            "TOKENIZERS_PARALLELISM" => Some("OFF".into()),
            "DOCSBASE_CONFIG_DIR" => Some("relative/config".into()),
            "OPENROUTER_API_KEY" => Some("credential-sentinel".into()),
            _ => None,
        }
    })
    .expect("capture");
    assert!(
        !reads.contains(&"RAYON_RS_NUM_CPUS"),
        "unused fallback must not be read"
    );
    assert!(!reads.contains(&"OPENROUTER_API_KEY"));
    assert!(reads.iter().all(|key| {
        [
            "TOKIO_WORKER_THREADS",
            "RAYON_NUM_THREADS",
            "TOKENIZERS_PARALLELISM",
            "DOCSBASE_CACHE_DIR",
            "DOCSBASE_CONFIG_DIR",
            "DOCSBASE_DATA_DIR",
            "ORT_DYLIB_PATH",
            "DOCSBASE_DAEMON_EXE",
        ]
        .contains(key)
    }));
    assert_eq!(captured.tokio_workers.value.get(), 6);
    assert_eq!(captured.tokio_workers.source, SourceId::Os);
    assert_eq!(captured.rayon_threads.value.get(), 2);
    assert!(!captured.tokenizers_parallelism.value);
    let config_dir = captured.config_dir.as_ref().expect("config override");
    assert_eq!(
        config_dir.value,
        std::path::PathBuf::from("relative/config")
    );
    assert_eq!(
        config_dir.source,
        SourceId::Environment("DOCSBASE_CONFIG_DIR")
    );
    assert!(!format!("{captured:?}").contains("credential-sentinel"));
}

#[test]
fn bootstrap_preserves_rayon_and_tokenizers_legacy_fallbacks() {
    for (rayon, legacy, expected, reads_legacy) in [
        ("0", "2", 6, false),
        ("invalid", "2", 2, true),
        ("invalid", "0", 6, true),
    ] {
        let mut reads = Vec::new();
        let captured = BootstrapContext::capture_with(6, EntrypointFacts::standalone(), |key| {
            reads.push(key);
            match key {
                "RAYON_NUM_THREADS" => Some(rayon.into()),
                "RAYON_RS_NUM_CPUS" => Some(legacy.into()),
                "TOKENIZERS_PARALLELISM" => Some("unrecognised-legacy-true".into()),
                _ => None,
            }
        })
        .expect("legacy fallback");
        assert_eq!(captured.rayon_threads.value.get(), expected);
        assert_eq!(reads.contains(&"RAYON_RS_NUM_CPUS"), reads_legacy);
        assert!(captured.tokenizers_parallelism.value);
        assert!(!format!("{captured:?}").contains("unrecognised-legacy-true"));
    }
    for value in ["0", "NaN", "-1", ""] {
        let error = BootstrapContext::capture_with(6, EntrypointFacts::standalone(), |key| {
            (key == "TOKIO_WORKER_THREADS").then(|| value.into())
        })
        .expect_err("Tokio would panic");
        assert!(error.to_string().contains("TOKIO_WORKER_THREADS"));
    }
}

#[test]
fn compatibility_overrides_stay_infallible_until_explicit_validation() {
    let value = Config::default().with_overrides(&ConfigOverrides {
        max_file_size: Some(0),
        ..ConfigOverrides::default()
    });
    assert_eq!(value.max_file_size, 0);
    assert!(value.validate().is_err());
}

#[test]
fn explicit_missing_paths_are_not_created_during_resolution() {
    let root = TempDir::new().expect("root");
    let global = root.path().join("absent-global");
    let project = root.path().join("absent-project");
    let resolved = docsbase_memory::config::resolve::load_from(&global, Some(&project))
        .expect("defaults without I/O writes");
    assert_eq!(resolved.settings(), &Config::default());
    assert!(
        std::fs::read_dir(root.path())
            .expect("directory")
            .next()
            .is_none()
    );
}

#[test]
fn imported_hybrid_snapshot_rejects_unknown_stage_providers() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, HybridCfg, RerankerCfg};
    let snapshots = [
        HybridCfg {
            enabled: true,
            embedding: Some(EmbeddingCfg {
                provider: "unknown-embedding".into(),
                ..EmbeddingCfg::default()
            }),
            ..HybridCfg::default()
        },
        HybridCfg {
            enabled: true,
            reranker: Some(RerankerCfg {
                provider: "unknown-reranker".into(),
                ..RerankerCfg::default()
            }),
            ..HybridCfg::default()
        },
    ];
    for snapshot in snapshots {
        let error =
            Resolution::<HybridSpec>::from_provided(&DefaultsContext::standalone(), &snapshot)
                .finish()
                .expect_err("snapshot import must validate the provider just like a patch");
        assert!(error.to_string().contains("unknown provider"));
    }
}

#[test]
fn imported_fastembed_snapshot_rejects_disabled_normalization() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, HybridCfg};
    let snapshot = HybridCfg {
        enabled: true,
        embedding: Some(EmbeddingCfg {
            provider: "fastembed".into(),
            model: "multilingual-e5-small".into(),
            dimensions: 384,
            normalize: false,
        }),
        ..HybridCfg::default()
    };
    let error = Resolution::<HybridSpec>::from_provided(&DefaultsContext::standalone(), &snapshot)
        .finish()
        .expect_err("normalization is a structural provider constraint in every build");
    assert!(error.to_string().contains("normalize = true"));
}

#[cfg(feature = "hybrid-local")]
#[test]
fn imported_fastembed_snapshot_rejects_registry_shape_mismatches() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, HybridCfg, RerankerCfg};
    let embedding = EmbeddingCfg {
        provider: "fastembed".into(),
        model: "multilingual-e5-small".into(),
        dimensions: 384,
        normalize: true,
    };
    let snapshots = [
        HybridCfg {
            enabled: true,
            embedding: Some(EmbeddingCfg {
                dimensions: 128,
                ..embedding.clone()
            }),
            ..HybridCfg::default()
        },
        HybridCfg {
            enabled: true,
            embedding: Some(EmbeddingCfg {
                model: "jina-reranker-v1-turbo-en".into(),
                ..embedding.clone()
            }),
            ..HybridCfg::default()
        },
        HybridCfg {
            enabled: true,
            embedding: Some(EmbeddingCfg {
                model: "unknown-model".into(),
                ..embedding
            }),
            ..HybridCfg::default()
        },
        HybridCfg {
            enabled: true,
            reranker: Some(RerankerCfg {
                provider: "fastembed".into(),
                model: "multilingual-e5-small".into(),
                window: 50,
            }),
            ..HybridCfg::default()
        },
        HybridCfg {
            enabled: true,
            reranker: Some(RerankerCfg {
                provider: "fastembed".into(),
                model: "unknown-model".into(),
                window: 50,
            }),
            ..HybridCfg::default()
        },
    ];
    for snapshot in snapshots {
        assert!(
            Resolution::<HybridSpec>::from_provided(&DefaultsContext::standalone(), &snapshot)
                .finish()
                .is_err(),
            "registry validation must reject {snapshot:?}"
        );
    }
}

#[test]
fn imported_valid_hybrid_snapshots_match_patch_feature_policy() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, HybridCfg, RerankerCfg};
    let context = DefaultsContext::standalone();
    let openrouter = HybridCfg::enabled_with_defaults();
    let resolved = Resolution::<HybridSpec>::from_provided(&context, &openrouter)
        .finish()
        .expect("pure OpenRouter settings resolve without credentials or runtime capability");
    assert_eq!(resolved.settings(), &openrouter);
    let fastembed = HybridCfg {
        enabled: true,
        embedding: Some(EmbeddingCfg {
            provider: "fastembed".into(),
            model: "multilingual-e5-small".into(),
            dimensions: 384,
            normalize: true,
        }),
        reranker: Some(RerankerCfg {
            provider: "fastembed".into(),
            model: "jina-reranker-v1-turbo-en".into(),
            window: 50,
        }),
        ..HybridCfg::default()
    };
    let result = Resolution::<HybridSpec>::from_provided(&context, &fastembed).finish();
    #[cfg(feature = "hybrid-local")]
    assert_eq!(
        result
            .expect("catalog validation needs neither installed runtime nor model files")
            .settings(),
        &fastembed
    );
    #[cfg(not(feature = "hybrid-local"))]
    assert!(
        result
            .expect_err("same build-feature admission as explicit fastembed file patches")
            .to_string()
            .contains("hybrid-local")
    );
}

#[test]
fn stricter_typed_snapshot_validation_preserves_plain_config_compatibility() {
    use docsbase_memory::hybrid::config::{EmbeddingCfg, HybridCfg};
    let snapshot = Config {
        hybrid: true,
        hybrid_cfg: HybridCfg {
            enabled: true,
            embedding: Some(EmbeddingCfg {
                provider: "unknown-legacy-provider".into(),
                ..EmbeddingCfg::default()
            }),
            ..HybridCfg::default()
        },
        ..Config::default()
    };
    snapshot
        .validate()
        .expect("legacy Config validation remains structural");
    let project = TempDir::new().expect("project");
    assert_eq!(
        Config::for_project(&snapshot, project.path())
            .expect("plain legacy snapshot adapter preserves prior acceptance"),
        snapshot
    );
}
