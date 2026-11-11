//! Transport-level cancellation tests use real HTTP and native IPC sockets.
use super::*;
use crate::{
    config::{bootstrap::DefaultsContext, schema::SettingsSpec},
    ipc::{
        framing::{Frame, FrameReader},
        protocol::{self, Request, Response},
    },
    platform,
};
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

#[tokio::test]
async fn http_disconnect_closes_ipc_and_control_capacity_survives() {
    transport_scenario(60_000, true).await;
}

#[tokio::test]
async fn original_deadline_bounds_ipc_and_stalled_http_body() {
    transport_scenario(1000, false).await;
}

#[allow(clippy::too_many_lines)] // Complete socket-level transport scenarios.
async fn transport_scenario(timeout_ms: u64, disconnect: bool) {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = platform::daemon_endpoint(cache.path());
    if let platform::Endpoint::Unix(path) = &endpoint {
        platform::fs::secure_dir(path.parent().unwrap()).unwrap();
    }
    let listener = platform::bind(&endpoint).unwrap();
    let (entered_tx, mut entered) = mpsc::channel(8);
    let (closed_tx, mut closed) = mpsc::channel(8);
    let acceptor = tokio::spawn(async move {
        loop {
            let stream = listener.accept().await.unwrap();
            let entered = entered_tx.clone();
            let closed = closed_tx.clone();
            tokio::spawn(async move {
                let (read, mut writer) = tokio::io::split(stream);
                let mut reader = FrameReader::new(BufReader::new(read));
                while let Frame::Line(line) = reader.read_frame().await.unwrap() {
                    let req = protocol::decode_request(&line).unwrap();
                    let response = match req {
                        Request::Hello { .. } => Response::Hello {
                            protocol_version: protocol::PROTOCOL_VERSION,
                            build_id: protocol::build_id(),
                            schema_version: 1,
                            capabilities: vec![
                                protocol::PANEL_READ_CAPABILITY.into(),
                                protocol::SEARCH_CONTEXT_CAPABILITY.into(),
                            ],
                        },
                        Request::BindReadOnly { .. } => Response::ToolResult { value: json!({}) },
                        Request::CallTool { name, .. } if name == "panel_info" => {
                            let policy = crate::search::settings::standalone().unwrap();
                            let s = policy.settings();
                            Response::ToolResult {
                                value: json!({"capabilities":[protocol::PANEL_READ_CAPABILITY],"request_timeout_ms":timeout_ms,"dedupe_max_candidates":s.dedupe_max_candidates,"max_request_payload_bytes":s.max_request_payload_bytes,"max_retained_payload_bytes":s.max_retained_payload_bytes,"max_hits":1000}),
                            }
                        }
                        Request::CallTool { .. } => Response::ToolResult { value: json!([]) },
                        Request::Search { .. } => {
                            entered.send(()).await.unwrap();
                            let frame = reader.read_frame().await;
                            assert!(
                                matches!(frame, Ok(Frame::Eof) | Err(_)),
                                "cancelled HTTP must close native IPC: {frame:?}"
                            );
                            closed.send(()).await.unwrap();
                            return;
                        }
                        _ => unreachable!(),
                    };
                    writer
                        .write_all(&protocol::encode(&response).unwrap())
                        .await
                        .unwrap();
                }
            });
        }
    });
    let mut settings = settings::WebSpec::defaults(&DefaultsContext::standalone());
    settings.search_connections = 1;
    settings.control_connections = 1;
    settings.queue_limit = 0;
    let daemon = daemon_client::DaemonClient::connect(cache.path().to_owned(), settings.clone())
        .await
        .unwrap();
    let http = platform::web::bind_loopback(0).await.unwrap();
    let address = http.local_addr().unwrap();
    let state = Arc::new(server::AppState {
        daemon,
        settings,
        authority: address.to_string(),
        bootstrap: Mutex::new(None),
        cookie_name: "test".into(),
        cookie_value: "secret".into(),
        initial_project: None,
        serial: AtomicU64::new(1),
    });
    let router = server::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(http, router).await.unwrap();
    });
    let mut client = platform::web::connect_loopback(address).await.unwrap();
    let body = r#"{"query":"hello"}"#;
    client.write_all(format!("POST /api/v1/projects/1/search HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nCookie: test=secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.recv())
        .await
        .unwrap()
        .unwrap();
    // A second search is refused without queue growth; control still responds.
    let busy = state
        .daemon
        .call(
            Some(1),
            "search_docs",
            json!({"query":"other"}),
            Instant::now(),
        )
        .await
        .unwrap_err();
    assert!(busy.to_string().contains("busy"));
    let status = state
        .daemon
        .call(None, "panel_projects", json!({}), Instant::now())
        .await
        .unwrap();
    assert_eq!(status, json!([]));
    if disconnect {
        drop(client);
        tokio::time::timeout(Duration::from_secs(10), closed.recv())
            .await
            .expect("HTTP abort must propagate before the 60-second search deadline")
            .unwrap();
    } else {
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_buf(&mut answer))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&answer).contains("504 Gateway Timeout"));
        tokio::time::timeout(Duration::from_secs(10), closed.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let status = state
        .daemon
        .call(None, "panel_projects", json!({}), Instant::now())
        .await
        .unwrap();
    assert_eq!(status, json!([]));
    assert!(state.serial.load(Ordering::Relaxed) > 1);
    if !disconnect {
        // A stalled body consumes the same original request deadline.
        let mut stalled = platform::web::connect_loopback(address).await.unwrap();
        stalled.write_all(format!("POST /api/v1/projects/1/search HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nCookie: test=secret\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 100\r\n\r\n{{").as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stalled.read_to_end(&mut answer))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&answer).contains("504 Gateway Timeout"));
    }
    server.abort();
    acceptor.abort();
}

#[test]
fn web_policy_validation_and_provenance() {
    use crate::config::{resolve::resolve_component, schema::SourceId};
    let invalid = settings::WebPatch {
        search_connections: Some(0),
        ..Default::default()
    };
    assert!(
        resolve_component::<settings::WebSpec>(
            &DefaultsContext::standalone(),
            [(SourceId::Cli, invalid)]
        )
        .is_err()
    );
    let policy = resolve_component::<settings::WebSpec>(
        &DefaultsContext::standalone(),
        [(
            SourceId::Cli,
            settings::WebPatch {
                port: Some(8080),
                ..Default::default()
            },
        )],
    )
    .unwrap();
    assert_eq!(policy.settings().port, 8080);
    assert_eq!(
        policy.provenance().get("web.port").unwrap().source,
        SourceId::Cli
    );
}

#[tokio::test]
async fn missing_panel_capability_refuses_before_binding() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = platform::daemon_endpoint(cache.path());
    if let platform::Endpoint::Unix(path) = &endpoint {
        platform::fs::secure_dir(path.parent().unwrap()).unwrap();
    }
    let listener = platform::bind(&endpoint).unwrap();
    let peer = tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = FrameReader::new(BufReader::new(read));
        assert!(matches!(reader.read_frame().await.unwrap(), Frame::Line(_)));
        write
            .write_all(
                &protocol::encode(&Response::Hello {
                    protocol_version: protocol::PROTOCOL_VERSION,
                    build_id: protocol::build_id(),
                    schema_version: 1,
                    capabilities: vec![],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(reader.read_frame().await.unwrap(), Frame::Eof));
    });
    let policy = settings::WebSpec::defaults(&DefaultsContext::standalone());
    let outcome = daemon_client::DaemonClient::connect(cache.path().to_owned(), policy).await;
    assert!(matches!(outcome, Err(crate::error::Error::Protocol { .. })));
    peer.await.unwrap();
}

#[test]
fn safe_config_excludes_entire_provider_records_and_marks_unknown_origins() {
    let mut config = crate::config::Config::default();
    let mut record = crate::hybrid::config::ProviderRecord::new(true);
    record.params.insert(
        "api_key".into(),
        toml::Value::String("secret-test-sentinel".into()),
    );
    record.params.insert(
        "endpoint".into(),
        toml::Value::String("https://user:password@example.test/".into()),
    );
    config.providers.insert("openrouter", record);
    let view = crate::ipc::panel::ConfigView::from_config(&config);
    let wire = serde_json::to_string(&view).unwrap();
    assert!(!wire.contains("secret-test-sentinel"));
    assert!(!wire.contains("password"));
    assert!(!wire.contains("api_key"));
    assert!(view.fields.iter().all(|field| field.source.is_none()));
}
