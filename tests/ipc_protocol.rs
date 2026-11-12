use std::path::PathBuf;

use docsbase_memory::error::Error;
use docsbase_memory::ipc::protocol::{
    PROTOCOL_VERSION, Request, Response, decode_request, decode_response, encode,
    ensure_protocol_version,
};
use serde_json::json;

#[test]
fn roundtrip() {
    let requests = [
        Request::Hello {
            protocol_version: PROTOCOL_VERSION,
            build_id: "test-build".to_owned(),
            client: "codex".to_owned(),
        },
        Request::RegisterSession {
            pid: 42,
            cwd: PathBuf::from("/work/project"),
        },
        Request::CallTool {
            name: "search_docs".to_owned(),
            args: json!({"query": "assessment_plan_id", "limit": 5}),
        },
        Request::StopDaemon,
    ];
    for request in requests {
        let bytes = encode(&request).expect("encode");
        assert_eq!(bytes.last(), Some(&b'\n'), "NDJSON line must end with \\n");
        assert_eq!(
            bytes.split(|b| *b == b'\n').count(),
            2,
            "exactly one newline per message"
        );
        let decoded = decode_request(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    let response = Response::Hello {
        protocol_version: PROTOCOL_VERSION,
        build_id: "test-build".to_owned(),
        schema_version: 1,
        capabilities: Vec::new(),
    };
    let bytes = encode(&response).expect("encode response");
    let decoded: Response = serde_json::from_slice(&bytes).expect("decode response");
    assert_eq!(decoded, response);
}

#[test]
fn scoped_search_roundtrip_keeps_the_explicit_budget() {
    let wire = json!({
        "method": "Search",
        "params": {
            "args": {"query":"widgets", "explain":true},
            "context": {"remaining_ms":5000, "elapsed_ms":11, "queue_ms":3},
        },
    });
    let request = decode_request(&serde_json::to_vec(&wire).expect("wire bytes"))
        .expect("scoped search request");
    let encoded = encode(&request).expect("encode scoped search");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&encoded).expect("wire value"),
        wire
    );
}

#[test]
fn scoped_budget_is_validated_before_dispatch() {
    for context in [
        json!({"remaining_ms":0, "elapsed_ms":11, "queue_ms":3}),
        json!({"remaining_ms":5000, "elapsed_ms":2, "queue_ms":3}),
    ] {
        let wire = json!({"method":"Search", "params":{"args":{}, "context":context}});
        let error = decode_request(&serde_json::to_vec(&wire).expect("wire"))
            .expect_err("invalid scoped budget");
        assert!(matches!(error, Error::Admission { .. }), "{error:?}");
    }
}

#[test]
fn scoped_context_has_no_hidden_budget_defaults() {
    for missing in ["remaining_ms", "elapsed_ms", "queue_ms"] {
        let mut context = json!({"remaining_ms":5000, "elapsed_ms":11, "queue_ms":3});
        context.as_object_mut().expect("context").remove(missing);
        let wire = json!({"method":"Search", "params":{"args":{}, "context":context}});
        let error = decode_request(&serde_json::to_vec(&wire).expect("wire"))
            .expect_err("missing budget field");
        assert!(matches!(error, Error::Protocol { .. }));
        assert!(error.inner_message().contains(missing), "{error}");
    }
}

#[test]
fn legacy_hello_has_no_scoped_capability() {
    let reply = decode_response(br#"{"kind":"Hello","data":{"protocol_version":1,"build_id":"old-daemon","schema_version":2}}"#)
        .expect("legacy hello");
    match reply {
        Response::Hello { capabilities, .. } => {
            assert_eq!(capabilities, [] as [std::string::String; 0]);
        }
        other => panic!("expected hello, got {other:?}"),
    }
}

#[test]
fn unknown_tool_rejected() {
    let line = br#"{"method":"CallTool","params":{"name":"exec_shell","args":{}}}"#;
    let err = decode_request(line).expect_err("must reject");
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert_eq!(err.mcp_code(), -32011);
    assert!(err.to_string().contains("exec_shell"), "{err}");
}

#[test]
fn version_checked() {
    ensure_protocol_version(PROTOCOL_VERSION).expect("same version");
    let err = ensure_protocol_version(PROTOCOL_VERSION + 1).expect_err("must reject");
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert_eq!(err.mcp_code(), -32011);
}

#[test]
fn malformed_json_is_protocol_error() {
    let err = decode_request(b"{not json}").expect_err("must reject");
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
}

#[test]
fn tool_result_round_trips_payloads() {
    let response = Response::ToolResult {
        value: serde_json::json!({ "threads": 11, "fd_count": 7 }),
    };
    let line = encode(&response).expect("encode");
    let decoded = decode_response(&line).expect("decode");
    assert_eq!(decoded, response);
}
