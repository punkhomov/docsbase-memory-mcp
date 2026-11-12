//! Pure change-plan contracts exercised through actual component owners.
use docsbase_memory::config::change::{
    Action, Assessment, BlockerCode, Effect, ExpectedState, Grant, Observation, PlanLimits, Reason,
    ResourceId, Target, compile,
};
use docsbase_memory::config::migrate::{VectorBudgetDecision, draft_migration};
use docsbase_memory::config::schema::{ArtifactFacts, SourceId};
use docsbase_memory::config::source::SourceLayer;
use docsbase_memory::search::settings::SearchSpec;
use docsbase_memory::store::{Db, settings::DbSpec};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn limits() -> PlanLimits {
    PlanLimits {
        max_actions: 32,
        max_dependencies: 64,
        max_metadata_bytes: 64 * 1024,
    }
}
fn assessment(name: &str, dependencies: &[&str]) -> Assessment {
    let resource = ResourceId::Runtime(name.into());
    Assessment {
        expected: vec![Observation {
            resource: resource.clone(),
            state: ExpectedState::Missing,
        }],
        actions: vec![Action {
            resource,
            effect: Effect::InitializeRuntime,
            target: Some(Target::Identity(format!("test-v1:{name}"))),
            reasons: vec![Reason::FirstInitialization],
            dependencies: dependencies
                .iter()
                .map(|v| ResourceId::Runtime((*v).into()))
                .collect(),
            required_grants: vec![],
        }],
        blockers: vec![],
    }
}

#[test]
fn runtime_owners_classify_changes_without_artifact_actions() {
    let old = docsbase_memory::search::settings::standalone()
        .unwrap()
        .into_settings();
    let mut next = old.clone();
    next.request_timeout_ms += 1;
    let before = SearchSpec::assess_change(Some(&old), &old).unwrap();
    let unchanged = compile(&[before], limits()).unwrap();
    assert_eq!(
        unchanged.body.actions,
        [] as [docsbase_memory::config::change::Action; 0]
    );
    assert_eq!(unchanged.body.unchanged_observations.len(), 1);
    let changed = compile(
        &[SearchSpec::assess_change(Some(&old), &next).unwrap()],
        limits(),
    )
    .unwrap();
    assert_eq!(changed.body.actions.len(), 1);
    assert_eq!(changed.body.actions[0].effect, Effect::ReconfigureRuntime);
    assert!(matches!(
        changed.body.actions[0].resource,
        ResourceId::Runtime(_)
    ));
    assert!(
        changed
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::ExecutorUnavailable)
    );
    assert_ne!(changed.plan_id, unchanged.plan_id);
    let init = compile(&[SearchSpec::assess_change(None, &next).unwrap()], limits()).unwrap();
    assert_eq!(init.body.actions[0].effect, Effect::InitializeRuntime);
    next.request_timeout_ms = 0;
    assert!(SearchSpec::assess_change(Some(&old), &next).is_err());

    let db = docsbase_memory::store::settings::standalone()
        .unwrap()
        .into_settings();
    let mut db_next = db.clone();
    db_next.read_busy_timeout += Duration::from_nanos(1);
    let plan = compile(
        &[DbSpec::assess_change(Some(&db), &db_next).unwrap()],
        limits(),
    )
    .unwrap();
    assert_eq!(plan.body.actions[0].effect, Effect::ReconfigureRuntime);
    assert!(matches!(
        plan.body.actions[0].resource,
        ResourceId::Runtime(_)
    ));
    assert_ne!(
        plan.body.actions[0].target,
        Some(Target::Identity(match &plan.body.expected[0].state {
            ExpectedState::KnownIdentity(id) => id.clone(),
            _ => panic!("known policy"),
        }))
    );
    db_next.wal_attempts = 0;
    assert!(DbSpec::assess_change(None, &db_next).is_err());
}

#[test]
fn graph_is_canonical_deduplicated_and_topologically_ordered() {
    let a = assessment("a", &["z"]);
    let z = assessment("z", &[]);
    let mut duplicate = a.clone();
    duplicate.actions[0].reasons.push(Reason::SettingsChanged);
    let first = compile(&[a.clone(), z.clone(), duplicate.clone()], limits()).unwrap();
    let second = compile(&[duplicate, z, a], limits()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.body.actions.len(), 2);
    assert_eq!(
        first.body.actions[0].resource,
        ResourceId::Runtime("z".into())
    );
    assert_eq!(first.body.actions[1].reasons.len(), 2);
    assert_eq!(first.body.actions[1].dependencies.len(), 1);
    assert_eq!(first.body.blockers.len(), 2);
    assert_eq!(first.body.plan_version, 1);
    assert!(first.plan_id.starts_with("change-plan-v1:"));
}

#[test]
fn graph_rejects_conflicts_missing_inputs_and_cycles() {
    let a = assessment("a", &[]);
    let mut conflicting = a.clone();
    conflicting.actions[0].target = Some(Target::Identity("different".into()));
    assert!(compile(&[a.clone(), conflicting], limits()).is_err());
    let mut facts = a.clone();
    facts.expected[0].state = ExpectedState::Unknown;
    assert!(compile(&[a.clone(), facts], limits()).is_err());
    let mut no_observation = a.clone();
    no_observation.expected.clear();
    assert!(compile(&[no_observation], limits()).is_err());
    assert!(compile(&[assessment("a", &["missing"])], limits()).is_err());
    assert!(
        compile(
            &[assessment("a", &["b"]), assessment("b", &["a"])],
            limits()
        )
        .is_err()
    );
    for effect in [Effect::Evict, Effect::DeleteStore, Effect::Rebuild] {
        let mut incompatible = a.clone();
        incompatible.actions[0].effect = effect;
        assert!(compile(&[a.clone(), incompatible], limits()).is_err());
    }
    let mut grants = a.clone();
    grants.actions[0].required_grants.push(Grant::Destructive);
    assert!(compile(&[a, grants], limits()).is_err());
}

#[test]
fn limits_bound_raw_inputs_and_final_output_before_publication() {
    let a = assessment("a", &[]);
    let mut cap = limits();
    cap.max_actions = 1;
    assert!(compile(&[a.clone(), a.clone()], cap).is_err());
    cap = limits();
    cap.max_dependencies = 0;
    assert!(compile(&[assessment("a", &["z"]), assessment("z", &[])], cap).is_err());
    cap = limits();
    cap.max_metadata_bytes = 1;
    assert!(compile(std::slice::from_ref(&a), cap).is_err());
    let plan = compile(std::slice::from_ref(&a), limits()).unwrap();
    let size = serde_json::to_vec(&plan).unwrap().len();
    cap.max_metadata_bytes = size;
    assert_eq!(compile(std::slice::from_ref(&a), cap).unwrap(), plan);
    cap.max_metadata_bytes -= 1;
    assert!(compile(&[a], cap).is_err());
}

#[test]
fn migration_dependencies_remain_blocked_and_sources_are_not_claimed_observed() {
    let global =
        SourceLayer::parse(SourceId::Global("config.toml".into()), "hybrid=false\n").unwrap();
    let incomplete = draft_migration(&global, &[PathBuf::from("project.toml")], &[], None).unwrap();
    assert_eq!(
        incomplete.edits,
        [] as [docsbase_memory::config::migrate::SourceEdit; 0]
    );
    let blocked = compile(&[incomplete.assess_change().unwrap()], limits()).unwrap();
    assert_eq!(
        blocked.body.actions,
        [] as [docsbase_memory::config::change::Action; 0]
    );
    assert!(
        blocked
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::UninspectedProject)
    );
    assert!(
        blocked
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::VectorBudgetDecisionRequired)
    );
    let mut draft = draft_migration(
        &global,
        &[],
        &[],
        Some(VectorBudgetDecision::UseBytes(4096)),
    )
    .unwrap();
    let first = compile(&[draft.assess_change().unwrap()], limits()).unwrap();
    assert_eq!(first.body.actions[0].effect, Effect::AdoptSharedStore);
    assert_eq!(
        first.body.actions[0].target,
        Some(Target::VectorBudgetBytes(4096))
    );
    assert_eq!(first.body.actions[1].effect, Effect::RewriteSource);
    assert_eq!(
        first.body.actions[1].dependencies,
        vec![first.body.actions[0].resource.clone()]
    );
    assert!(
        first
            .body
            .expected
            .iter()
            .any(|o| matches!(o.state, ExpectedState::SuppliedSourceRevision(_)))
    );
    assert!(
        first
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::SourceObservationRequired)
    );
    assert!(
        first
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::StrictStoreAdoptionUnavailable)
    );
    draft.runtime_activation_available = true;
    assert_eq!(
        compile(&[draft.assess_change().unwrap()], limits()).unwrap(),
        first
    );
    let valid = draft.clone();
    draft.edits[0].expected_source_hash = "forged".into();
    assert!(draft.assess_change().is_err());
    draft = valid.clone();
    draft.draft_version = 999;
    assert!(draft.assess_change().is_err());
    draft = valid;
    draft.selected_vector_max_bytes = None;
    assert!(draft.assess_change().is_err());
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut files = BTreeMap::new();
    if root.exists() {
        files.insert(root.to_owned(), None);
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                files.insert(path.clone(), Some(std::fs::read(path).unwrap()));
            }
        }
    }
    files
}
fn schema_plan(cache: &Path) -> docsbase_memory::config::change::ChangePlan {
    let policy = docsbase_memory::store::settings::standalone().unwrap();
    let before = snapshot(cache);
    let facts = Db::inspect_readonly(cache, &policy).unwrap();
    let plan = compile(&[DbSpec::assess_schema(&facts).unwrap()], limits()).unwrap();
    assert_eq!(snapshot(cache), before);
    plan
}
#[test]
fn missing_schema_plan_creates_no_directory_and_no_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("missing");
    let plan = schema_plan(&cache);
    assert!(!cache.exists());
    assert_eq!(plan.body.actions[0].effect, Effect::InitializeStore);
    assert_eq!(
        plan.body.required_grants,
        [] as [docsbase_memory::config::change::Grant; 0]
    );
}
#[test]
fn actual_schema_observations_preserve_files_and_never_claim_query_readiness() {
    let dir = tempfile::tempdir().unwrap();
    drop(Db::open(dir.path()).unwrap());
    let ready = schema_plan(dir.path());
    assert_eq!(
        ready.body.actions,
        [] as [docsbase_memory::config::change::Action; 0]
    );
    assert_eq!(ready.body.unchanged_observations.len(), 1);
    let path = dir.path().join(docsbase_memory::store::DB_FILE);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    let old = schema_plan(dir.path());
    assert_eq!(old.body.actions[0].effect, Effect::MigrateSchema);
    assert!(old.body.required_grants.contains(&Grant::Destructive));
    std::fs::write(
        dir.path()
            .join(format!("{}-wal", docsbase_memory::store::DB_FILE)),
        b"pending",
    )
    .unwrap();
    let wal = schema_plan(dir.path());
    assert_eq!(
        wal.body.actions,
        [] as [docsbase_memory::config::change::Action; 0]
    );
    assert!(
        wal.body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::ObservationUnavailable)
    );
    std::fs::remove_file(path).unwrap();
    let recovery = schema_plan(dir.path());
    assert_eq!(recovery.body.actions[0].effect, Effect::RecoverStore);
}
#[test]
fn unknown_or_future_schema_identity_is_never_reused() {
    let empty = DbSpec::assess_schema(&ArtifactFacts::default()).unwrap();
    assert_ne!(
        compile(&[empty], limits()).unwrap().body.blockers,
        [] as [docsbase_memory::config::change::Blocker; 0]
    );
    let dir = tempfile::tempdir().unwrap();
    drop(Db::open(dir.path()).unwrap());
    let settings = docsbase_memory::store::settings::standalone().unwrap();
    let mut facts = Db::inspect_readonly(dir.path(), &settings).unwrap();
    for identity in [
        None,
        Some("sqlite-schema-v2:2:1"),
        Some("sqlite-schema-v1:999:1"),
        Some("sqlite-schema-v1:02:1"),
        Some("sqlite-schema-v1:2:1:extra"),
    ] {
        facts.artifacts[0].identity = identity.map(str::to_owned);
        let plan = compile(&[DbSpec::assess_schema(&facts).unwrap()], limits()).unwrap();
        assert_eq!(
            plan.body.actions,
            [] as [docsbase_memory::config::change::Action; 0]
        );
        assert_eq!(
            plan.body.unchanged_observations,
            [] as [docsbase_memory::config::change::ResourceId; 0]
        );
        assert!(
            plan.body
                .blockers
                .iter()
                .any(|b| b.code == BlockerCode::UnsupportedIdentity)
        );
    }
}

#[test]
fn identity_covers_observations_targets_reasons_and_permissions() {
    use docsbase_memory::config::change::Blocker;
    let original = assessment("a", &[]);
    let before = original.clone();
    let baseline = compile(std::slice::from_ref(&original), limits()).unwrap();
    let mut variants = vec![original.clone(); 5];
    variants[0].expected[0].state = ExpectedState::KnownIdentity("old-policy-v1:x".into());
    variants[1].actions[0].target = Some(Target::Identity("new-policy-v1:y".into()));
    variants[2].actions[0].reasons.push(Reason::SettingsChanged);
    variants[3].actions[0]
        .required_grants
        .push(Grant::Destructive);
    variants[4].blockers.push(Blocker {
        resource: None,
        code: BlockerCode::ObservationUnavailable,
    });
    for variant in variants {
        assert_ne!(
            compile(&[variant], limits()).unwrap().plan_id,
            baseline.plan_id
        );
    }
    let mut cap = limits();
    cap.max_metadata_bytes = 0;
    assert!(compile(std::slice::from_ref(&original), cap).is_err());
    assert_eq!(original, before);
    let empty = compile(&[], limits()).unwrap();
    assert_eq!(
        empty.body.actions,
        [] as [docsbase_memory::config::change::Action; 0]
    );
    assert_eq!(
        empty.body.blockers,
        [] as [docsbase_memory::config::change::Blocker; 0]
    );
    assert!(compile(&[assessment("self", &["self"])], limits()).is_err());
}

#[test]
fn migration_projection_never_emits_source_values_and_bounds_source_hashing() {
    let source = "# PRIVATE_SOURCE_MARKER\nhybrid=false\n";
    let global = SourceLayer::parse(SourceId::Global("config.toml".into()), source).unwrap();
    let mut draft = draft_migration(
        &global,
        &[],
        &[],
        Some(VectorBudgetDecision::UseBytes(4096)),
    )
    .unwrap();
    let plan = compile(&[draft.assess_change().unwrap()], limits()).unwrap();
    let json = serde_json::to_string(&plan).unwrap();
    assert!(!json.contains("PRIVATE_SOURCE_MARKER"));
    assert!(!json.contains("hybrid=false"));
    draft.edits[0].before =
        "x".repeat(usize::try_from(docsbase_memory::limits::MAX_STATE_BYTES).unwrap() + 1);
    draft.edits[0].expected_source_hash = blake3::hash(draft.edits[0].before.as_bytes())
        .to_hex()
        .to_string();
    let error = draft.assess_change().unwrap_err().to_string();
    assert!(!error.contains("PRIVATE_SOURCE_MARKER"));
}

#[test]
fn equivalent_source_spellings_have_identical_plan_encoding() {
    use docsbase_memory::config::change::Blocker;
    fn source_assessment(path: &str) -> Assessment {
        let mut input = assessment("placeholder", &[]);
        let source = ResourceId::Source(SourceId::Global(PathBuf::from(path)));
        input.expected[0].resource = source.clone();
        input.actions[0].resource = source.clone();
        input.blockers.push(Blocker {
            resource: Some(source),
            code: BlockerCode::SourceObservationRequired,
        });
        input
    }
    let aliases = ["cfg/config.toml", "cfg//config.toml", "cfg/./config.toml"];
    let baseline = compile(&[source_assessment(aliases[0])], limits()).unwrap();
    for alias in aliases {
        let actual = compile(&[source_assessment(alias)], limits()).unwrap();
        assert_eq!(actual.plan_id, baseline.plan_id);
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&baseline).unwrap()
        );
        let forward = compile(
            &[source_assessment(alias), source_assessment(aliases[0])],
            limits(),
        )
        .unwrap();
        let reverse = compile(
            &[source_assessment(aliases[0]), source_assessment(alias)],
            limits(),
        )
        .unwrap();
        assert_eq!(forward.plan_id, reverse.plan_id);
        assert_eq!(forward.plan_id, baseline.plan_id);
    }
    // Parent components must survive: collapsing them could cross a symlink.
    assert_ne!(
        compile(&[source_assessment("cfg/../config.toml")], limits())
            .unwrap()
            .plan_id,
        compile(&[source_assessment("config.toml")], limits())
            .unwrap()
            .plan_id
    );
    // Same aliases in dependency references must have the same wire identity too.
    let source = source_assessment(aliases[0]);
    let mut dependent = assessment("dependent", &[]);
    dependent.actions[0]
        .dependencies
        .push(ResourceId::Source(SourceId::Global(aliases[1].into())));
    let first = compile(&[source.clone(), dependent.clone()], limits()).unwrap();
    dependent.actions[0].dependencies[0] = ResourceId::Source(SourceId::Global(aliases[0].into()));
    assert_eq!(
        first.plan_id,
        compile(&[source, dependent], limits()).unwrap().plan_id
    );
}

#[cfg(windows)]
#[test]
fn windows_source_prefix_equality_matches_plan_encoding() {
    fn plan(path: &str) -> docsbase_memory::config::change::ChangePlan {
        let resource = ResourceId::Source(SourceId::Global(path.into()));
        compile(
            &[Assessment {
                expected: vec![Observation {
                    resource,
                    state: ExpectedState::Unknown,
                }],
                ..Assessment::default()
            }],
            limits(),
        )
        .unwrap()
    }
    for (left, right) in [
        (r"C:\config.toml", "c:/config.toml"),
        (r"\\server\share\cfg\file", r"\\server\share\cfg\.\file"),
    ] {
        assert_eq!(Path::new(left), Path::new(right));
        assert_eq!(plan(left).plan_id, plan(right).plan_id);
    }
}

#[test]
fn exact_input_action_and_edge_limits_accept_the_boundary() {
    let inputs = [assessment("a", &["z"]), assessment("z", &[])];
    let mut cap = limits();
    cap.max_actions = 2;
    cap.max_dependencies = 1;
    assert_eq!(compile(&inputs, cap).unwrap().body.actions.len(), 2);
    // Raw duplicate metadata may exceed the deduplicated output size.
    let duplicates = vec![assessment("a", &[]); 10];
    let input_bytes = serde_json::to_vec(&duplicates).unwrap().len();
    let plan = compile(&duplicates, limits()).unwrap();
    assert!(input_bytes > serde_json::to_vec(&plan).unwrap().len());
    cap = limits();
    cap.max_metadata_bytes = input_bytes;
    assert_eq!(compile(&duplicates, cap).unwrap(), plan);
    cap.max_metadata_bytes -= 1;
    assert!(compile(&duplicates, cap).is_err());
}

#[test]
fn canonical_paths_cannot_hide_oversized_raw_input() {
    let path = format!("cfg{}config.toml", "/".repeat(8192));
    let input = Assessment {
        expected: vec![Observation {
            resource: ResourceId::Source(SourceId::Global(path.into())),
            state: ExpectedState::Unknown,
        }],
        ..Assessment::default()
    };
    assert!(serde_json::to_vec(&input).unwrap().len() < 1024);
    let mut cap = limits();
    cap.max_metadata_bytes = 1024;
    assert!(compile(&[input], cap).is_err());
}
