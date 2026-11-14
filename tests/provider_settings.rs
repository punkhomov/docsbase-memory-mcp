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
