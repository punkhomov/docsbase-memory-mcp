//! Native negotiation must prevent a legacy peer from silently ignoring literal mode.
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use docsbase_memory::error::Error;
use docsbase_memory::hybrid::work::Cancellation;
use docsbase_memory::ipc::async_client::AsyncClient;
use docsbase_memory::ipc::client::Client;
use docsbase_memory::ipc::protocol::{self, Request, Response, SEARCH_CONTEXT_CAPABILITY};
use docsbase_memory::platform;
use docsbase_memory::search::operation::SearchOperation;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WATCHDOG: Duration = Duration::from_secs(300);
const LITERAL_CAP: &str = "literal-query-v1";

struct Peer {
    cache: TempDir,
    task: Option<tokio::task::JoinHandle<Vec<Request>>>,
}

impl Peer {
    fn new(literal: bool, fail_second_hello: bool) -> Self {
        Self::with_cache(
            tempfile::tempdir().expect("cache"),
            literal,
            fail_second_hello,
        )
    }

    fn with_cache(cache: TempDir, literal: bool, fail_second_hello: bool) -> Self {
        let endpoint = platform::daemon_endpoint(cache.path());
        if let platform::Endpoint::Unix(path) = &endpoint {
            platform::fs::secure_dir(path.parent().expect("runtime parent"))
                .expect("secure runtime");
        }
        let listener = platform::bind(&endpoint).expect("native bind");
        let mut capabilities = vec![SEARCH_CONTEXT_CAPABILITY.to_owned()];
        if literal {
            capabilities.push(LITERAL_CAP.to_owned());
        }
        let hello = |version| Response::Hello {
            protocol_version: version,
            build_id: protocol::build_id(),
            schema_version: 1,
            capabilities: capabilities.clone(),
        };
        let mut hellos = VecDeque::from([hello(protocol::PROTOCOL_VERSION)]);
        if fail_second_hello {
            hellos.push_back(hello(protocol::PROTOCOL_VERSION + 1));
        }
        let task = tokio::spawn(async move {
            let stream = listener.accept().await.expect("native accept");
            drop(listener);
            let (read, mut write) = tokio::io::split(stream);
            let mut reader = BufReader::new(read);
            let mut requests = Vec::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => panic!("native read: {error}"),
                }
                let request = protocol::decode_request(line.as_bytes()).expect("request");
                let response = match &request {
                    Request::Hello { .. } => hellos.pop_front().expect("expected Hello"),
                    Request::CallTool { args, .. } | Request::Search { args, .. } => {
                        Response::ToolResult {
                            value: args.clone(),
                        }
                    }
                    Request::RegisterSession { .. }
                    | Request::RegisterUnbound { .. }
                    | Request::BindReadOnly { .. } => Response::ToolResult { value: Value::Null },
                    Request::StopDaemon => panic!("unexpected stop"),
                };
                requests.push(request);
                write
                    .write_all(&protocol::encode(&response).expect("reply"))
                    .await
                    .expect("send reply");
            }
            requests
        });
        Self {
            cache,
            task: Some(task),
        }
    }

    async fn finish(mut self) -> Vec<Request> {
        self.task.take().expect("task").await.expect("peer joined")
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn operation() -> SearchOperation {
    let mut settings = docsbase_memory::search::settings::standalone()
        .expect("settings")
        .into_settings();
    settings.request_timeout_ms = 300_000;
    SearchOperation::new(&settings, Instant::now(), Cancellation::new()).expect("operation")
}

fn literal() -> Value {
    json!({"query":"Crohn's disease: alpha (+)","query_mode":"literal"})
}

#[tokio::test]
async fn async_helpers_refuse_literal_and_invalid_modes_before_old_peer_dispatch() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(false, false);
        let mut client = AsyncClient::connect(peer.cache.path()).await.expect("connect");
        client.handshake_registry().await.expect("old scoped Hello");
        for args in [literal(), json!({"query":"widgets","query_mode":"auto"}), json!({"query":"widgets","query_mode":4})] {
            let generic = client.call_tool("search_docs", args.clone()).await;
            let scoped = client.search(args, &operation()).await;
            assert!(matches!(generic, Err(Error::Protocol { .. })), "generic={generic:?}");
            assert!(matches!(scoped, Err(Error::Protocol { .. })), "scoped={scoped:?}");
        }
        client.search(json!({"query":"widgets"}), &operation()).await.expect("default Syntax on old scoped peer");
        client.call_tool("status", json!({})).await.expect("healthy connection");
        drop(client);
        let requests = peer.finish().await;
        assert!(matches!(requests.as_slice(), [Request::Hello {..},Request::Search {args,..},Request::CallTool {name,..}] if args.get("query_mode").is_none() && name == "status"), "{requests:?}");
    }).await.expect("watchdog");
}

#[tokio::test]
async fn blocking_helper_refuses_literal_and_invalid_modes_before_old_peer_dispatch() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(false, false);
        let cache = peer.cache.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&cache).expect("connect");
            client.handshake_registry().expect("old scoped Hello");
            for args in [literal(), json!({"query":"widgets","query_mode":true})] {
                let result = client.call_tool("search_docs", args);
                assert!(matches!(result, Err(Error::Protocol { .. })), "{result:?}");
            }
            client.call_tool("search_docs", json!({"query":"widgets"})).expect("default Syntax still supported");
        }).await.expect("blocking joined");
        let requests = peer.finish().await;
        assert!(matches!(requests.as_slice(), [Request::Hello {..}, Request::CallTool {args,..}] if args.get("query_mode").is_none()), "{requests:?}");
    }).await.expect("watchdog");
}

#[tokio::test]
async fn capable_async_helpers_forward_the_original_literal_request() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(true, false);
        let mut client = AsyncClient::connect(peer.cache.path())
            .await
            .expect("connect");
        client
            .handshake(Path::new("."))
            .await
            .expect("Hello/session");
        assert_eq!(
            client
                .call_tool("search_docs", literal())
                .await
                .expect("tool"),
            literal()
        );
        assert_eq!(
            client
                .search(literal(), &operation())
                .await
                .expect("scoped"),
            literal()
        );
        drop(client);
        let requests = peer.finish().await;
        assert_eq!(requests.len(), 4);
    })
    .await
    .expect("watchdog");
}

#[tokio::test]
async fn capable_blocking_helper_forwards_the_original_literal_request() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(true, false);
        let cache = peer.cache.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&cache).expect("connect");
            client.handshake_registry().expect("Hello");
            assert_eq!(
                client.call_tool("search_docs", literal()).expect("tool"),
                literal()
            );
        })
        .await
        .expect("blocking joined");
        assert_eq!(peer.finish().await.len(), 2);
    })
    .await
    .expect("watchdog");
}

#[tokio::test]
async fn failed_async_hello_clears_previous_literal_capability() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(true, true);
        let mut client = AsyncClient::connect(peer.cache.path())
            .await
            .expect("connect");
        client.handshake_registry().await.expect("first Hello");
        assert!(matches!(
            client.handshake_registry().await,
            Err(Error::Protocol { .. })
        ));
        let result = client.call_tool("search_docs", literal()).await;
        assert!(matches!(result, Err(Error::Protocol { .. })), "{result:?}");
        drop(client);
        assert_eq!(peer.finish().await.len(), 2, "no search after failed Hello");
    })
    .await
    .expect("watchdog");
}

#[tokio::test]
async fn failed_blocking_hello_clears_previous_literal_capability() {
    tokio::time::timeout(WATCHDOG, async {
        let peer = Peer::new(true, true);
        let cache = peer.cache.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&cache).expect("connect");
            client.handshake_registry().expect("first Hello");
            assert!(matches!(
                client.handshake_registry(),
                Err(Error::Protocol { .. })
            ));
            let result = client.call_tool("search_docs", literal());
            assert!(matches!(result, Err(Error::Protocol { .. })), "{result:?}");
        })
        .await
        .expect("blocking joined");
        assert_eq!(peer.finish().await.len(), 2, "no search after failed Hello");
    })
    .await
    .expect("watchdog");
}

#[tokio::test]
async fn cli_refuses_an_old_scoped_peer_even_with_a_valid_direct_index() {
    tokio::time::timeout(WATCHDOG, async {
        let cache = tempfile::tempdir().expect("cache");
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("guide.md"), "# Guide\n\nwidgets\n").expect("guide");
        let cache_path = cache.path().to_path_buf();
        let root_path = root.path().to_path_buf();
        let run = move |args: &[&str]| {
            std::process::Command::new(assert_cmd::cargo::cargo_bin!("docsbase"))
                .env("DOCSBASE_CACHE_DIR", &cache_path)
                .env("DOCSBASE_CONFIG_DIR", cache_path.join("config"))
                .current_dir(&root_path)
                .args(args)
                .output()
                .expect("CLI")
        };
        let index = run.clone();
        let output = tokio::task::spawn_blocking(move || index(&["index", "."]))
            .await
            .expect("index joined");
        assert!(output.status.success(), "{output:?}");
        let peer = Peer::with_cache(cache, false, false);
        let output = tokio::task::spawn_blocking(move || {
            run(&["search", "widgets", "--query-mode", "literal"])
        })
        .await
        .expect("search joined");
        assert!(
            !output.status.success(),
            "must not fall back to valid local index: {output:?}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(LITERAL_CAP) && error.contains("restart"),
            "{error}"
        );
        assert!(output.stdout.is_empty());
        let requests = peer.finish().await;
        assert!(
            matches!(
                requests.as_slice(),
                [Request::Hello { .. }, Request::RegisterSession { .. }]
            ),
            "no search/retry/downgrade: {requests:?}"
        );
    })
    .await
    .expect("watchdog");
}
