//! Real dispatch must refuse before spawning when its application lane is full.

use super::*;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Arc<Shared>) {
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let shared = Shared::open_with_launch(
        cache.path(),
        &crate::store::settings::standalone().unwrap(),
        &crate::store::settings::standalone_read_pool().unwrap(),
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &LaunchSnapshot::capture_from(config.path()),
        DaemonPatch {
            search_active: Some(1),
            search_pending: Some(0),
            control_active: Some(1),
            control_pending: Some(0),
            ..DaemonPatch::default()
        },
    )
    .unwrap();
    (cache, config, shared)
}

fn pending_refusal(error: &Error) {
    assert!(matches!(error, Error::Admission { .. }), "{error}");
    assert!(error.to_string().contains("pending limit"), "{error}");
}

#[tokio::test]
async fn metadata_dispatch_obeys_control_lane_before_route() {
    let (_cache, _config, shared) = fixture();
    let _held = shared.workers.control().await.unwrap();
    pending_refusal(
        &call_tool(&shared, None, "list_projects", json!({}))
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn search_dispatch_obeys_search_lane_before_route() {
    let (_cache, _config, shared) = fixture();
    let operation = search_operation(&shared, "search_docs").unwrap().unwrap();
    let _held = shared.workers.search(&operation).await.unwrap();
    pending_refusal(
        &call_tool(&shared, None, "search_docs", json!({}))
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn registration_obeys_control_lane_before_database_work() {
    let (_cache, config, shared) = fixture();
    let _held = shared.workers.control().await.unwrap();
    pending_refusal(
        &register_session(&shared, std::process::id(), config.path(), false)
            .await
            .unwrap_err(),
    );
    assert_eq!(shared.sessions.count(), 0);
}

#[tokio::test]
async fn readonly_bind_obeys_control_lane_before_session_effects() {
    let (_cache, _config, shared) = fixture();
    let _held = shared.workers.control().await.unwrap();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (events, _) = mpsc::channel(1);
    let (shutdown, _) = watch::channel(false);
    let scope = scoped::Scope {
        activity: Activity::new(&in_flight, &events, None),
        operation: None,
    };
    let mut peer = Peer {
        hello_done: true,
        ..Peer::default()
    };
    let mut reader = FrameReader::new(BufReader::new(&b""[..]));
    let (response, _) = handle_request(
        &shared,
        &shutdown,
        &in_flight,
        &mut peer,
        Request::BindReadOnly {
            pid: std::process::id(),
            project_id: None,
        },
        &scope,
        &mut reader,
    )
    .await;
    assert!(
        matches!(response, Response::Error { code: -32010, .. }),
        "{response:?}"
    );
    assert_eq!(shared.sessions.count(), 0);
}
