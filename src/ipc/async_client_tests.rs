//! Native client lifecycle tests; gates establish ordering, not elapsed sleeps.

use std::future::{Future as _, poll_fn};
use std::task::Poll;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use super::async_client::AsyncClient;
use super::framing::{Frame, FrameReader};
use super::protocol::{self, Request, Response, SEARCH_CONTEXT_CAPABILITY};
use crate::error::Error;
use crate::hybrid::work::Cancellation;
use crate::platform;
use crate::search::operation::SearchOperation;

const WATCHDOG: Duration = Duration::from_secs(300);

struct Harness {
    client: AsyncClient,
    entered: oneshot::Receiver<()>,
    server: tokio::task::JoinHandle<Vec<Request>>,
    _cache: tempfile::TempDir,
}

impl Harness {
    async fn new(capable: bool, answer: Option<Response>) -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let endpoint = platform::daemon_endpoint(cache.path());
        if let platform::Endpoint::Unix(path) = &endpoint {
            platform::fs::secure_dir(path.parent().expect("socket directory"))
                .expect("runtime directory");
        }
        let listener = platform::bind(&endpoint).expect("native listener");
        let (client, stream) =
            tokio::try_join!(AsyncClient::connect(cache.path()), listener.accept(),)
                .expect("native connection");
        let (entered, observed) = oneshot::channel();
        let server = tokio::spawn(serve(stream, capable, answer, entered));
        Self {
            client,
            entered: observed,
            server,
            _cache: cache,
        }
    }
}

async fn serve(
    stream: platform::Stream,
    capable: bool,
    answer: Option<Response>,
    entered: oneshot::Sender<()>,
) -> Vec<Request> {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = FrameReader::new(BufReader::new(read));
    let mut requests = Vec::new();
    let mut entered = Some(entered);
    loop {
        let line = match reader.read_frame().await {
            Ok(Frame::Line(line)) => line,
            Ok(Frame::Eof) => return requests,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return requests;
            }
            other => panic!("unexpected native frame: {other:?}"),
        };
        let request = protocol::decode_request(&line).expect("request");
        let response = match &request {
            Request::Hello { .. } => Some(Response::Hello {
                protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(),
                schema_version: 1,
                capabilities: if capable {
                    vec![SEARCH_CONTEXT_CAPABILITY.to_owned()]
                } else {
                    Vec::new()
                },
            }),
            Request::RegisterSession { .. }
            | Request::RegisterUnbound { .. }
            | Request::BindReadOnly { .. }
            | Request::CallTool { .. } => Some(Response::ToolResult { value: Value::Null }),
            Request::Search { .. } => {
                if answer.is_none() {
                    write
                        .write_all(b"{\"partial\":")
                        .await
                        .expect("response prefix");
                }
                if let Some(entered) = entered.take() {
                    let _ = entered.send(());
                }
                answer.clone()
            }
            Request::StopDaemon => panic!("unexpected stop"),
        };
        requests.push(request);
        if let Some(response) = response {
            write
                .write_all(&protocol::encode(&response).expect("encode reply"))
                .await
                .expect("native reply");
        }
    }
}

fn operation() -> SearchOperation {
    let mut settings = crate::search::settings::standalone()
        .expect("owner policy")
        .into_settings();
    settings.request_timeout_ms = u64::try_from((WATCHDOG * 2).as_millis()).expect("safety policy");
    let started = Instant::now()
        .checked_sub(Duration::from_millis(12))
        .expect("original ingress");
    let operation =
        SearchOperation::new(&settings, started, Cancellation::new()).expect("operation");
    operation.record_queue(Duration::from_millis(3));
    operation
}

#[tokio::test]
async fn scoped_client_sends_original_context_and_finalizes_explain_total() {
    tokio::time::timeout(WATCHDOG, async {
        let mut harness = Harness::new(
            true,
            Some(Response::ToolResult {
                value: json!({"hits":[], "meta":{"queue_ms":3,"stage_ms":{"total":1}}}),
            }),
        )
        .await;
        harness.client.handshake_registry().await.expect("hello");
        let operation = operation();
        let value = harness
            .client
            .search(json!({"query":"widgets", "explain":true}), &operation)
            .await
            .expect("search");
        assert!(value["meta"]["stage_ms"]["total"].as_u64().expect("total") >= 12);
        assert_eq!(
            value["meta"]["queue_ms"], 3,
            "upstream queue is not counted twice"
        );
        assert!(!operation.cancellation().is_cancelled());
        drop(harness.client);
        let requests = harness.server.await.expect("server joined");
        let Request::Search { context, .. } = requests.last().expect("search request") else {
            panic!("not scoped search");
        };
        assert!(context.elapsed_ms >= 12);
        assert!(context.remaining_ms > 0);
        assert!(
            u128::from(context.remaining_ms)
                <= operation
                    .deadline()
                    .duration_since(operation.started_at())
                    .as_millis()
        );
        assert_eq!(context.queue_ms, 3);
    })
    .await
    .expect("client watchdog");
}

#[tokio::test]
async fn missing_capability_refuses_before_sending_search() {
    tokio::time::timeout(WATCHDOG, async {
        let mut harness = Harness::new(false, None).await;
        harness
            .client
            .handshake_registry()
            .await
            .expect("legacy hello");
        let error = harness
            .client
            .search(json!({"query":"widgets"}), &operation())
            .await
            .expect_err("refuse old daemon");
        assert!(matches!(error, Error::Admission { .. }));
        assert!(error.to_string().contains("restart"));
        drop(harness.client);
        let requests = harness.server.await.expect("server joined");
        assert_eq!(
            requests.len(),
            1,
            "only Hello may be sent to an incapable daemon"
        );
    })
    .await
    .expect("capability watchdog");
}

#[tokio::test]
async fn dropping_search_poisons_connection_and_cancels_original_operation() {
    abandon_case(false).await;
}

#[tokio::test]
async fn expiry_poisons_pending_exchange_without_a_new_budget() {
    abandon_case(true).await;
}

async fn abandon_case(timed_out: bool) {
    tokio::time::timeout(WATCHDOG, async {
        let mut harness = Harness::new(true, None).await;
        harness.client.handshake_registry().await.expect("hello");
        let operation = operation();
        let (trigger, expiry) = oneshot::channel();
        let mut pending = Box::pin(harness.client.search_before(
            json!({"query":"widgets"}),
            &operation,
            async {
                let _ = expiry.await;
            },
        ));
        tokio::select! {
            result = &mut pending => panic!("search completed before abandonment: {result:?}"),
            entered = &mut harness.entered => entered.expect("server received search"),
        }
        let blocked = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
        assert!(blocked, "partial response cannot complete a search");
        if timed_out {
            trigger.send(()).expect("expire search");
            assert!(matches!((&mut pending).await, Err(Error::Admission { .. })));
        }
        drop(pending);
        assert!(operation.cancellation().is_cancelled());
        let reuse = harness
            .client
            .call(Request::CallTool {
                name: "status".to_owned(),
                args: json!({}),
            })
            .await;
        assert!(
            matches!(reuse, Err(Error::Transport { .. })),
            "interrupted exchange must not be reused"
        );
        drop(harness.client);
        let requests = harness.server.await.expect("server joined after closure");
        assert_eq!(requests.len(), 2, "no replay or reuse after abandonment");
    })
    .await
    .expect("abandonment watchdog");
}

#[tokio::test]
async fn scoped_client_preserves_default_array_after_session_registration() {
    tokio::time::timeout(WATCHDOG, async {
        let expected = json!([{"path":"guide.md", "score":1.0}]);
        let mut harness = Harness::new(true, Some(Response::ToolResult { value: expected.clone() })).await;
        harness.client.handshake(std::path::Path::new(".")).await.expect("bound session");
        harness.client.register_unbound(std::path::Path::new(".")).await.expect("unbound session");
        let actual = harness.client.search(json!({"query":"widgets"}), &operation()).await.expect("array search");
        assert_eq!(actual, expected);
        assert_eq!(harness.client.call_tool("status", json!({})).await.expect("status"), Value::Null);
        drop(harness.client);
        let requests = harness.server.await.expect("server joined");
        assert!(matches!(requests.as_slice(), [Request::Hello { .. }, Request::RegisterSession { .. }, Request::RegisterUnbound { .. } | Request::BindReadOnly { .. }, Request::Search { .. }, Request::CallTool { name, .. }] if name == "status"));
    }).await.expect("session watchdog");
}

#[tokio::test]
async fn daemon_query_error_retains_category_without_poisoning_healthy_exchange() {
    tokio::time::timeout(WATCHDOG, async {
        let query_error = Error::Query {
            message: "invalid query".to_owned(),
        };
        let mut harness = Harness::new(
            true,
            Some(Response::Error {
                code: query_error.mcp_code(),
                message: query_error.inner_message().clone(),
                instruction: None,
            }),
        )
        .await;
        harness.client.handshake_registry().await.expect("hello");
        let operation = operation();
        let error = harness
            .client
            .search(json!({"query":"["}), &operation)
            .await
            .expect_err("query error");
        assert!(matches!(error, Error::Query { .. }));
        assert!(!operation.cancellation().is_cancelled());
        harness
            .client
            .call_tool("status", json!({}))
            .await
            .expect("healthy connection reused");
        drop(harness.client);
        assert_eq!(harness.server.await.expect("server joined").len(), 3);
    })
    .await
    .expect("daemon error watchdog");
}

#[tokio::test]
async fn expired_original_operation_sends_no_search() {
    tokio::time::timeout(WATCHDOG, async {
        let mut harness = Harness::new(true, None).await;
        harness.client.handshake_registry().await.expect("hello");
        let settings = crate::search::settings::standalone().expect("owner policy");
        let started = Instant::now()
            .checked_sub(Duration::from_millis(
                settings.settings().request_timeout_ms,
            ))
            .expect("expired original start");
        let operation = SearchOperation::new(settings.settings(), started, Cancellation::new())
            .expect("original operation");
        let error = harness
            .client
            .search(json!({"query":"widgets"}), &operation)
            .await
            .expect_err("expired search");
        assert!(matches!(error, Error::Admission { .. }));
        assert!(operation.cancellation().is_cancelled());
        drop(harness.client);
        assert_eq!(harness.server.await.expect("server joined").len(), 1);
    })
    .await
    .expect("expired operation watchdog");
}

#[tokio::test]
async fn ready_work_cannot_hide_original_operation_cancellation() {
    let operation = operation();
    let result = super::async_client::within_operation(&operation, async {
        operation.cancellation().cancel();
        Ok(Value::Null)
    })
    .await;
    assert!(matches!(result, Err(Error::Admission { .. })));
}
