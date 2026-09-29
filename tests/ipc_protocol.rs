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
    };
    let bytes = encode(&response).expect("encode response");
    let decoded: Response = serde_json::from_slice(&bytes).expect("decode response");
    assert_eq!(decoded, response);
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
fn stats_response_round_trips_threads() {
    let response = Response::Stats {
        fd_count: 7,
        sessions: 2,
        threads: 11,
    };
    let line = encode(&response).expect("encode");
    let decoded = decode_response(&line).expect("decode");
    assert_eq!(decoded, response);
}
