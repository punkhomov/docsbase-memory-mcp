use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::config::Config;
use docsbase_memory::config::resolve;
use docsbase_memory::config::schema::SourceId;
use docsbase_memory::config::source::SourceLayer;
use docsbase_memory::config::view;
use docsbase_memory::hybrid::config::ProviderRecord;
use serde_json::{Value, json};

fn row<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == key)
        .unwrap()
}

#[test]
fn effective_view_matches_real_v1_resolution_and_field_origins() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let project = root.path().join("project");
    fs::create_dir(&global).unwrap();
    fs::create_dir(&project).unwrap();
    fs::write(
        global.join("config.toml"),
        "max_file_size=2048\nhybrid=true\n[search]\nrequest_timeout_ms=2468\n",
    )
    .unwrap();
    fs::write(
        project.join(".docsbase.toml"),
        "max_docs_per_project=123\n[hybrid.reranker]\nwindow=20\n",
    )
    .unwrap();
    let resolved = resolve::load_from(&global, Some(&project)).unwrap();
    let output = serde_json::to_value(view::load_from(&global, Some(&project)).unwrap()).unwrap();
    assert_eq!(output["view_version"], 1);
    assert_eq!(output["basis"], "source_resolution");
    assert_eq!(output["coverage"], "file_settings");
    assert_eq!(output["live_runtime_observed"], false);
    assert_eq!(output["provisioning_checked"], false);
    let rows = output["fields"].as_array().unwrap();
    let keys: Vec<_> = rows.iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        resolved
            .provenance()
            .iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        row(&output, "max_file_size")["value"],
        resolved.settings().max_file_size
    );
    assert_eq!(
        row(&output, "max_file_size")["source"],
        json!({"kind":"global","value":global.join("config.toml")})
    );
    assert_eq!(
        row(&output, "max_docs_per_project")["source"],
        json!({"kind":"project","value":project.join(".docsbase.toml")})
    );
    let timeout = row(&output, "search.request_timeout_ms");
    assert_eq!(timeout["value"], 2468);
    assert_eq!(timeout["unit"], "milliseconds");
    assert_eq!(timeout["scope"], "daemon");
    // The project v1 table still replaces the global hybrid value.
    assert_eq!(row(&output, "hybrid.embedding.model")["value"], Value::Null);
    assert_eq!(row(&output, "hybrid.reranker.window")["value"], 20);
    assert_eq!(
        row(&output, "hybrid.reranker.provider")["capability"]["compiled"],
        cfg!(feature = "hybrid-remote")
    );
}

#[test]
fn view_defaults_and_v2_budget_do_not_invent_provenance() {
    let missing = tempfile::tempdir().unwrap();
    let output = serde_json::to_value(view::load_from(missing.path(), None).unwrap()).unwrap();
    assert!(
        output["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["source"] == json!({"kind":"defaults"}))
    );
    let source = SourceLayer::parse(
        SourceId::Global("global.toml".into()),
        "config_version=2\n[vector_cache]\nmax_bytes=4096\n",
    )
    .unwrap();
    let preview = resolve::preview_sources(&source, None).unwrap();
    let output = serde_json::to_value(view::from_resolved(&preview.effective).unwrap()).unwrap();
    assert_eq!(output["basis"], "resolved_snapshot");
    assert_eq!(row(&output, "vector_cache.max_bytes")["value"], 4096);
    assert_eq!(row(&output, "vector_cache.max_bytes")["scope"], "daemon");
    assert!(
        !output["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "hybrid.cache.max_bytes")
    );
}

#[test]
fn effective_fastembed_embedding_limit_is_validated_and_global() {
    let global = tempfile::tempdir().unwrap();
    let path = global.path().join("config.toml");
    for invalid in ["2", "513", "\"128\""] {
        fs::write(
            &path,
            format!("config_version=2\n[providers.fastembed]\nembedding_max_length={invalid}\n"),
        )
        .unwrap();
        assert!(resolve::global_from(global.path()).is_err(), "{invalid}");
    }
    fs::write(
        &path,
        "config_version=2\n[providers.fastembed]\nembedding_max_length=128\n",
    )
    .unwrap();
    let view = serde_json::to_value(view::load_from(global.path(), None).unwrap()).unwrap();
    let limit = row(&view, "providers.fastembed.embedding_max_length");
    assert_eq!(limit["value"], 128);
    assert_eq!(limit["source"], json!({"kind":"global","value":path}));
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join(".docsbase.toml"),
        "config_version=2\n[providers.fastembed]\nembedding_max_length=64\n",
    )
    .unwrap();
    assert!(resolve::load_from(global.path(), Some(project.path())).is_err());
}

#[test]
fn watcher_effective_view_attributes_website_override_to_cli_profile() {
    use docsbase_memory::daemon::launch::LaunchSnapshot;
    use docsbase_memory::daemon::settings::{DaemonPatch, DaemonProfile};

    let global = tempfile::tempdir().unwrap();
    let config = global.path().join("config.toml");
    fs::write(&config, "config_version = 2\n[watch]\nenabled = true\n").unwrap();
    let snapshot = LaunchSnapshot::capture_from(global.path());
    let view = |profile| {
        let resolved = snapshot
            .resolve(DaemonPatch {
                profile: Some(profile),
                ..DaemonPatch::default()
            })
            .unwrap();
        serde_json::to_value(view::from_resolved(&resolved).unwrap()).unwrap()
    };
    let website = view(DaemonProfile::Website {
        project_root: global.path().to_owned(),
    });
    let watcher = row(&website, "watch.enabled");
    assert_eq!(watcher["value"], false);
    assert_eq!(watcher["source"], json!({"kind":"global","value":config}));
    assert_eq!(
        watcher["derived_from"],
        json!({"key":"daemon.profile","source":{"kind":"cli"}})
    );

    let shared = view(DaemonProfile::Shared);
    let watcher = row(&shared, "watch.enabled");
    assert_eq!(watcher["value"], true);
    assert!(watcher.get("derived_from").is_none());
}

#[test]
fn local_provisioning_policy_reports_global_overrides_and_rejects_project_scope() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("config.toml");
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        &global,
        "config_version = 2
[provisioning.models]
max_file_bytes = 12345
lock_attempts = 1
         [provisioning.runtime]
response_timeout_ms = 15000
",
    )
    .unwrap();
    let output =
        serde_json::to_value(view::load_from(root.path(), Some(&project)).unwrap()).unwrap();
    let file_bytes = row(&output, "provisioning.models.max_file_bytes");
    assert_eq!(file_bytes["value"], 12345);
    assert_eq!(file_bytes["scope"], "machine");
    assert_eq!(
        file_bytes["source"],
        json!({"kind":"global","value":global})
    );
    assert_eq!(
        row(&output, "provisioning.models.lock_attempts")["value"],
        1
    );
    assert_eq!(
        row(&output, "provisioning.runtime.response_timeout_ms")["value"],
        15000,
    );
    assert_eq!(
        row(&output, "provisioning.runtime.body_timeout_ms")["source"],
        json!({"kind":"defaults"}),
    );

    fs::write(
        project.join(".docsbase.toml"),
        "config_version = 2
[provisioning.models]
lock_attempts = 2
",
    )
    .unwrap();
    let error = resolve::load_from(root.path(), Some(&project)).unwrap_err();
    assert!(
        error.to_string().contains("provisioning is global-only"),
        "{error}"
    );
}

#[test]
fn provider_projection_never_serializes_arbitrary_record_parameters() {
    use docsbase_memory::hybrid::provider::providers;
    let mut record = ProviderRecord::new(true);
    record
        .params
        .insert("api_key".into(), "credential-sentinel".into());
    let provider = providers()
        .iter()
        .find(|p| p.name() == "openrouter")
        .unwrap();
    let output = provider.configuration_view(&record).unwrap();
    let text = serde_json::to_string(&output).unwrap();
    assert!(!text.contains("credential-sentinel"));
    assert_eq!(output["enabled"], true);
    assert_eq!(output["unexposed_parameters"], true);
    assert_eq!(output["parameters"].as_object().unwrap().len(), 0);
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn walk(root: &Path, path: &Path, out: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        let mut children = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            let rel = child.strip_prefix(root).unwrap().to_owned();
            if child.is_dir() {
                out.push((rel, None));
                walk(root, &child, out);
            } else {
                out.push((rel, Some(fs::read(child).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}
fn command(root: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("docsbase");
    cmd.current_dir(root)
        .env("DOCSBASE_CONFIG_DIR", root.join("config"))
        .env("DOCSBASE_CACHE_DIR", root.join("cache"))
        .env("DOCSBASE_DATA_DIR", root.join("data"))
        .env("DOCSBASE_DAEMON_EXE", root.join("must-not-start"))
        .env("OPENROUTER_API_KEY", "credential-sentinel")
        .env("ORT_DYLIB_PATH", root.join("must-not-load"));
    cmd
}

#[test]
fn effective_cli_is_readonly_and_bare_config_stays_compatible() {
    let root = tempfile::tempdir().unwrap();
    let before = snapshot(root.path());
    let output = command(root.path())
        .args(["config", "effective", "--format=json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let view: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(view["view_version"], 1);
    assert_eq!(snapshot(root.path()), before);
    let old = command(root.path())
        .arg("config")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&old).unwrap(),
        docsbase_memory::cli::config_payload(&Config::default())
    );
    command(root.path())
        .args(["config", "effective", "--format=yaml"])
        .assert()
        .failure();
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn effective_cli_reports_captured_bootstrap_values_and_sources() {
    let root = tempfile::tempdir().unwrap();
    let before = snapshot(root.path());
    let output = command(root.path())
        .args(["config", "effective", "--format=json"])
        .env("TOKIO_WORKER_THREADS", "2")
        .env("RAYON_NUM_THREADS", "3")
        .env("TOKENIZERS_PARALLELISM", "off")
        .env("DOCSBASE_IPC_DEBUG", "1")
        .env("DOCSBASE_WATCH_DEBUG", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let view: Value = serde_json::from_slice(&output).unwrap();
    let bootstrap = &view["bootstrap"];
    for (key, variable, value) in [
        (
            "cache_dir",
            "DOCSBASE_CACHE_DIR",
            json!(root.path().join("cache")),
        ),
        (
            "config_dir",
            "DOCSBASE_CONFIG_DIR",
            json!(root.path().join("config")),
        ),
        (
            "data_dir",
            "DOCSBASE_DATA_DIR",
            json!(root.path().join("data")),
        ),
        (
            "daemon_exe",
            "DOCSBASE_DAEMON_EXE",
            json!(root.path().join("must-not-start")),
        ),
        (
            "ort_dylib_path",
            "ORT_DYLIB_PATH",
            json!(root.path().join("must-not-load")),
        ),
        ("tokio_workers", "TOKIO_WORKER_THREADS", json!(2)),
        ("rayon_threads", "RAYON_NUM_THREADS", json!(3)),
        (
            "tokenizers_parallelism",
            "TOKENIZERS_PARALLELISM",
            json!(false),
        ),
        ("ipc_debug", "DOCSBASE_IPC_DEBUG", json!(true)),
        ("watch_debug", "DOCSBASE_WATCH_DEBUG", json!(true)),
    ] {
        assert_eq!(
            bootstrap[key],
            json!({
                "value": value,
                "source": {"kind": "environment", "value": variable},
            }),
            "{key}"
        );
    }
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("credential-sentinel")
    );
    assert_eq!(snapshot(root.path()), before);

    fs::create_dir(root.path().join("config")).unwrap();
    let global = root.path().join("config/config.toml");
    fs::write(
        &global,
        "config_version=2\n[providers.fastembed]\nenabled=true\nruntime_path='not-loaded-by-the-view'\n",
    ).unwrap();
    let output = command(root.path())
        .args(["config", "effective", "--format=json"])
        .env_remove("ORT_DYLIB_PATH")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let from_file: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        from_file["bootstrap"]["ort_dylib_path"],
        json!({
            "value": "not-loaded-by-the-view",
            "source": { "kind": "global", "value": global },
        })
    );
    assert_eq!(from_file["provisioning_checked"], false);

    command(root.path())
        .env("TOKIO_WORKER_THREADS", "invalid")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn effective_cli_does_not_probe_models_databases_or_credentials() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("config")).unwrap();
    fs::create_dir(root.path().join("cache")).unwrap();
    fs::write(
        root.path()
            .join("cache")
            .join(docsbase_memory::store::DB_FILE),
        b"not a database",
    )
    .unwrap();
    fs::write(root.path().join("config/config.toml"), "hybrid=true\n[providers.fastembed]\nenabled=true\nruntime_path='missing-runtime'\n[providers.openrouter]\nenabled=true\n").unwrap();
    let before = snapshot(root.path());
    let output = command(root.path())
        .env_remove("OPENROUTER_API_KEY")
        .args(["config", "effective"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let providers = row(&parsed, "providers")["value"].as_array().unwrap();
    assert_eq!(providers[0]["provider"], "fastembed");
    assert_eq!(
        providers[0]["configuration"]["parameters"]["runtime_path"],
        "missing-runtime"
    );
    assert_eq!(parsed["provisioning_checked"], false);
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("credential-sentinel")
    );
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn effective_cli_errors_do_not_echo_raw_sources() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("config")).unwrap();
    for text in [
        "api_key='credential-sentinel'",
        "max_file_size='credential-sentinel'",
        "config_version=3",
        "[hybrid.embedding]\nprovider='credential-sentinel'\n",
    ] {
        fs::write(root.path().join("config/config.toml"), text).unwrap();
        let before = snapshot(root.path());
        let out = command(root.path())
            .args(["config", "effective"])
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(!String::from_utf8_lossy(&out.stderr).contains("credential-sentinel"));
        assert_eq!(out.stdout, [] as [u8; 0]);
        assert_eq!(snapshot(root.path()), before);
    }
}
