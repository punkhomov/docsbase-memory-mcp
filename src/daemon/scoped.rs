//! Opt-in search lifetime and bounded delivery, independent of provider CPU.

use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::ipc::framing::{Frame, FrameReader};
use crate::ipc::protocol::{self, Response};
use crate::ipc::protocol::{Request, SearchCallContext};
use crate::search::operation::SearchOperation;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

use super::{Activity, Peer, Shared, call_tool_in, error_response, search_operation};

pub(super) struct Scope {
    pub(super) activity: Arc<Activity>,
    pub(super) operation: Option<SearchOperation>,
}

impl Scope {
    pub(super) fn new(shared: &Shared, request: &Request, activity: Arc<Activity>) -> Result<Self> {
        let operation = match request {
            Request::Search { context, .. } => Some(from_context(shared, context)?),
            Request::CallTool { name, .. } => search_operation(shared, name)?,
            _ => None,
        };
        Ok(Self {
            activity,
            operation,
        })
    }
}

fn from_context(shared: &Shared, context: &SearchCallContext) -> Result<SearchOperation> {
    context.validate()?;
    let now = Instant::now();
    let start = now
        .checked_sub(Duration::from_millis(context.elapsed_ms))
        .ok_or_else(|| Error::Admission {
            message: "scoped search elapsed time exceeds this platform's clock range".to_owned(),
        })?;
    let deadline = now
        .checked_add(Duration::from_millis(context.remaining_ms))
        .ok_or_else(|| Error::Admission {
            message: "scoped search remaining time exceeds this platform's clock range".to_owned(),
        })?;
    let operation = SearchOperation::constrained(
        &shared.global_config.search,
        start,
        deadline,
        Cancellation::new(),
    )?;
    operation.record_queue(Duration::from_millis(context.queue_ms));
    Ok(operation)
}

pub(super) async fn call<R: AsyncBufRead + Unpin>(
    shared: &Arc<Shared>,
    peer: &mut Peer,
    args: Value,
    scope: &Scope,
    reader: &mut FrameReader<R>,
) -> (Response, bool) {
    let Some(operation) = scope.operation.as_ref() else {
        return (
            error_response(&Error::internal(
                "scoped search lacks an admitted operation",
            )),
            true,
        );
    };
    if let Err(error) = operation.check() {
        return (error_response(&error), true);
    }
    let work = call_tool_in(
        shared,
        peer.session_id,
        "search_docs",
        args,
        Some(operation.clone()),
        Some(Arc::clone(&scope.activity)),
    );
    let cancelled = async {
        match peer.cancel_rx.as_mut() {
            Some(cancel) => {
                let _ = cancel.await;
            }
            None => std::future::pending().await,
        }
    };
    let deadline = tokio::time::Instant::from_std(operation.deadline());
    tokio::select! {
        biased;
        () = cancelled => {
            operation.cancellation().cancel();
            (error_response(&Error::Admission {
                message: "scoped search session closed".to_owned(),
            }), true)
        }
        () = tokio::time::sleep_until(deadline) => {
            operation.cancellation().cancel();
            (error_response(&Error::Admission {
                message: "search request deadline expired".to_owned(),
            }), true)
        }
        frame = reader.read_frame() => {
            operation.cancellation().cancel();
            let error = match frame {
                Ok(Frame::Eof) => Error::Admission {
                    message: "scoped search connection closed".to_owned(),
                },
                Ok(Frame::TooLarge) => Error::Protocol {
                    message: format!("frame exceeds {} bytes", protocol::MAX_FRAME_BYTES),
                },
                Ok(Frame::Incomplete) => Error::Protocol {
                    message: "incomplete frame without delimiter".to_owned(),
                },
                Ok(Frame::Line(_)) => Error::Protocol {
                    message: "one outstanding scoped search per connection; wait for its reply".to_owned(),
                },
                Err(error) => Error::transport_with_source("scoped search connection read", error),
            };
            (error_response(&error), true)
        }
        result = work => {
            let close = operation.cancellation().is_cancelled();
            let response = match result {
                Ok(value) => Response::ToolResult { value },
                Err(error) => error_response(&error),
            };
            (response, close)
        }
    }
}

pub(super) async fn reply<W: AsyncWrite + Unpin>(
    writer: &mut W,
    mut response: Response,
    operation: Option<&SearchOperation>,
    immediate: bool,
) -> bool {
    let boundary = operation.and_then(|operation| operation.check().err());
    let expired = immediate || boundary.is_some();
    if let Some(error) = boundary
        && matches!(response, Response::ToolResult { .. })
    {
        response = error_response(&error);
    }
    let Some(bytes) = response_bytes(&response) else {
        return false;
    };
    drop(response);
    if expired {
        // No extra time budget for an already-expired error reply. This may
        // deliver a complete frame, a prefix, or no bytes before closure.
        let mut writing = std::pin::pin!(writer.write_all(&bytes));
        let _ = poll_fn(|cx| Poll::Ready(writing.as_mut().poll(cx))).await;
        return false;
    }
    match operation {
        Some(operation) => {
            let deadline = tokio::time::Instant::from_std(operation.deadline());
            write_before(
                writer,
                &bytes,
                operation,
                tokio::time::sleep_until(deadline),
            )
            .await
        }
        None => writer.write_all(&bytes).await.is_ok(),
    }
}

fn response_bytes(response: &Response) -> Option<Vec<u8>> {
    let bytes = protocol::encode(response).ok()?;
    if bytes.len() <= protocol::MAX_FRAME_BYTES + 1 {
        return Some(bytes);
    }
    let error = Error::Protocol {
        message: format!(
            "response frame is {} bytes, over {}; reduce max_file_size or the document size",
            bytes.len(),
            protocol::MAX_FRAME_BYTES,
        ),
    };
    protocol::encode(&error_response(&error)).ok()
}

async fn write_before<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
    operation: &SearchOperation,
    expiry: impl Future<Output = ()>,
) -> bool {
    tokio::select! {
        biased;
        () = expiry => {
            operation.cancellation().cancel();
            false
        }
        result = writer.write_all(bytes) => operation.check().is_ok() && result.is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future as _, poll_fn};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc as std_mpsc};
    use std::task::Poll;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
    use tokio::sync::{Semaphore, mpsc, oneshot};

    use crate::daemon::request::Activity;
    use crate::hybrid::HybridStages;
    use crate::hybrid::config::Ranking;
    use crate::hybrid::work::Cancellation;
    use crate::ipc::framing::FrameReader;
    use crate::search::operation::SearchOperation;

    use super::super::Peer;
    use super::super::search_phase_tests::{CountingReranker, Fixture, TestEmbedder};
    use super::*;

    const WATCHDOG: Duration = Duration::from_secs(300);

    trait TestIo: AsyncRead + AsyncWrite + Unpin + Send {}
    impl<T: AsyncRead + AsyncWrite + Unpin + Send> TestIo for T {}

    struct Peers {
        client: Box<dyn TestIo>,
        reader: FrameReader<BufReader<tokio::io::ReadHalf<Box<dyn TestIo>>>>,
        writer: tokio::io::WriteHalf<Box<dyn TestIo>>,
    }

    impl Peers {
        async fn new(native: bool, cache: &std::path::Path) -> Self {
            tokio::time::timeout(WATCHDOG, async {
                let (mut client, backend): (Box<dyn TestIo>, Box<dyn TestIo>) = if native {
                    let endpoint = crate::platform::daemon_endpoint(cache);
                    if let crate::platform::Endpoint::Unix(path) = &endpoint {
                        crate::platform::fs::secure_dir(path.parent().expect("socket directory"))
                            .expect("private socket directory");
                    }
                    let listener = crate::platform::bind(&endpoint).expect("native listener");
                    let (client, backend) = tokio::try_join!(
                        crate::platform::connect_async(&endpoint),
                        listener.accept(),
                    )
                    .expect("native streams");
                    (Box::new(client), Box::new(backend))
                } else {
                    let (client, backend) = tokio::io::duplex(1024);
                    (Box::new(client), Box::new(backend))
                };
                let (read, mut writer) = tokio::io::split(backend);
                let mut reader = FrameReader::new(BufReader::new(read));
                // Exchange multiple frames before closure, as real IPC does.
                client
                    .write_all(b"hello\nregister\n")
                    .await
                    .expect("prelude");
                assert!(matches!(
                    reader.read_frame().await.expect("hello"),
                    Frame::Line(_)
                ));
                assert!(matches!(
                    reader.read_frame().await.expect("register"),
                    Frame::Line(_)
                ));
                writer.write_all(b"ok\n").await.expect("prelude reply");
                let mut reply = [0; 3];
                client
                    .read_exact(&mut reply)
                    .await
                    .expect("read prelude reply");
                assert_eq!(&reply, b"ok\n");
                Self {
                    client,
                    reader,
                    writer,
                }
            })
            .await
            .expect("native/prelude watchdog")
        }
    }

    #[tokio::test]
    async fn reply_expiry_releases_backpressure_without_a_fresh_budget() {
        let count = Arc::new(AtomicUsize::new(0));
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots).try_acquire_owned().expect("permit");
        let (events, mut settled) = mpsc::channel(1);
        let activity = Activity::new(&count, &events, Some(&Arc::new(permit)));
        let policy = crate::search::settings::standalone().expect("owner policy");
        let operation =
            SearchOperation::new(policy.settings(), Instant::now(), Cancellation::new())
                .expect("operation");
        let (client, mut writer) = tokio::io::duplex(1);
        let (trigger, expiry) = oneshot::channel();
        let mut writing = Box::pin(async {
            let written = write_before(&mut writer, &[b'x'; 64], &operation, async {
                let _ = expiry.await;
            })
            .await;
            drop(activity);
            written
        });
        let blocked = poll_fn(|cx| Poll::Ready(writing.as_mut().poll(cx).is_pending())).await;
        let held =
            count.load(Ordering::SeqCst) == 1 && Arc::clone(&slots).try_acquire_owned().is_err();
        let _ = trigger.send(());
        let result = if blocked {
            poll_fn(|cx| {
                Poll::Ready(match writing.as_mut().poll(cx) {
                    Poll::Ready(written) => Some(written),
                    Poll::Pending => None,
                })
            })
            .await
        } else {
            None
        };
        drop(writing);
        drop(client);
        assert!(blocked && held);
        assert_eq!(result, Some(false));
        assert!(operation.cancellation().is_cancelled());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
        settled.try_recv().expect("wire activity settled");
    }

    #[test]
    fn scoped_eof_returns_while_actual_provider_remains_charged() {
        eof_case(false);
    }

    #[test]
    fn native_scoped_eof_returns_while_actual_provider_remains_charged() {
        eof_case(true);
    }

    struct Gated {
        fixture: Fixture,
        entered: std_mpsc::Receiver<()>,
        release: Option<std_mpsc::Sender<()>>,
        reranks: Arc<AtomicUsize>,
    }

    impl Gated {
        fn new() -> Self {
            let (entered, observed) = std_mpsc::channel();
            let (release, proceed) = std_mpsc::channel();
            let reranks = Arc::new(AtomicUsize::new(0));
            let fixture = Fixture::new(HybridStages {
                embedder: Some(Arc::new(TestEmbedder {
                    entered: Some(entered),
                    release: Mutex::new(proceed),
                    calls: Arc::default(),
                })),
                ranking: Ranking::Rrf,
                reranker: Some(Arc::new(CountingReranker(Arc::clone(&reranks)))),
            });
            Self {
                fixture,
                entered: observed,
                release: Some(release),
                reranks,
            }
        }

        fn proceed(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
        }

        fn operation() -> SearchOperation {
            let mut policy = crate::search::settings::standalone()
                .expect("owner policy")
                .into_settings();
            policy.request_timeout_ms =
                u64::try_from((WATCHDOG * 2).as_millis()).expect("test safety policy");
            SearchOperation::new(&policy, Instant::now(), Cancellation::new())
                .expect("explicit test operation")
        }
    }

    impl Drop for Gated {
        fn drop(&mut self) {
            self.proceed();
        }
    }

    #[test]
    fn second_frame_during_scoped_search_does_not_dispatch_stop() {
        cancellation_case(false);
    }

    #[test]
    fn session_cancellation_stops_scoped_wait_before_provider_returns() {
        cancellation_case(true);
    }

    fn cancellation_case(session: bool) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let mut gated = Gated::new();
        let count = Arc::new(AtomicUsize::new(0));
        let (events, mut settled) = mpsc::channel(1);
        let observations = runtime.block_on(async {
            let operation = Gated::operation();
            let scope = Scope {
                activity: Activity::new(&count, &events, None),
                operation: Some(operation.clone()),
            };
            let mut peers = Peers::new(false, &gated.fixture.shared.cache).await;
            let (cancel, cancelled) = oneshot::channel();
            let mut peer = Peer {
                hello_done: true,
                session_id: Some(gated.fixture.session),
                cancel_rx: Some(cancelled),
            };
            let mut pending = Box::pin(call(
                &gated.fixture.shared,
                &mut peer,
                json!({"query":"widgets", "rerank":true}),
                &scope,
                &mut peers.reader,
            ));
            let first = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
            let entered = first && gated.entered.recv_timeout(WATCHDOG).is_ok();
            if session {
                cancel.send(()).expect("session signal");
            } else {
                peers
                    .client
                    .write_all(&protocol::encode(&Request::StopDaemon).expect("stop frame"))
                    .await
                    .expect("second frame");
            }
            let response = poll_fn(|cx| {
                Poll::Ready(match pending.as_mut().poll(cx) {
                    Poll::Ready((Response::Error { code, .. }, true)) => Some(code),
                    _ => None,
                })
            })
            .await;
            let cancelled = operation.cancellation().is_cancelled();
            drop(pending);
            drop(scope);
            (entered, response, cancelled, count.load(Ordering::SeqCst))
        });
        gated.proceed();
        drop(runtime);
        let code = if session { -32010 } else { -32011 };
        assert_eq!(observations, (true, Some(code), true, 1));
        assert!(!gated.fixture.shared.stopping.load(Ordering::SeqCst));
        assert_eq!(gated.reranks.load(Ordering::SeqCst), 0);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        settled.try_recv().expect("physical work settled");
    }

    #[test]
    fn partial_next_frame_survives_scoped_work_completion() {
        completion_case(false);
    }

    #[test]
    fn legacy_half_close_waits_for_the_search_reply() {
        completion_case(true);
    }

    fn completion_case(legacy: bool) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let mut gated = Gated::new();
        let observations = runtime.block_on(async {
            let count = Arc::new(AtomicUsize::new(0));
            let (events, _) = mpsc::channel(1);
            let scope = Scope {
                activity: Activity::new(&count, &events, None),
                operation: Some(Gated::operation()),
            };
            let mut peers = Peers::new(false, &gated.fixture.shared.cache).await;
            let mut peer = Peer {
                hello_done: true,
                session_id: Some(gated.fixture.session),
                cancel_rx: None,
            };
            let (shutdown, _) = tokio::sync::watch::channel(false);
            let args = json!({"query":"widgets", "rerank":true});
            if legacy {
                peers.client.shutdown().await.expect("write half-close");
            } else {
                peers.client.write_all(b"partial").await.expect("prefix");
            }
            let shared = Arc::clone(&gated.fixture.shared);
            let mut pending = Box::pin(async {
                if legacy {
                    super::super::handle_request(
                        &shared,
                        &shutdown,
                        &count,
                        &mut peer,
                        Request::CallTool {
                            name: "search_docs".to_owned(),
                            args,
                        },
                        &scope,
                        &mut peers.reader,
                    )
                    .await
                } else {
                    call(&shared, &mut peer, args, &scope, &mut peers.reader).await
                }
            });
            let first = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
            let entered = first && gated.entered.recv_timeout(WATCHDOG).is_ok();
            let waiting = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
            gated.proceed();
            let result = tokio::time::timeout(WATCHDOG, &mut pending).await;
            let success = matches!(result, Ok((Response::ToolResult { .. }, false)));
            drop(pending);
            let preserved = if legacy {
                true
            } else {
                peers.client.write_all(b" suffix\n").await.expect("suffix");
                matches!(
                    tokio::time::timeout(WATCHDOG, peers.reader.read_frame()).await,
                    Ok(Ok(Frame::Line(line))) if line == b"partial suffix\n"
                )
            };
            (entered, waiting, success, preserved)
        });
        drop(runtime);
        assert_eq!(observations, (true, true, true, true));
        assert_eq!(gated.reranks.load(Ordering::SeqCst), 1);
    }

    fn eof_case(native: bool) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let count = Arc::new(AtomicUsize::new(0));
        let slots = Arc::new(Semaphore::new(1));
        let (events, mut settled) = mpsc::channel(1);
        let reranks = Arc::new(AtomicUsize::new(0));
        let (fixture, observations) = runtime.block_on(async {
            let (entered, observed) = std_mpsc::channel();
            let (release, proceed) = std_mpsc::channel();
            let fixture = Fixture::new(HybridStages {
                embedder: Some(Arc::new(TestEmbedder {
                    entered: Some(entered),
                    release: Mutex::new(proceed),
                    calls: Arc::default(),
                })),
                ranking: Ranking::Rrf,
                reranker: Some(Arc::new(CountingReranker(Arc::clone(&reranks)))),
            });
            let mut policy = crate::search::settings::standalone()
                .expect("owner policy")
                .into_settings();
            policy.request_timeout_ms =
                u64::try_from((WATCHDOG * 2).as_millis()).expect("test safety policy");
            let operation = SearchOperation::new(&policy, Instant::now(), Cancellation::new())
                .expect("explicit test operation");
            let permit = Arc::clone(&slots).try_acquire_owned().expect("permit");
            let scope = Scope {
                activity: Activity::new(&count, &events, Some(&Arc::new(permit))),
                operation: Some(operation.clone()),
            };
            let mut peers = Peers::new(native, &fixture.shared.cache).await;
            let mut peer = Peer {
                hello_done: true,
                session_id: Some(fixture.session),
                cancel_rx: None,
            };
            let mut pending = Box::pin(call(
                &fixture.shared,
                &mut peer,
                json!({"query":"widgets", "rerank":true}),
                &scope,
                &mut peers.reader,
            ));
            let first_pending =
                poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
            let entered = first_pending && observed.recv_timeout(WATCHDOG).is_ok();
            drop(peers.client);
            let returned = if entered {
                if native {
                    tokio::time::timeout(WATCHDOG, &mut pending)
                        .await
                        .is_ok_and(|(_, close)| close)
                } else {
                    poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_ready())).await
                }
            } else {
                false
            };
            let cancelled_before_cleanup = operation.cancellation().is_cancelled();
            drop(pending);
            drop(peers.reader);
            drop(peers.writer);
            drop(scope);
            let held = count.load(Ordering::SeqCst) == 1
                && Arc::clone(&slots).try_acquire_owned().is_err();
            let _ = release.send(());
            (fixture, (entered, returned, cancelled_before_cleanup, held))
        });
        drop(runtime);
        assert_eq!(observations, (true, true, true, true));
        assert_eq!(reranks.load(Ordering::SeqCst), 0);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
        settled.try_recv().expect("actual work settled");
        assert!(fixture.shared.read_pool().try_acquire().is_ok());
    }
}
