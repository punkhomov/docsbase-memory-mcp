//! Rust-owned wire fixture and native Node/IPC acceptance (#79, stage C).
use docsbase_memory::{
    error::Error,
    ipc::{
        panel::SearchHit,
        protocol::{self, Request, Response, SearchCallContext},
    },
    store::migrations::SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{Value, json};

// rig enables serde_json/preserve_order in cli-full. Wire object key order is
// immaterial; normalize both map implementations without losing numeric tokens.
fn canonical(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect(),
            )
        }
        other => other,
    }
}
fn frame(value: &impl Serialize) -> String {
    let actual: Value = serde_json::from_slice(&protocol::encode(value).unwrap()).unwrap();
    format!("{}\n", serde_json::to_string(&canonical(actual)).unwrap())
}
fn fixture() -> Value {
    let build = protocol::build_id();
    let caps = [
        protocol::SEARCH_CONTEXT_CAPABILITY,
        protocol::PANEL_READ_CAPABILITY,
        protocol::LITERAL_QUERY_CAPABILITY,
    ];
    let hit = SearchHit {
        chunk_id: u64::MAX,
        path: "guides/привет.md".into(),
        heading_path: "Guide > Привет".into(),
        lines: [1, 3],
        score: 1.25,
        snippet: Some("widgets stay local".into()),
        snippet_truncated: Some(false),
    };
    let errors = [
        Error::Admission {
            message: "admission".into(),
        },
        Error::Protocol {
            message: "protocol".into(),
        },
        Error::Project {
            message: "project".into(),
            instruction: Some("index explicitly".into()),
        },
        Error::Index {
            path: None,
            message: "index".into(),
        },
        Error::Query {
            message: "query".into(),
        },
        Error::internal("internal"),
    ];
    json!({
        "protocolVersion": protocol::PROTOCOL_VERSION, "schemaVersion": SCHEMA_VERSION,
        "buildId": build, "maxFrameBytes": protocol::MAX_FRAME_BYTES, "capabilities": caps,
        "requests": {
            "hello": frame(&Request::Hello { protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(), client: "docsbase-node".into() }),
            "bind": frame(&Request::BindReadOnly { pid: 1234, project_id: Some(7) }),
            "search": frame(&Request::Search { args: json!({"query":"widgets", "query_mode":"literal", "snippet":true}),
                context: SearchCallContext { remaining_ms: 1000, elapsed_ms: 20, queue_ms: 10 } }),
            "status": frame(&Request::CallTool { name: "status".into(), args: json!({}) }),
        },
        "responses": {
            "hello": frame(&Response::Hello { protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(), schema_version: SCHEMA_VERSION,
                capabilities: caps.iter().map(|s| (*s).to_owned()).collect() }),
            "bind": frame(&Response::ToolResult { value: json!({"session_id": 1}) }),
            "hits": frame(&Response::ToolResult { value: json!([hit]) }),
            "explained": frame(&Response::ToolResult { value: json!({"hits":[hit],"meta":{"index_generation":3}}) }),
        },
        "errors": errors.iter().map(|error| frame(&Response::Error {
            code: error.mcp_code(), message: error.inner_message(),
            instruction: error.instruction().map(str::to_owned),
        })).collect::<Vec<_>>(),
    })
}
#[test]
fn rust_wire_fixture_matches_node_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("packages/client/test/fixtures/protocol.json");
    let expected = format!(
        "{}\n",
        serde_json::to_string_pretty(&canonical(fixture())).unwrap()
    );
    if std::env::var_os("DOCSBASE_GENERATE_IPC_FIXTURE").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        expected,
        "run DOCSBASE_GENERATE_IPC_FIXTURE=1 cargo test --test node_client rust_wire_fixture_matches_node_contract"
    );
}

// cli-full/Web already requires Node + prepared workspace assets. Lean Cargo
// keeps its Rust-only fixture gate without requiring frontend tooling.
#[cfg(feature = "web")]
#[test]
fn official_node_client_searches_real_daemon_and_reconnects() {
    use docsbase_memory::platform::{self, Endpoint};
    use std::{path::PathBuf, time::Duration};
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = docsbase_memory::daemon::lifecycle::stop_daemon(&self.0);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let _cleanup = Cleanup(cache.clone());
    let endpoint = match platform::daemon_endpoint(&cache) {
        Endpoint::Unix(path) => path.to_str().unwrap().to_owned(),
        Endpoint::Pipe(name) => format!(r"\\.\pipe\{name}"),
    };
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/client/test/native.mjs");
    let mut node = assert_cmd::Command::new("node");
    node.arg(script)
        .arg(env!("CARGO_BIN_EXE_docsbase"))
        .arg(root.path())
        .arg(endpoint)
        .arg(protocol::build_id())
        .arg(SCHEMA_VERSION.to_string())
        .env("DOCSBASE_CACHE_DIR", &cache)
        .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
        .env("DOCSBASE_DATA_DIR", root.path().join("data"))
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ORT_DYLIB_PATH")
        .timeout(Duration::from_secs(240));
    let output = node.assert().success().get_output().clone();
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["nativeIpc"], true);
    assert_eq!(summary["searchesAccepted"], 11);
    assert_eq!(summary["overloaded"], 1);
    assert_eq!(summary["reconnected"], true);
    assert_eq!(summary["providerArtifacts"], false);
}
