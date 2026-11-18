//! Inject ticks/signals and drive the real accept loop without timer assertions.

use std::future::{Future, poll_fn};
use std::task::Poll;

use super::*;
use crate::daemon::server::Shared;
use crate::ipc::async_client::AsyncClient;
use crate::ipc::protocol::Response;

fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<Shared>,
    Listener,
    crate::test_transport::TestTransport,
) {
    fixture_with_config("")
}

fn fixture_with_config(
    config_text: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<Shared>,
    Listener,
    crate::test_transport::TestTransport,
) {
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("config.toml"), config_text).unwrap();
    let shared = Shared::open_with_launch(
        cache.path(),
        &crate::store::settings::standalone().unwrap(),
        &crate::store::settings::standalone_read_pool().unwrap(),
        crate::hybrid::dense_settings::standalone()
            .unwrap()
            .settings(),
        &LaunchSnapshot::capture_from(config.path()),
        DaemonPatch {
            control_active: Some(1),
            control_pending: Some(1),
            idle_grace_ms: Some(60_000),
            ..DaemonPatch::default()
        },
    )
    .unwrap();
    create_state_dir(cache.path()).unwrap();
    let transport = crate::test_transport::TestTransport::new(16);
    let listener = platform::bind(&transport, &platform::daemon_endpoint(cache.path())).unwrap();
    (cache, config, shared, listener, transport)
}

async fn poll_pending(future: std::pin::Pin<&mut impl std::future::Future>) {
    let mut future = future;
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test]
async fn queued_janitor_coalesces_ticks_without_blocking_accept_or_stop() {
    let (cache, _config, shared, listener, transport) = fixture();
    let _held = shared.workers.control().await.unwrap();
    let (ticks, mut tick_rx) = mpsc::channel::<()>(4);
    let (_signal, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let mut serving = Box::pin(accept_loop_with(
        Arc::clone(&shared),
        &listener,
        async {
            let _ = signal_rx.await;
        },
        async || {
            tick_rx.recv().await.unwrap();
        },
    ));
    ticks.send(()).await.unwrap();
    poll_pending(serving.as_mut()).await;
    for _ in 0..2 {
        ticks.send(()).await.unwrap();
        poll_pending(serving.as_mut()).await;
    }
    let competing = shared.workers.control();
    tokio::pin!(competing);
    let refused = poll_fn(|cx| Poll::Ready(competing.as_mut().poll(cx))).await;
    assert!(matches!(refused, Poll::Ready(Err(Error::Admission { .. }))));
    let client = async {
        let mut client = AsyncClient::connect(&transport, cache.path())
            .await
            .unwrap();
        client.handshake_registry().await.unwrap();
        client.call(Request::StopDaemon).await.unwrap();
    };
    tokio::pin!(client);
    tokio::select! {
        () = &mut client => serving.await.unwrap(),
        result = &mut serving => { result.unwrap(); client.await; }
    }
}

#[tokio::test]
async fn queued_janitor_does_not_block_signal_shutdown() {
    let (_cache, _config, shared, listener, _transport) = fixture();
    let _held = shared.workers.control().await.unwrap();
    let (ticks, mut tick_rx) = mpsc::channel::<()>(1);
    let (signal, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let mut serving = Box::pin(accept_loop_with(
        Arc::clone(&shared),
        &listener,
        async {
            let _ = signal_rx.await;
        },
        async || {
            tick_rx.recv().await.unwrap();
        },
    ));
    ticks.send(()).await.unwrap();
    poll_pending(serving.as_mut()).await;
    signal.send(()).unwrap();
    serving.await.unwrap();
    assert!(shared.workers.control().await.is_err());
}

#[tokio::test]
async fn running_janitor_coalesces_and_keeps_physical_ownership_after_waiter_drop() {
    let (_cache, _config, shared, _listener, _transport) = fixture();
    let count = Arc::new(AtomicUsize::new(0));
    let (events, mut observed) = mpsc::channel(1);
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let mut janitor = Janitor::default();
    janitor.tick_with(&shared, &count, &events, move |_| {
        entered.send(()).unwrap();
        proceed.recv_timeout(Duration::from_mins(5)).unwrap();
        Vec::new()
    });
    let mut completion = Box::pin(janitor.completed());
    poll_pending(completion.as_mut()).await;
    started.await.unwrap();
    drop(completion);
    janitor.tick_with(&shared, &count, &events, |_| {
        panic!("busy tick must coalesce")
    });
    drop(janitor);
    let charged = count.load(Ordering::SeqCst);
    let mut competing = Box::pin(shared.workers.control());
    poll_pending(competing.as_mut()).await;
    drop(competing);
    release.send(()).unwrap();
    observed.recv().await.unwrap();
    assert_eq!(charged, 1);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(shared.workers.control().await.is_ok());
}

#[tokio::test]
async fn graceful_stop_preserves_accepted_phase_admission_and_refuses_new_ingress() {
    let (cache, _config, shared, listener, transport) = fixture();
    let accepted = shared.activity.track();
    let operation = crate::search::operation::SearchOperation::with_timeout(
        std::time::Instant::now(),
        Duration::from_secs(60),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    // First physical phase finished; its accepted request is between phases.
    drop(shared.workers.background(&operation).await.unwrap());
    let owner = shared.clone();
    let (_signal, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        accept_loop_with(
            owner,
            &listener,
            async {
                let _ = signal_rx.await;
            },
            async || std::future::pending().await,
        )
        .await
    });
    let mut client = AsyncClient::connect(&transport, cache.path())
        .await
        .unwrap();
    client.handshake_registry().await.unwrap();
    let stopping = tokio::spawn(async move { client.call(Request::StopDaemon).await });
    loop {
        let mut late = AsyncClient::connect(&transport, cache.path())
            .await
            .unwrap();
        if let Err(error) = late.handshake_registry().await {
            assert!(
                error.to_string().contains("daemon is shutting down"),
                "{error}"
            );
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !stopping.is_finished(),
        "accepted physical Activity still owns the drain"
    );
    drop(shared.workers.background(&operation).await.unwrap());
    drop(accepted);
    stopping.await.unwrap().unwrap();
    serving.await.unwrap().unwrap();
    assert!(
        shared.workers.background(&operation).await.is_err(),
        "ordinary admission closes after physical drain"
    );
}

// A watchdog catches a broken fixture; it never decides admission or grace.
const REGISTRATION_WATCHDOG: Duration = Duration::from_mins(5);

async fn grace_state(
    state: &mut watch::Receiver<Option<tokio::time::Instant>>,
    armed: bool,
) -> std::result::Result<Option<tokio::time::Instant>, watch::error::RecvError> {
    loop {
        let deadline = *state.borrow_and_update();
        if deadline.is_some() == armed {
            return Ok(deadline);
        }
        state.changed().await?;
    }
}

struct GraceControl {
    now: Arc<std::sync::Mutex<tokio::time::Instant>>,
    trigger: mpsc::Sender<()>,
    state: watch::Receiver<Option<tokio::time::Instant>>,
}

fn controlled_accept(
    owner: Arc<Shared>,
    listener: Listener,
) -> (tokio::task::JoinHandle<Result<()>>, GraceControl) {
    let (observed, state) = watch::channel(None);
    let (trigger, receiver) = mpsc::channel::<()>(1);
    let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
    let clock = Arc::new(std::sync::Mutex::new(tokio::time::Instant::now()));
    let now = clock.clone();
    let serving = tokio::spawn(async move {
        accept_loop_events(
            owner,
            &listener,
            std::future::pending(),
            async || std::future::pending().await,
            || *now.lock().unwrap(),
            |deadline| {
                let observed = observed.clone();
                let receiver = receiver.clone();
                async move {
                    observed.send_replace(deadline);
                    if deadline.is_some() {
                        receiver.lock().await.recv().await.unwrap();
                    } else {
                        std::future::pending::<()>().await;
                    }
                }
            },
        )
        .await
    });
    (
        serving,
        GraceControl {
            now: clock,
            trigger,
            state,
        },
    )
}

#[test]
fn registration_survives_armed_grace() {
    for rooted in [false, true] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let policy = crate::daemon::settings::standalone().unwrap();
        let mut root = crate::daemon::job_service::JobRoot::new(policy.settings());
        let service = root.service();
        root.run(&runtime, armed_registration(rooted, service))
            .unwrap();
    }
}

async fn armed_registration(
    rooted: bool,
    service: crate::daemon::job_service::JobService,
) -> Result<()> {
    let (cache, _config, shared, listener, transport) = fixture_with_config("auto_index = true\n");
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".git")).unwrap();
    std::fs::write(
        project.path().join("guide.md"),
        "# Guide\n\nwidget registration\n",
    )
    .unwrap();
    if rooted {
        shared.activate_jobs(service).unwrap();
    }
    let (entered, work_started) = tokio::sync::oneshot::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    *shared.registration_work_gate.lock().unwrap() = Some(Box::new(move || {
        let _ = entered.send(());
        // Disconnect also releases the real worker if an earlier assertion fails.
        let _ = proceed.recv_timeout(REGISTRATION_WATCHDOG);
    }));
    let (mut serving, mut clock) = controlled_accept(shared.clone(), listener);
    let mut client = AsyncClient::connect(&transport, cache.path())
        .await
        .unwrap();
    client.handshake_registry().await.unwrap();
    let deadline = tokio::time::timeout(REGISTRATION_WATCHDOG, grace_state(&mut clock.state, true))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let cwd = project.path().to_path_buf();
    let registering = tokio::spawn(async move {
        let reply = client
            .call(Request::RegisterSession {
                pid: std::process::id(),
                cwd,
            })
            .await;
        (client, reply)
    });
    tokio::time::timeout(REGISTRATION_WATCHDOG, work_started)
        .await
        .unwrap()
        .unwrap();
    let active = shared.activity.counter.load(Ordering::SeqCst);
    let unbound = shared.sessions.count();
    let original_deadline = *clock.state.borrow_and_update();
    *clock.now.lock().unwrap() = deadline;
    clock.trigger.send(()).await.unwrap();
    // Observing None proves the actual expired-deadline branch ran and rechecked
    // physical Activity while the admitted registration worker was still gated.
    let survived =
        tokio::time::timeout(REGISTRATION_WATCHDOG, grace_state(&mut clock.state, false)).await;
    let _ = release.send(());
    let (client, reply) = tokio::time::timeout(REGISTRATION_WATCHDOG, registering)
        .await
        .unwrap()
        .unwrap();
    let bound = shared.sessions.count();
    drop(client);
    let rearmed =
        tokio::time::timeout(REGISTRATION_WATCHDOG, grace_state(&mut clock.state, true)).await;
    if let Ok(Ok(Some(deadline))) = rearmed {
        *clock.now.lock().unwrap() = deadline;
        let _ = clock.trigger.send(()).await;
    }
    let stopped = tokio::time::timeout(REGISTRATION_WATCHDOG, &mut serving).await;
    if stopped.is_err() {
        serving.abort();
        let _ = serving.await;
    }
    // Assertions follow release and cleanup, including the intentionally broken RED.
    assert!(
        active > 0 && unbound == 0,
        "real admitted work must precede session binding"
    );
    assert_eq!(
        original_deadline,
        Some(deadline),
        "the original grace must still be armed"
    );
    assert!(
        matches!(survived, Ok(Ok(None))),
        "expired grace cut off admitted registration: {survived:?}"
    );
    assert!(
        matches!(reply, Ok(Response::ToolResult { ref value }) if value["status"] == "indexed"),
        "{reply:?}"
    );
    assert_eq!(bound, 1, "successful registration must bind a real session");
    assert!(
        matches!(rearmed, Ok(Ok(Some(_)))),
        "disconnect must rearm real idle exit: {rearmed:?}"
    );
    assert!(
        matches!(stopped, Ok(Ok(Ok(())))),
        "controlled idle expiry must exit cleanly: {stopped:?}"
    );
    Ok(())
}
