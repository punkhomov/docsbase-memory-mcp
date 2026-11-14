use docsbase_memory::config::{resolve, view};

#[test]
fn resolved_remote_policy_keeps_owner_values_and_provenance() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[remote]\ninteractive_active=1\ninteractive_pending=0\nattempt_timeout_ms=73\nretry_attempts=1\nembed_batch_texts=2\nmax_response_bytes=47\n",
    )
    .expect("write explicit policy");
    let resolved = resolve::global_from(directory.path()).expect("resolve remote owner");
    let effective = view::from_resolved(&resolved).expect("effective owner view");
    let field = effective
        .fields
        .iter()
        .find(|field| field.key == "remote.attempt_timeout_ms")
        .expect("resolved remote field");
    assert_eq!(field.value, 73);
    assert_eq!(
        resolved
            .provenance()
            .get("remote.attempt_timeout_ms")
            .expect("owner provenance")
            .source,
        docsbase_memory::config::schema::SourceId::Global(path)
    );
}

#[test]
fn remote_policy_is_not_a_project_override() {
    let configuration = tempfile::tempdir().expect("global directory");
    let project = tempfile::tempdir().expect("project directory");
    std::fs::write(
        project.path().join(".docsbase.toml"),
        "[remote]\ninteractive_active=1",
    )
    .expect("project source");
    assert!(resolve::load_from(configuration.path(), Some(project.path())).is_err());
}

#[test]
fn remote_v1_source_roundtrip_preserves_effective_policy_and_capabilities() {
    use docsbase_memory::config::schema::SourceId;
    use docsbase_memory::config::source::SourceLayer;
    let text = "[remote]\nattempt_timeout_ms=73\nretry_attempts=1\ninteractive_active=2\n";
    let document: toml::Value = toml::from_str(text).unwrap();
    let roundtrip = toml::to_string(&document).unwrap();
    let source = SourceLayer::parse(SourceId::Global("global.toml".into()), &roundtrip).unwrap();
    let preview = resolve::preview_sources(&source, None).unwrap();
    assert_eq!(preview.effective.settings().remote.attempt_timeout_ms, 73);
    let output = view::from_resolved(&preview.effective).unwrap();
    let field = output
        .fields
        .iter()
        .find(|field| field.key == "remote.attempt_timeout_ms")
        .unwrap();
    assert_eq!(
        field.capability.as_ref().unwrap().compiled,
        cfg!(feature = "hybrid-remote")
    );
    assert_eq!(field.origin.source, SourceId::Global("global.toml".into()));
}

#[test]
fn remote_policy_change_requests_runtime_restart_without_artifact_effects() {
    use docsbase_memory::config::change::{Effect, ResourceId};
    use docsbase_memory::daemon::launch::LaunchSnapshot;
    use docsbase_memory::daemon::server::Shared;
    use docsbase_memory::daemon::settings::DaemonPatch;
    use docsbase_memory::hybrid::remote_settings::{RemoteSpec, standalone};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "[remote]\nretry_attempts=1\n").unwrap();
    let snapshot = LaunchSnapshot::capture_from(directory.path());
    let cache = tempfile::tempdir().unwrap();
    let shared = Shared::open_with_launch(
        cache.path(),
        &docsbase_memory::store::settings::standalone().unwrap(),
        &docsbase_memory::store::settings::standalone_read_pool().unwrap(),
        docsbase_memory::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &snapshot,
        DaemonPatch::default(),
    )
    .unwrap();
    assert!(shared.restart_notice().is_none());
    let old = resolve::global_from(directory.path()).unwrap();
    std::fs::write(&path, "[remote]\nretry_attempts=2\n").unwrap();
    let next = resolve::global_from(directory.path()).unwrap();
    assert!(shared.restart_notice().unwrap().contains("changed"));
    let change =
        RemoteSpec::assess_change(Some(&old.settings().remote), &next.settings().remote).unwrap();
    assert_eq!(change.actions.len(), 1);
    let action = &change.actions[0];
    assert_eq!(action.resource, ResourceId::Runtime("remote".into()));
    assert_eq!(action.effect, Effect::ReconfigureRuntime);
    assert_eq!(action.dependencies, []);
    assert_eq!(action.required_grants, []);
    assert_eq!(change.blockers, []);
    assert_eq!(
        RemoteSpec::assess_change(Some(&next.settings().remote), &next.settings().remote)
            .unwrap()
            .actions,
        []
    );
    assert_eq!(
        RemoteSpec::assess_change(None, standalone().unwrap().settings())
            .unwrap()
            .actions[0]
            .effect,
        Effect::InitializeRuntime
    );
}
