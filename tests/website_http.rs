//! Native HTTP -> official SDK -> IPC -> fixed Website daemon acceptance.
#![cfg(feature = "web")]

use docsbase_memory::ipc::protocol;
use docsbase_memory::store::migrations::SCHEMA_VERSION;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn website_http_search_updates_cancellation_and_reconnect_use_real_daemon() {
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = docsbase_memory::daemon::lifecycle::stop_daemon(&self.0);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let _cleanup = Cleanup(cache.clone());
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/website-search/test/native.mjs");
    let output = assert_cmd::Command::new("node")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_docsbase"))
        .arg(root.path())
        .arg(protocol::build_id())
        .arg(SCHEMA_VERSION.to_string())
        .env("DOCSBASE_CACHE_DIR", &cache)
        .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
        .env("DOCSBASE_DATA_DIR", root.path().join("data"))
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ORT_DYLIB_PATH")
        .timeout(Duration::from_secs(240))
        .assert()
        .success()
        .get_output()
        .clone();
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in [
        "httpNativeIpc",
        "explicitUpdates",
        "partialRecovery",
        "boundedAdmission",
        "cancelledRequests",
        "reconnected",
        "watchersOff",
    ] {
        assert_eq!(summary[field], true, "{field}: {summary}");
    }
}
