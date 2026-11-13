//! Inject ticks/signals and drive the real accept loop without timer assertions.

use std::future::{Future, poll_fn};
use std::task::Poll;

use super::*;
use crate::daemon::server::Shared;
use crate::ipc::async_client::AsyncClient;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Arc<Shared>, Listener) {
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
            control_active: Some(1),
            control_pending: Some(1),
            idle_grace_ms: Some(60_000),
            ..DaemonPatch::default()
        },
    )
    .unwrap();
    let listener = platform::bind(&platform::daemon_endpoint(cache.path())).unwrap();
    (cache, config, shared, listener)
}

async fn poll_pending(future: std::pin::Pin<&mut impl std::future::Future>) {
    let mut future = future;
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test]
async fn queued_janitor_coalesces_ticks_without_blocking_accept_or_stop() {
    let (cache, _config, shared, listener) = fixture();
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
        let mut client = AsyncClient::connect(cache.path()).await.unwrap();
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
    let (_cache, _config, shared, listener) = fixture();
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
