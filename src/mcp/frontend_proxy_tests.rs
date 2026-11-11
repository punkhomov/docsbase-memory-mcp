//! Native frontend requests, using local fake peers and explicit ordering gates.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::error::Error;
use crate::ipc::framing::{Frame, FrameReader};
use crate::ipc::protocol::{self, Request, Response, SEARCH_CONTEXT_CAPABILITY};
use crate::platform;

use super::Frontend;

const WATCHDOG: Duration = Duration::from_secs(300);

#[derive(Clone)]
enum Mode {
    Reply,
    HoldFirst,
    CloseFirst,
    CloseAlways,
    CancelFirst(crate::hybrid::work::Cancellation),
    IndexThenRejectRebind(RebindRejection),
}

#[derive(Clone, Copy)]
enum RebindRejection {
    Admission,
    Protocol,
    Transport,
}

struct State {
    capable: bool,
    mode: Mode,
    sent: mpsc::UnboundedSender<Request>,
    closed: mpsc::UnboundedSender<()>,
    calls: std::sync::atomic::AtomicUsize,
    release: tokio::sync::Notify,
}

struct Fake {
    frontend: Frontend,
    received: mpsc::UnboundedReceiver<Request>,
    closed: mpsc::UnboundedReceiver<()>,
    state: std::sync::Arc<State>,
    listener: tokio::task::JoinHandle<()>,
    cache: tempfile::TempDir,
}

fn policy() -> crate::search::settings::SearchSettings {
    let mut policy = crate::search::settings::standalone()
        .expect("owner policy")
        .into_settings();
    policy.request_timeout_ms = u64::try_from((WATCHDOG * 2).as_millis()).expect("safety policy");
    policy
}

impl Fake {
    fn new(capable: bool) -> Self {
        Self::with_mode(capable, Mode::Reply)
    }

    fn with_mode(capable: bool, mode: Mode) -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let endpoint = platform::daemon_endpoint(cache.path());
        std::fs::create_dir_all(cache.path().join("state")).expect("state directory");
        if let platform::Endpoint::Unix(path) = &endpoint {
            platform::fs::secure_dir(path.parent().expect("socket directory"))
                .expect("runtime directory");
        }
        let listener = platform::bind(&endpoint).expect("native listener");
        write_state(cache.path(), &endpoint);
        let (sent, received) = mpsc::unbounded_channel();
        let (closed_sender, closed) = mpsc::unbounded_channel();
        let state = std::sync::Arc::new(State {
            capable,
            mode,
            sent,
            closed: closed_sender,
            calls: std::sync::atomic::AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
        });
        let state_for_peers = std::sync::Arc::clone(&state);
        let listener = tokio::spawn(async move {
            let mut peers = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok(stream) = accepted else { return; };
                        peers.spawn(serve(stream, std::sync::Arc::clone(&state_for_peers)));
                    }
                    Some(result) = peers.join_next(), if !peers.is_empty() => result.expect("native fake peer"),
                }
            }
        });
        Self {
            frontend: Frontend::with_search_policy(cache.path().to_path_buf(), policy()),
            received,
            closed,
            state,
            listener,
            cache,
        }
    }

    async fn search_received(&mut self) -> crate::ipc::protocol::SearchCallContext {
        loop {
            if let Request::Search { context, .. } =
                self.received.recv().await.expect("search event")
            {
                return context;
            }
        }
    }

    fn calls(&self) -> usize {
        self.state.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

fn write_state(cache: &Path, endpoint: &platform::Endpoint) {
    std::fs::write(
        cache.join("state/daemon.json"),
        serde_json::to_vec(&json!({
            "pid":std::process::id(), "socket":endpoint, "build_id":protocol::build_id(),
            "schema_version":crate::store::migrations::SCHEMA_VERSION, "cache_root":cache,
        }))
        .expect("state JSON"),
    )
    .expect("daemon state");
}

type Reader = FrameReader<BufReader<tokio::io::ReadHalf<platform::Stream>>>;

fn terminal(frame: &std::io::Result<Frame>) -> bool {
    matches!(frame, Ok(Frame::Eof))
        || matches!(frame, Err(error) if matches!(error.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset))
}

async fn serve(stream: platform::Stream, state: std::sync::Arc<State>) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = FrameReader::new(BufReader::new(read));
    let mut indexed = false;
    loop {
        let frame = reader.read_frame().await;
        if terminal(&frame) {
            if indexed {
                state.closed.send(()).expect("indexed peer closed");
            }
            return;
        }
        let Ok(Frame::Line(line)) = frame else {
            panic!("native fake read: {frame:?}");
        };
        let request = protocol::decode_request(&line).expect("request");
        let response = if matches!(&request, Request::Search { .. }) {
            search_reply(&state, request, &mut reader).await
        } else {
            state.sent.send(request.clone()).expect("observe request");
            Some(match request {
                Request::Hello { .. } => Response::Hello {
                    protocol_version: protocol::PROTOCOL_VERSION,
                    build_id: protocol::build_id(),
                    schema_version: 1,
                    capabilities: if state.capable {
                        vec![SEARCH_CONTEXT_CAPABILITY.to_owned()]
                    } else {
                        Vec::new()
                    },
                },
                Request::RegisterSession { .. } => match &state.mode {
                    Mode::IndexThenRejectRebind(rejection) if indexed => match rejection {
                        RebindRejection::Admission => Response::Error {
                            code: -32010,
                            message: "session limit reached".to_owned(),
                            instruction: None,
                        },
                        RebindRejection::Protocol => Response::Error {
                            code: -32011,
                            message: "session registration rejected".to_owned(),
                            instruction: None,
                        },
                        RebindRejection::Transport => return,
                    },
                    Mode::IndexThenRejectRebind(_) => Response::Error {
                        code: -32012,
                        message: "project is not indexed".to_owned(),
                        instruction: Some("run index_project".to_owned()),
                    },
                    _ => Response::ToolResult { value: Value::Null },
                },
                Request::RegisterUnbound { .. } | Request::BindReadOnly { .. } => {
                    Response::ToolResult { value: Value::Null }
                }
                Request::CallTool { name, .. }
                    if name == "index_project"
                        && matches!(state.mode, Mode::IndexThenRejectRebind(_)) =>
                {
                    indexed = true;
                    Response::ToolResult {
                        value: json!({"indexed":1}),
                    }
                }
                Request::CallTool { .. } => Response::ToolResult {
                    value: json!({"legacy":true}),
                },
                Request::Search { .. } | Request::StopDaemon => panic!("unexpected fake request"),
            })
        };
        let Some(response) = response else {
            return;
        };
        write
            .write_all(&protocol::encode(&response).expect("response frame"))
            .await
            .expect("fake reply");
    }
}

async fn search_reply(state: &State, request: Request, reader: &mut Reader) -> Option<Response> {
    let ordinal = state
        .calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    state.sent.send(request).expect("observe scoped search");
    match &state.mode {
        Mode::HoldFirst if ordinal == 0 => {
            tokio::select! {
                () = state.release.notified() => {}
                frame = reader.read_frame() => {
                    assert!(terminal(&frame), "active scoped peer must close: {frame:?}");
                    state.closed.send(()).expect("observe closure");
                    return None;
                }
            }
        }
        Mode::CloseFirst if ordinal == 0 => return None,
        Mode::CloseAlways => return None,
        Mode::CancelFirst(token) if ordinal == 0 => {
            token.cancel();
            return None;
        }
        _ => {}
    }
    Some(Response::ToolResult { value: json!([]) })
}

async fn check_post_index_rebind_rejection(rejection: RebindRejection) {
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::with_mode(true, Mode::IndexThenRejectRebind(rejection));
        let result = fake
            .frontend
            .proxy("index_project", json!({}))
            .await
            .expect("successful index survives rejected rebind");
        assert_eq!(result, json!({"indexed":1}));
        let mut requests = Vec::new();
        while let Ok(request) = fake.received.try_recv() {
            if !matches!(request, Request::Hello { .. }) {
                requests.push(request);
            }
        }
        assert_eq!(
            requests
                .iter()
                .filter(|request| matches!(request, Request::CallTool { name, .. } if name == "index_project"))
                .count(),
            1,
            "post-index rejection must never replay indexing"
        );
        assert!(
            matches!(requests.as_slice(), [
                Request::RegisterSession { .. },
                Request::RegisterUnbound { .. } | Request::BindReadOnly { .. },
                Request::CallTool { name, .. },
                Request::RegisterSession { .. },
            ] if name == "index_project"),
            "real unbound registration, index and rejected rebind: {requests:?}"
        );
        assert!(
            fake.frontend.conn.lock().await.is_none(),
            "rejected post-index lease must be discarded"
        );
        if !matches!(rejection, RebindRejection::Transport) {
            fake.closed.recv().await.expect("discarded native lease closed");
        }
    })
    .await
    .expect("post-index rebind watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_index_rebind_admission_discards_lease() {
    check_post_index_rebind_rejection(RebindRejection::Admission).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_index_rebind_protocol_discards_lease() {
    check_post_index_rebind_rejection(RebindRejection::Protocol).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_index_rebind_transport_discards_lease() {
    check_post_index_rebind_rejection(RebindRejection::Transport).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frontend_refuses_search_when_daemon_lacks_scoped_capability() {
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::new(false);
        let mut policy = crate::search::settings::standalone()
            .expect("owner policy")
            .into_settings();
        policy.request_timeout_ms =
            u64::try_from((WATCHDOG * 2).as_millis()).expect("safety policy");
        fake.frontend = Frontend::with_search_policy(fake.cache.path().to_path_buf(), policy);
        let result = fake
            .frontend
            .proxy("search_docs", json!({"query":"widgets"}))
            .await;
        assert!(
            matches!(result, Err(Error::Admission { .. })),
            "old daemon must be refused: {result:?}"
        );
        assert!(
            result
                .expect_err("legacy refusal")
                .to_string()
                .contains("restart")
        );
        while let Ok(request) = fake.received.try_recv() {
            assert!(!matches!(
                request,
                Request::Search { .. } | Request::CallTool { .. }
            ));
        }
    })
    .await
    .expect("capability watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_frontend_retains_original_start_and_complete_owner_policy() {
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::new(true);
        let mut policy = crate::search::settings::standalone()
            .expect("owner policy")
            .into_settings();
        policy.request_timeout_ms =
            u64::try_from((WATCHDOG * 2).as_millis()).expect("explicit safety policy");
        let budget = policy.request_timeout_ms;
        fake.frontend = Frontend::with_search_policy(fake.cache.path().to_path_buf(), policy);
        let started = std::time::Instant::now()
            .checked_sub(Duration::from_millis(12))
            .expect("original ingress");
        let result = fake
            .frontend
            .proxy_cancellable(
                "search_docs",
                json!({"query":"widgets"}),
                started,
                std::future::pending(),
            )
            .await
            .expect("scoped response");
        assert_eq!(result, json!([]));
        let mut scoped = None;
        while let Ok(request) = fake.received.try_recv() {
            if let Request::Search { context, .. } = request {
                scoped = Some(context);
            }
        }
        let context = scoped.expect("scoped context sent");
        assert!(context.elapsed_ms >= 12);
        assert!(context.remaining_ms > 0);
        assert!(context.elapsed_ms.saturating_add(context.remaining_ms) <= budget);
    })
    .await
    .expect("original budget watchdog");
}

struct McpPeer {
    write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    read: FrameReader<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    server: tokio::task::JoinHandle<()>,
}

impl McpPeer {
    async fn new(frontend: Frontend) -> Self {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (read, write) = tokio::io::split(server);
        let server = tokio::spawn(async move {
            let transport = rmcp::transport::async_rw::AsyncRwTransport::<
                rmcp::service::RoleServer,
                _,
                _,
            >::new_server(
                super::CappedRead::new(read, protocol::MAX_FRAME_BYTES),
                write,
            );
            rmcp::serve_server(frontend, transport)
                .await
                .expect("MCP initialized")
                .waiting()
                .await
                .expect("MCP stopped");
        });
        let (read, write) = tokio::io::split(client);
        let mut peer = Self {
            write,
            read: FrameReader::new(BufReader::new(read)),
            server,
        };
        peer.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"test","version":"0"}
        }})).await;
        let Frame::Line(line) = peer.read.read_frame().await.expect("initialize reply") else {
            panic!("missing reply");
        };
        let initialized: Value = serde_json::from_slice(&line).expect("initialize JSON");
        assert_eq!(initialized["id"], 1);
        peer.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        peer
    }

    async fn send(&mut self, message: Value) {
        self.write
            .write_all(&protocol::encode(&message).expect("MCP frame"))
            .await
            .expect("MCP send");
    }
}

impl Drop for McpPeer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_mcp_cancel_notification_closes_only_its_scoped_exchange() {
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::with_mode(true, Mode::HoldFirst);
        let mut peer = McpPeer::new(fake.frontend.clone()).await;
        peer.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"search_docs","arguments":{"query":"widgets"}}}))
            .await;
        fake.search_received().await;
        peer.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":2,"reason":"test cancellation"}}))
            .await;
        fake.closed
            .recv()
            .await
            .expect("native scoped peer closed after actual rmcp cancellation");
        assert_eq!(fake.calls(), 1, "cancelled request cannot reconnect/replay");
        assert!(fake.frontend.conn.lock().await.is_none());
    })
    .await
    .expect("real MCP cancellation watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_queued_caller_preserves_the_active_connection_lease() {
    use crate::hybrid::work::Cancellation;
    use crate::search::operation::SearchOperation;
    use std::future::{Future as _, poll_fn};
    use std::task::Poll;
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::with_mode(true, Mode::HoldFirst);
        let active_frontend = fake.frontend.clone();
        let active = tokio::spawn(async move {
            active_frontend
                .proxy("search_docs", json!({"query":"active"}))
                .await
        });
        fake.search_received().await;
        let operation =
            SearchOperation::new(&policy(), std::time::Instant::now(), Cancellation::new())
                .expect("queued operation");
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let mut queued = Box::pin(super::run_cancellable(
            Some(&operation),
            fake.frontend
                .proxy_ready("search_docs", json!({"query":"queued"}), Some(&operation)),
            async {
                let _ = cancelled.await;
            },
        ));
        assert!(poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await);
        cancel.send(()).expect("cancel queued waiter");
        let result = (&mut queued).await;
        drop(queued);
        let still_active = fake.closed.try_recv().is_err() && fake.calls() == 1;
        fake.state.release.notify_one();
        let active_result = active.await.expect("active task joined");
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert!(operation.cancellation().is_cancelled());
        assert!(still_active);
        assert_eq!(active_result.expect("live caller survived"), json!([]));
        assert!(
            fake.frontend
                .conn
                .try_lock()
                .expect("active lease restored")
                .is_some()
        );
    })
    .await
    .expect("queued cancellation watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transport_retry_retains_one_original_policy_budget() {
    tokio::time::timeout(WATCHDOG, async {
        let mut fake = Fake::with_mode(true, Mode::CloseFirst);
        let started = std::time::Instant::now()
            .checked_sub(Duration::from_millis(12))
            .expect("original ingress");
        let value = fake
            .frontend
            .proxy_cancellable(
                "search_docs",
                json!({"query":"widgets"}),
                started,
                std::future::pending(),
            )
            .await
            .expect("single reconnect succeeded");
        assert_eq!(value, json!([]));
        let first = fake.search_received().await;
        let second = fake.search_received().await;
        assert_eq!(fake.calls(), 2);
        assert!(first.elapsed_ms >= 12 && second.elapsed_ms >= first.elapsed_ms);
        let budget = policy().request_timeout_ms;
        for context in [first, second] {
            assert!(context.elapsed_ms.saturating_add(context.remaining_ms) <= budget);
        }
    })
    .await
    .expect("retry budget watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_transport_failure_stops_after_one_reconnect() {
    tokio::time::timeout(WATCHDOG, async {
        let fake = Fake::with_mode(true, Mode::CloseAlways);
        let result = fake
            .frontend
            .proxy("search_docs", json!({"query":"widgets"}))
            .await;
        assert!(matches!(result, Err(Error::Transport { .. })));
        assert_eq!(fake.calls(), 2);
        assert!(
            fake.frontend
                .conn
                .try_lock()
                .expect("failed lease unlocked")
                .is_none()
        );
    })
    .await
    .expect("bounded retry watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_operation_cannot_replay_after_transport_failure() {
    use crate::hybrid::work::Cancellation;
    use crate::search::operation::SearchOperation;
    tokio::time::timeout(WATCHDOG, async {
        let cancellation = Cancellation::new();
        let fake = Fake::with_mode(true, Mode::CancelFirst(cancellation.clone()));
        let operation = SearchOperation::new(&policy(), std::time::Instant::now(), cancellation)
            .expect("original operation");
        let result = super::run_cancellable(
            Some(&operation),
            fake.frontend
                .proxy_in("search_docs", json!({"query":"widgets"}), Some(&operation)),
            std::future::pending(),
        )
        .await;
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert!(operation.cancellation().is_cancelled());
        assert_eq!(fake.calls(), 1);
    })
    .await
    .expect("no replay watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_during_actual_startup_cannot_advance_to_search() {
    use crate::hybrid::work::Cancellation;
    use crate::search::operation::SearchOperation;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc as std_mpsc,
    };
    tokio::time::timeout(WATCHDOG, async {
        let operation =
            SearchOperation::new(&policy(), std::time::Instant::now(), Cancellation::new())
                .expect("original operation");
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, proceed) = std_mpsc::channel();
        let (done, completed) = tokio::sync::oneshot::channel();
        let active = Arc::new(AtomicBool::new(false));
        let active_worker = Arc::clone(&active);
        let paid = Arc::new(AtomicUsize::new(0));
        let paid_work = Arc::clone(&paid);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let mut pending = Box::pin(super::run_cancellable(
            Some(&operation),
            async {
                super::ensure_using(
                    Path::new("unused-test-cache"),
                    Some(&operation),
                    move |_| {
                        active_worker.store(true, Ordering::SeqCst);
                        let _ = entered.send(());
                        let result = proceed.recv_timeout(WATCHDOG);
                        active_worker.store(false, Ordering::SeqCst);
                        let _ = done.send(());
                        result.map_err(|error| Error::internal(error.to_string()))
                    },
                )
                .await?;
                paid_work.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Null)
            },
            async {
                let _ = cancelled.await;
            },
        ));
        tokio::select! {
            entered = observed => entered.expect("real blocking startup entered"),
            result = &mut pending => panic!("startup completed before gate: {result:?}"),
        }
        cancel.send(()).expect("cancel startup waiter");
        let result = (&mut pending).await;
        drop(pending);
        let still_running = active.load(Ordering::SeqCst);
        let _ = release.send(());
        completed.await.expect("actual startup worker completed");
        assert!(matches!(result, Err(Error::Admission { .. })));
        assert!(operation.cancellation().is_cancelled());
        assert!(
            still_running,
            "caller cancellation does not preempt synchronous startup"
        );
        assert_eq!(paid.load(Ordering::SeqCst), 0);
    })
    .await
    .expect("startup cancellation watchdog");
}

#[test]
fn cancelled_startup_queued_on_blocking_pool_cannot_start_daemon() {
    use crate::hybrid::work::Cancellation;
    use crate::search::operation::SearchOperation;
    use std::future::{Future as _, poll_fn};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc as std_mpsc,
    };
    use std::task::Poll;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("bounded runtime");
    let starts = Arc::new(AtomicUsize::new(0));
    let observations = runtime.block_on(async {
        tokio::time::timeout(WATCHDOG, async {
            let (entered, observed) = tokio::sync::oneshot::channel();
            let (release, proceed) = std_mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                let _ = entered.send(());
                proceed
                    .recv_timeout(WATCHDOG)
                    .expect("blocking pool released");
            });
            observed.await.expect("only blocking slot occupied");
            let operation =
                SearchOperation::new(&policy(), std::time::Instant::now(), Cancellation::new())
                    .expect("queued operation");
            let worker_starts = Arc::clone(&starts);
            let mut pending = Box::pin(super::run_cancellable(
                Some(&operation),
                super::ensure_using(
                    Path::new("unused-test-cache"),
                    Some(&operation),
                    move |_| {
                        worker_starts.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                ),
                std::future::pending(),
            ));
            let queued = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
            drop(pending);
            let cancelled = operation.cancellation().is_cancelled();
            let _ = release.send(());
            occupied.await.expect("occupied worker joined");
            (queued, cancelled)
        })
        .await
        .expect("queued startup watchdog")
    });
    drop(runtime); // joins the actual detached queued startup closure too
    assert_eq!(observations, (true, true));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
}
