use clap::Parser;
use std::path::PathBuf;

use docsbase_memory::config::bootstrap::DefaultsContext;
use docsbase_memory::config::resolve::{Resolution, resolve_component};
use docsbase_memory::config::schema::{Scope, SourceId, Unit};
use docsbase_memory::config::{Config, source::SourceLayer};
use docsbase_memory::daemon::launch::LaunchSnapshot;
use docsbase_memory::daemon::server::Shared;
use docsbase_memory::daemon::session::SessionRegistry;
use docsbase_memory::daemon::settings::{DaemonPatch, DaemonProfile, DaemonSpec};

fn resolve(
    patch: DaemonPatch,
) -> docsbase_memory::config::schema::Resolved<docsbase_memory::daemon::settings::DaemonSettings> {
    resolve_component::<DaemonSpec>(&DefaultsContext::standalone(), [(SourceId::Cli, patch)])
        .unwrap()
}

#[test]
fn serve_grace_is_an_optional_cli_patch_and_preserves_explicit_zero() {
    use docsbase_memory::cli::{Cli, Command};
    assert!(matches!(
        Cli::try_parse_from(["docsbase", "serve"]).unwrap().command,
        Command::Serve { grace_ms: None, .. }
    ));
    assert!(matches!(
        Cli::try_parse_from(["docsbase", "serve", "--grace-ms", "0"])
            .unwrap()
            .command,
        Command::Serve {
            grace_ms: Some(0),
            ..
        }
    ));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[daemon]\nidle_grace_ms=37").unwrap();
    let snapshot = LaunchSnapshot::capture_from(dir.path());
    for args in [
        vec!["docsbase", "serve"],
        vec!["docsbase", "serve", "--grace-ms", "0"],
    ] {
        let Command::Serve { grace_ms, .. } = Cli::try_parse_from(args).unwrap().command else {
            unreachable!()
        };
        let resolved = snapshot
            .resolve(DaemonPatch {
                idle_grace_ms: grace_ms,
                ..DaemonPatch::default()
            })
            .unwrap();
        assert_eq!(
            resolved.settings().daemon.idle_grace_ms,
            grace_ms.unwrap_or(37)
        );
        assert_eq!(
            resolved
                .provenance()
                .get("daemon.idle_grace_ms")
                .unwrap()
                .source,
            if grace_ms.is_some() {
                SourceId::Cli
            } else {
                SourceId::Global(path.clone())
            }
        );
    }
}

#[test]
fn owner_override_limits_the_real_session_registry() {
    let settings = resolve(DaemonPatch {
        max_sessions: Some(1),
        ..DaemonPatch::default()
    });
    let sessions = SessionRegistry::with_settings(settings.settings()).unwrap();
    let (first, _cancel) = sessions
        .join(std::process::id(), PathBuf::from("/first"), None)
        .unwrap();
    assert!(
        sessions
            .join(std::process::id(), PathBuf::from("/second"), None)
            .unwrap_err()
            .to_string()
            .contains("session limit")
    );
    sessions.leave(first);
    assert!(
        sessions
            .join(std::process::id(), PathBuf::from("/third"), None)
            .is_ok()
    );
}

#[test]
fn global_daemon_fields_have_owner_units_scope_and_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[daemon]\nprofile={mode='shared'}\nidle_grace_ms=0\nmax_connections=3\nsearch_pending=0\n",
    )
    .unwrap();
    let snapshot = docsbase_memory::config::resolve::global_from(dir.path()).unwrap();
    assert_eq!(snapshot.settings().daemon.idle_grace_ms, 0);
    assert_eq!(snapshot.settings().daemon.max_connections, 3);
    assert_eq!(snapshot.settings().daemon.search_pending, 0);
    for key in [
        "daemon.profile",
        "daemon.idle_grace_ms",
        "daemon.max_connections",
    ] {
        let origin = snapshot.provenance().get(key).unwrap();
        assert_eq!(origin.source, SourceId::Global(path.clone()));
        assert_eq!(origin.scope, Scope::Daemon);
    }
    assert_eq!(
        snapshot
            .provenance()
            .get("daemon.idle_grace_ms")
            .unwrap()
            .unit,
        Unit::Milliseconds
    );
    let view = docsbase_memory::config::view::from_resolved(&snapshot).unwrap();
    assert_eq!(
        view.fields
            .iter()
            .find(|f| f.key == "daemon.max_connections")
            .unwrap()
            .value,
        3
    );
    std::fs::write(&path, "").unwrap();
    let omitted = docsbase_memory::config::resolve::global_from(dir.path()).unwrap();
    assert_eq!(omitted.settings().daemon.profile, DaemonProfile::Shared);
    assert_eq!(
        omitted.provenance().get("daemon.profile").unwrap().source,
        SourceId::Defaults
    );
    assert_eq!(omitted.settings().daemon.idle_grace_ms, 5000);
}

#[test]
fn profile_overlay_replaces_the_complete_selector() {
    let ctx = DefaultsContext::standalone();
    let root = tempfile::tempdir().unwrap();
    let a = DaemonProfile::Website {
        project_root: root.path().join("a"),
    };
    let b = DaemonProfile::Website {
        project_root: root.path().join("b"),
    };
    for initial in [None, Some(DaemonProfile::Shared), Some(a)] {
        for replacement in [None, Some(DaemonProfile::Shared), Some(b.clone())] {
            let mut resolution = Resolution::<DaemonSpec>::new(&ctx);
            resolution
                .apply(
                    &SourceId::Global("global".into()),
                    DaemonPatch {
                        profile: initial.clone(),
                        ..DaemonPatch::default()
                    },
                )
                .unwrap();
            resolution
                .apply(
                    &SourceId::Cli,
                    DaemonPatch {
                        profile: replacement.clone(),
                        ..DaemonPatch::default()
                    },
                )
                .unwrap();
            let result = resolution.finish().unwrap();
            assert_eq!(
                result.settings().profile,
                replacement
                    .clone()
                    .or(initial.clone())
                    .unwrap_or(DaemonProfile::Shared)
            );
            assert_eq!(
                result.provenance().get("daemon.profile").unwrap().source,
                if replacement.is_some() {
                    SourceId::Cli
                } else if initial.is_some() {
                    SourceId::Global("global".into())
                } else {
                    SourceId::Defaults
                }
            );
        }
    }
}

#[test]
fn malformed_explicit_profiles_are_rejected_before_overlay() {
    for profile in [
        "{mode='unknown'}",
        "{mode='shared',project_root='/a'}",
        "{mode='website'}",
        "{mode='website',project_root='relative'}",
        "{mode='shared',unknown=1}",
    ] {
        assert!(
            SourceLayer::parse(
                SourceId::Global("g".into()),
                &format!("[daemon]\nprofile={profile}\n")
            )
            .and_then(|layer| docsbase_memory::config::resolve::preview_sources(&layer, None))
            .is_err(),
            "{profile}"
        );
    }
    assert!(
        resolve_component::<DaemonSpec>(
            &DefaultsContext::standalone(),
            [(SourceId::Project("p".into()), DaemonPatch::default())]
        )
        .is_err()
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".docsbase.toml"),
        "[daemon]\nmax_sessions=1",
    )
    .unwrap();
    assert!(Config::for_project(&Config::default(), dir.path()).is_err());
}

#[test]
fn website_rejects_explicit_idle_grace_even_when_zero() {
    let dir = tempfile::tempdir().unwrap();
    for grace in [0, 5000] {
        std::fs::write(
            dir.path().join("config.toml"),
            format!("{}\nidle_grace_ms={grace}", website_source(dir.path())),
        )
        .unwrap();
        assert!(docsbase_memory::config::resolve::global_from(dir.path()).is_err());
    }
}

#[test]
fn imported_website_policy_cannot_hide_a_changed_shared_grace() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = resolve(DaemonPatch::default()).into_settings();
    settings.profile = DaemonProfile::Website {
        project_root: root.path().to_owned(),
    };
    settings.idle_grace_ms = 0;
    assert!(
        Resolution::<DaemonSpec>::from_provided(&DefaultsContext::standalone(), &settings)
            .finish()
            .is_err()
    );
}

#[test]
fn owner_rejects_actual_consumer_overflow_and_closed_inventory_shortfall() {
    let oversized = tokio::sync::Semaphore::MAX_PERMITS + 1;
    let root = tempfile::tempdir().unwrap();
    for patch in [
        DaemonPatch {
            max_connections: Some(oversized),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            max_events: Some(oversized),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            search_active: Some(oversized),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            search_pending: Some(oversized),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            max_sessions: Some(usize::MAX),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            poll_interval_ms: Some(0),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            max_blocking_threads: Some(0),
            ..DaemonPatch::default()
        },
        DaemonPatch {
            profile: Some(DaemonProfile::Website {
                project_root: root.path().to_owned(),
            }),
            max_blocking_threads: Some(7),
            ..DaemonPatch::default()
        },
    ] {
        assert!(
            resolve_component::<DaemonSpec>(
                &DefaultsContext::standalone(),
                [(SourceId::Cli, patch)]
            )
            .is_err()
        );
    }
    let enough = resolve(DaemonPatch {
        profile: Some(DaemonProfile::Website {
            project_root: root.path().to_owned(),
        }),
        max_blocking_threads: Some(8),
        ..DaemonPatch::default()
    });
    assert_eq!(enough.settings().max_blocking_threads, 8);
}

#[test]
fn duration_validation_uses_the_actual_platform_instant_bound() {
    let valid = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(u64::MAX))
        .is_some();
    let result = resolve_component::<DaemonSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            DaemonPatch {
                control_timeout_ms: Some(u64::MAX),
                ..DaemonPatch::default()
            },
        )],
    );
    assert_eq!(result.is_ok(), valid);
}

#[test]
fn daemon_assessment_only_changes_runtime_and_requires_an_executor() {
    let old = resolve(DaemonPatch::default());
    let next = resolve(DaemonPatch {
        max_sessions: Some(1),
        ..DaemonPatch::default()
    });
    let result = DaemonSpec::assess_change(Some(old.settings()), next.settings()).unwrap();
    assert_eq!(result.actions.len(), 1);
    assert_eq!(
        result.actions[0].resource,
        docsbase_memory::config::change::ResourceId::Runtime("daemon".into())
    );
    assert_eq!(result.actions[0].required_grants, []);
    assert!(
        result
            .blockers
            .iter()
            .any(|b| b.code == docsbase_memory::config::change::BlockerCode::ExecutorUnavailable)
    );
}

#[test]
fn identical_effective_shared_policy_has_equal_identity_despite_distinct_origins() {
    use docsbase_memory::config::schema::{ArtifactFacts, ComponentImpact, SettingsSpec};
    let omitted = resolve(DaemonPatch::default());
    let explicit = resolve(DaemonPatch {
        idle_grace_ms: Some(5000),
        ..DaemonPatch::default()
    });
    assert_eq!(
        omitted
            .provenance()
            .get("daemon.idle_grace_ms")
            .unwrap()
            .source,
        SourceId::Defaults
    );
    assert_eq!(
        explicit
            .provenance()
            .get("daemon.idle_grace_ms")
            .unwrap()
            .source,
        SourceId::Cli
    );
    let omitted_initialization = DaemonSpec::assess_change(None, omitted.settings()).unwrap();
    let explicit_initialization = DaemonSpec::assess_change(None, explicit.settings()).unwrap();
    assert_eq!(
        omitted_initialization.actions[0].target, explicit_initialization.actions[0].target,
        "source applicability markers must not change effective runtime identity"
    );
    assert_eq!(
        DaemonSpec::impact(
            Some(omitted.settings()),
            explicit.settings(),
            &ArtifactFacts::default()
        ),
        ComponentImpact::Unchanged
    );
    let change = DaemonSpec::assess_change(Some(omitted.settings()), explicit.settings()).unwrap();
    assert_eq!(change.actions, []);
    assert_eq!(change.blockers, []);
}

fn open(snapshot: &LaunchSnapshot) -> (tempfile::TempDir, std::sync::Arc<Shared>) {
    let cache = tempfile::tempdir().unwrap();
    let shared = Shared::open_with_launch(
        cache.path(),
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        snapshot,
        DaemonPatch::default(),
    )
    .unwrap();
    (cache, shared)
}

#[test]
fn launch_settings_and_restart_baseline_use_the_same_captured_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[daemon]\nmax_sessions=1\n").unwrap();
    let captured = LaunchSnapshot::capture_from(dir.path());
    std::fs::write(&path, "invalid=[").unwrap();
    let (_cache, shared) = open(&captured);
    assert_eq!(shared.daemon_settings().max_sessions, 1);
    let (_id, _cancel) = shared
        .sessions
        .join(std::process::id(), dir.path().to_owned(), None)
        .unwrap();
    assert!(
        shared
            .sessions
            .join(std::process::id(), dir.path().to_owned(), None)
            .is_err(),
        "captured override must reach the real Shared session consumer"
    );
    assert!(shared.restart_notice().unwrap().contains("changed"));
    std::fs::write(path, "[daemon]\nmax_sessions=1\n").unwrap();
    assert!(shared.restart_notice().is_none());
}

#[test]
fn strict_launch_rejects_malformed_policy_before_cache_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[daemon]\nmax_events=0").unwrap();
    let cache = dir.path().join("cache");
    let result = Shared::open_with_launch(
        &cache,
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &LaunchSnapshot::capture_from(dir.path()),
        DaemonPatch::default(),
    );
    assert!(result.is_err());
    assert!(!cache.exists());
    let result = docsbase_memory::daemon::lifecycle::run_daemon_with_launch(
        &cache,
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &LaunchSnapshot::capture_from(dir.path()),
        DaemonPatch::default(),
    );
    assert!(result.is_err());
    assert!(
        !cache.exists(),
        "strict lifecycle must validate before its admission lock"
    );
}

#[test]
fn captured_tokio_worker_sum_overflow_is_rejected_before_cache_resources() {
    use docsbase_memory::config::bootstrap::{BootstrapContext, EntrypointFacts};
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let bootstrap =
        BootstrapContext::capture_with(usize::MAX, EntrypointFacts::standalone(), |_| None)
            .unwrap();
    let result = docsbase_memory::daemon::lifecycle::run_daemon_with_launch_and_bootstrap(
        &cache,
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &LaunchSnapshot::capture_from(dir.path()),
        DaemonPatch::default(),
        &bootstrap,
    );
    assert!(matches!(
        result,
        Err(docsbase_memory::error::Error::Admission { .. })
    ));
    assert!(result.unwrap_err().to_string().contains("overflow"));
    assert!(
        !cache.exists(),
        "no lease, database, job or listener resources may be created"
    );
}

#[test]
fn restart_diagnostics_distinguish_missing_loaded_and_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (_cache, shared) = open(&LaunchSnapshot::capture_from(dir.path()));
    assert!(shared.restart_notice().is_none());
    std::fs::write(&path, vec![b' '; 1_048_577]).unwrap();
    assert!(shared.restart_notice().unwrap().contains("unavailable"));
    assert!(
        shared.restart_notice().is_some(),
        "repeated unavailability remains visible"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(shared.restart_notice().unwrap().contains("unavailable"));
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, "").unwrap();
    assert!(shared.restart_notice().is_some());
    let (_cache2, loaded) = open(&LaunchSnapshot::capture_from(dir.path()));
    assert!(loaded.restart_notice().is_none());
    std::fs::remove_file(path).unwrap();
    assert!(loaded.restart_notice().is_some());
}

#[test]
fn website_data_resolves_but_startup_is_refused_until_backend_gates_exist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, website_source(dir.path())).unwrap();
    let snapshot = LaunchSnapshot::capture_from(dir.path());
    assert!(snapshot.resolve(DaemonPatch::default()).is_ok());
    let cache = dir.path().join("strict-cache");
    let result = Shared::open_with_launch(
        &cache,
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &snapshot,
        DaemonPatch::default(),
    );
    assert!(
        matches!(result, Err(docsbase_memory::error::Error::Admission { .. })),
        "S1a must not start an ungated Website service"
    );
    assert!(!cache.exists());
}

#[test]
fn legacy_cli_cannot_fall_back_to_shared_for_a_recognized_website() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), website_source(dir.path())).unwrap();
    let cache = dir.path().join("legacy-cache");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args(["serve", "--grace-ms", "0"])
        .env("DOCSBASE_CONFIG_DIR", dir.path())
        .env("DOCSBASE_CACHE_DIR", &cache)
        .env("DOCSBASE_DATA_DIR", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a recognized Website selector must never enter malformed-global fallback"
    );
    assert!(!cache.exists());
    assert!(!dir.path().join("data").exists());
}

#[test]
fn invalid_file_daemon_policy_cannot_fall_back_during_legacy_cli_startup() {
    let mut violations = Vec::new();
    for (profile, policy) in [
        ("website", "idle_grace_ms=0"),
        ("website", "max_events=0"),
        ("website", "max_file_size=0"),
        ("shared", "max_events=0"),
        ("shared", "max_events='invalid'"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let profile = if profile == "website" {
            website_source(dir.path())
        } else {
            "[daemon]\nprofile={mode='shared'}".to_owned()
        };
        let text = if policy.starts_with("max_file_size") {
            format!("{policy}\n{profile}")
        } else {
            format!("{profile}\n{policy}")
        };
        std::fs::write(dir.path().join("config.toml"), text).unwrap();
        let cache = dir.path().join("cache");
        let data = dir.path().join("data");
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_docsbase"))
            .args(["serve", "--grace-ms", "0"])
            .env("DOCSBASE_CONFIG_DIR", dir.path())
            .env("DOCSBASE_CACHE_DIR", &cache)
            .env("DOCSBASE_DATA_DIR", &data)
            .output()
            .unwrap();
        if output.status.success() || cache.exists() || data.exists() {
            violations.push(format!(
                "{policy}: status={}, cache={}, data={}",
                output.status,
                cache.exists(),
                data.exists()
            ));
        }
    }
    assert_eq!(violations, Vec::<String>::new());
}

fn website_source(root: &std::path::Path) -> String {
    format!(
        "[daemon]\nprofile={{mode='website',project_root={}}}",
        toml::Value::String(root.to_string_lossy().into_owned())
    )
}
