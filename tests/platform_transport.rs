//! Transport seam contract (ADR-9, T34): bind/connect roundtrip, endpoint
//! serde compatible with `daemon.json`'s `"socket"` string, and cleanup.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use tempfile::TempDir;

use docsbase_memory::platform::{self, Endpoint};

#[test]
fn roundtrip_and_remove() {
    let dir = TempDir::new().expect("cache");
    let endpoint = platform::daemon_endpoint(dir.path());
    std::fs::create_dir_all(endpoint.as_path().parent().expect("state")).expect("mkdir state");

    // Async bind needs a runtime context; production always binds inside the
    // daemon runtime.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let server_endpoint = endpoint.clone();
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = platform::bind(&server_endpoint).expect("bind");
            ready_tx.send(()).expect("signal bind");
            // The liveness probe connects and drops immediately; skip it.
            loop {
                let mut stream = listener.accept().await.expect("accept");
                let mut buf = [0_u8; 16];
                let read = stream.read(&mut buf).await.expect("read");
                if read == 0 {
                    continue;
                }
                assert_eq!(&buf[..read], b"ping\n");
                stream.write_all(b"pong\n").await.expect("write");
                stream.flush().await.expect("flush");
                break;
            }
        });
    });

    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("listener bound");
    assert!(platform::exists(&endpoint), "bound endpoint must exist");
    assert!(platform::connect_probe(&endpoint), "probe must connect");

    let mut client = platform::connect_blocking(&endpoint).expect("connect");
    client.write_all(b"ping\n").expect("send");
    client.flush().expect("flush");
    let mut line = String::new();
    BufReader::new(client)
        .read_line(&mut line)
        .expect("read response");
    assert_eq!(line, "pong\n");
    server.join().expect("server thread");

    platform::remove(&endpoint).expect("remove");
    assert!(!platform::exists(&endpoint), "endpoint removed");
    assert!(
        !platform::connect_probe(&endpoint),
        "no listener after remove"
    );
}

#[test]
fn endpoint_serde_roundtrip() {
    let endpoint = Endpoint::Unix(PathBuf::from("/tmp/docsbase.sock"));
    let json = serde_json::to_string(&endpoint).expect("serialize");
    assert_eq!(json, "\"/tmp/docsbase.sock\"", "daemon.json compatible");
    let parsed: Endpoint = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, endpoint);

    // The daemon state shape keeps its `socket` string.
    let state = serde_json::json!({
        "pid": 1,
        "socket": "/tmp/docsbase.sock",
        "build_id": "docsbase 0.0.0",
        "schema_version": 1,
        "cache_root": "/tmp/cache",
    });
    let socket = state["socket"].as_str().expect("socket string");
    let parsed: Endpoint = serde_json::from_value(serde_json::json!(socket)).expect("parse");
    assert_eq!(parsed, endpoint);
}
