use std::process::Command;

#[test]
fn migration_cli_observes_missing_sources_without_creating_state() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args([
            "config",
            "migrate",
            "--to",
            "2",
            "--format=json",
            "--accept-suggested-vector-budget",
        ])
        .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
        .env("DOCSBASE_CACHE_DIR", root.path().join("cache"))
        .env("DOCSBASE_DATA_DIR", root.path().join("data"))
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["sources"][0]["state"]["kind"], "missing");
    assert_eq!(
        view["plan"]["expected"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["resource"]["kind"] == "source")
            .unwrap()["state"]["kind"],
        "missing"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

use docsbase_memory::config::{
    migrate::VectorBudgetDecision,
    observe::{self, PlanningLimits},
};
use docsbase_memory::daemon::registry::ensure_project;
use docsbase_memory::store::{Db, settings};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut files = BTreeMap::new();
    if root.is_dir() {
        files.insert(root.to_owned(), None);
        for entry in fs::read_dir(root).unwrap() {
            files.extend(snapshot(&entry.unwrap().path()));
        }
    } else if root.is_file() {
        files.insert(root.to_owned(), Some(fs::read(root).unwrap()));
    }
    files
}
fn plan(root: &Path, decision: Option<VectorBudgetDecision>, limits: PlanningLimits) -> Value {
    let before = snapshot(root);
    let result = observe::migration_plan(
        &root.join("config"),
        &root.join("cache"),
        &settings::standalone().unwrap(),
        limits,
        decision,
    )
    .unwrap();
    assert_eq!(before, snapshot(root), "planning must not write anything");
    serde_json::from_str(&result.to_json(limits.max_output_bytes).unwrap()).unwrap()
}
fn accepted(root: &Path) -> Value {
    plan(
        root,
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
        PlanningLimits::standard(),
    )
}
fn write_global(root: &Path, source: impl AsRef<[u8]>) {
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/config.toml"), source).unwrap();
}
fn register(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    let mut db = Db::open(&root.join("cache")).unwrap();
    ensure_project(&mut db, &path).unwrap().canonical_root
}
fn has_blocker(value: &Value, code: &str) -> bool {
    value["plan"]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["code"] == code)
}

#[test]
fn actual_registry_includes_unindexed_and_missing_projects_but_not_cwd() {
    let root = tempfile::tempdir().unwrap();
    write_global(root.path(), "[hybrid.cache]\nmax_bytes=1000\n");
    let first = register(root.path(), "first");
    fs::write(
        first.join(".docsbase.toml"),
        "[hybrid.cache]\nmax_bytes=4000\n",
    )
    .unwrap();
    let second = register(root.path(), "removed");
    fs::remove_dir(&second).unwrap();
    let view = accepted(root.path());
    assert_eq!(view["registry"]["state"]["kind"], "ready");
    assert_eq!(view["registry_stable"], true);
    assert_eq!(view["sources"].as_array().unwrap().len(), 3);
    assert_eq!(
        view["sources"][2]["source"]["value"],
        json!(second.join(".docsbase.toml"))
    );
    assert_eq!(view["sources"][2]["state"]["kind"], "missing");
    assert_eq!(view["summary"]["suggested_vector_max_bytes"], 4000);
    assert_eq!(view["summary"]["rewrites"].as_array().unwrap().len(), 2);
    assert_eq!(view["summary"]["budgets"][0]["previous"], 1000);
    assert_eq!(view["summary"]["budgets"][0]["proposed"], 4000);
    assert!(has_blocker(&view, "strict_store_adoption_unavailable"));
    assert!(has_blocker(&view, "executor_unavailable"));
    assert!(!has_blocker(&view, "source_observation_required"));
    assert_eq!(view["atomic_snapshot"], false);
    assert_eq!(view["runtime_activation_available"], false);
    assert_eq!(view, accepted(root.path()));
}

#[test]
fn identity_binds_missing_empty_unchanged_sources_context_and_explicit_choice() {
    let root = tempfile::tempdir().unwrap();
    let missing = accepted(root.path());
    write_global(root.path(), "");
    let empty = accepted(root.path());
    assert_ne!(missing["observation_id"], empty["observation_id"]);
    assert_ne!(missing["plan"]["plan_id"], empty["plan"]["plan_id"]);
    assert_eq!(empty["sources"][0]["state"]["kind"], "loaded");
    let no_choice = plan(root.path(), None, PlanningLimits::standard());
    assert_ne!(empty["observation_id"], no_choice["observation_id"]);
    assert!(has_blocker(&no_choice, "vector_budget_decision_required"));
    assert_eq!(no_choice["plan"]["actions"], json!([]));
    let project = register(root.path(), "project");
    fs::write(project.join(".docsbase.toml"), "config_version = 2\n").unwrap();
    let unchanged = accepted(root.path());
    assert_eq!(
        unchanged["summary"]["rewrites"].as_array().unwrap().len(),
        1
    );
    // An unchanged input is a precondition too, not merely files to be rewritten.
    assert!(
        unchanged["plan"]["expected"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["resource"]["value"]["kind"] == "project")
    );
    fs::write(
        project.join(".docsbase.toml"),
        "config_version = 2\nmax_docs_per_project = 200\n",
    )
    .unwrap();
    assert_ne!(
        unchanged["observation_id"],
        accepted(root.path())["observation_id"]
    );
    let other = tempfile::tempdir().unwrap();
    write_global(other.path(), "");
    assert_ne!(
        empty["observation_id"],
        accepted(other.path())["observation_id"]
    );
    let other_cache = root.path().join("other-cache");
    let altered = observe::migration_plan(
        &root.path().join("config"),
        &other_cache,
        &settings::standalone().unwrap(),
        PlanningLimits::standard(),
        Some(VectorBudgetDecision::AcceptSuggestedMaximum),
    )
    .unwrap();
    let altered: Value = serde_json::from_str(&altered.to_json(8 * 1024 * 1024).unwrap()).unwrap();
    assert_ne!(unchanged["observation_id"], altered["observation_id"]);
}

#[test]
fn secrets_in_comments_values_and_invalid_sources_never_reach_output() {
    let root = tempfile::tempdir().unwrap();
    let canary = "SENSITIVE_CANARY_834712";
    write_global(
        root.path(),
        format!(
            "# {canary}\nignores=['{canary}']\n[hybrid.embedding]\nprovider='openrouter'\nmodel='https://user:{canary}@example.test/'\ndimensions=1536\n[providers.openrouter]\nenabled=true\n"
        ),
    );
    let output = accepted(root.path());
    assert!(!output.to_string().contains(canary));
    write_global(
        root.path(),
        format!("[providers.openrouter]\nenabled=true\napi_key='{canary}'\n"),
    );
    let error = observe::migration_plan(
        &root.path().join("config"),
        &root.path().join("cache"),
        &settings::standalone().unwrap(),
        PlanningLimits::standard(),
        None,
    )
    .unwrap_err();
    assert!(!format!("{error} {error:?}").contains(canary));
    for text in [
        format!("secret = '{canary}'"),
        format!("[malformed {canary}"),
    ] {
        write_global(root.path(), text);
        let output = accepted(root.path());
        assert!(!output.to_string().contains(canary));
        assert_eq!(
            output["sources"][0]["state"],
            json!({"kind":"unavailable", "value":"invalid"})
        );
        assert_eq!(output["plan"]["actions"], json!([]));
    }
    write_global(root.path(), [0xff, 0xfe]);
    assert_eq!(
        accepted(root.path())["sources"][0]["state"]["value"],
        "invalid"
    );
}

#[test]
fn unavailable_registry_never_uses_global_only_as_complete_inventory() {
    let root = tempfile::tempdir().unwrap();
    register(root.path(), "project");
    let db = Db::open(&root.path().join("cache")).unwrap();
    let view = accepted(root.path());
    assert_eq!(view["registry"]["state"]["kind"], "unavailable");
    assert_eq!(view["registry_stable"], false);
    assert_eq!(view["summary"], Value::Null);
    assert_eq!(view["plan"]["actions"], json!([]));
    assert!(has_blocker(&view, "observation_unavailable"));
    drop(db);
}

#[test]
fn limits_fail_closed_for_source_aggregate_registry_and_output() {
    let root = tempfile::tempdir().unwrap();
    write_global(root.path(), vec![b' '; 1_048_577]);
    assert_eq!(
        accepted(root.path())["sources"][0]["state"]["value"],
        "limit_exceeded"
    );
    write_global(root.path(), "auto_index = true\n");
    let project = register(root.path(), "project");
    fs::write(project.join(".docsbase.toml"), "auto_index = false\n").unwrap();
    let mut limits = PlanningLimits::standard();
    limits.max_source_bytes = 20;
    let value = plan(root.path(), None, limits);
    assert_eq!(value["sources"][1]["state"]["value"], "limit_exceeded");
    assert_eq!(value["plan"]["actions"], json!([]));
    limits = PlanningLimits::standard();
    limits.registry.max_projects = 0;
    assert_eq!(
        plan(root.path(), None, limits)["registry"]["state"]["value"],
        "limit_exceeded"
    );
    limits = PlanningLimits::standard();
    limits.max_output_bytes = 64;
    assert!(
        observe::migration_plan(
            &root.path().join("config"),
            &root.path().join("cache"),
            &settings::standalone().unwrap(),
            limits,
            None
        )
        .is_err()
    );
    limits = PlanningLimits::standard();
    limits.registry.max_metadata_bytes = 1;
    assert!(
        observe::migration_plan(
            &root.path().join("config"),
            &root.path().join("cache"),
            &settings::standalone().unwrap(),
            limits,
            None
        )
        .is_err()
    );
}

#[test]
fn nonregular_global_blocks_proposals() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("config/config.toml")).unwrap();
    let value = accepted(root.path());
    assert_eq!(value["sources"][0]["state"]["value"], "non_regular");
    assert!(has_blocker(&value, "observation_unavailable"));
    assert_eq!(value["plan"]["actions"], json!([]));
}

#[test]
fn cli_reads_v2_without_operational_loader_and_rejects_ambiguous_budget_choices() {
    let root = tempfile::tempdir().unwrap();
    write_global(
        root.path(),
        "config_version=2\n[vector_cache]\nmax_bytes=4096\n",
    );
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_docsbase"))
            .current_dir(root.path())
            .env("DOCSBASE_CONFIG_DIR", "config")
            .env("DOCSBASE_CACHE_DIR", "cache")
            .env("DOCSBASE_DATA_DIR", "data")
            .args(args)
            .output()
            .unwrap()
    };
    let output = invoke(&[
        "config",
        "migrate",
        "--to",
        "2",
        "--vector-max-bytes",
        "8192",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["summary"]["selected_vector_max_bytes"], 8192);
    assert_eq!(value["summary"]["budgets"][0]["previous"], 4096);
    for args in [
        vec!["config", "migrate", "--to", "3"],
        vec!["config", "migrate"],
        vec!["config", "migrate", "--to", "2", "--vector-max-bytes", "0"],
        vec![
            "config",
            "migrate",
            "--to",
            "2",
            "--vector-max-bytes",
            "100",
            "--accept-suggested-vector-budget",
        ],
    ] {
        assert!(!invoke(&args).status.success());
    }
    assert!(!root.path().join("cache").exists());
    assert!(!root.path().join("data").exists());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn final_symlink_and_fifo_are_rejected_without_reading_or_waiting() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("config")).unwrap();
        let file = root.path().join("config/config.toml");
        fs::write(root.path().join("target"), "auto_index=true").unwrap();
        symlink(root.path().join("target"), &file).unwrap();
        let output = observe::migration_plan(
            &root.path().join("config"),
            &root.path().join("cache"),
            &settings::standalone().unwrap(),
            PlanningLimits::standard(),
            None,
        )
        .unwrap();
        assert!(output.to_json(100_000).unwrap().contains("non_regular"));
        fs::remove_file(&file).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );
        // CLI is supervised so a regression cannot hang the entire suite.
        let mut child = Command::new(env!("CARGO_BIN_EXE_docsbase"))
            .args(["config", "migrate", "--to", "2"])
            .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
            .env("DOCSBASE_CACHE_DIR", root.path().join("cache"))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("FIFO source blocked the observer");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("non_regular")
        );
    }
}

#[test]
fn legacy_project_budget_conflict_is_explicit_under_v2_global() {
    let root = tempfile::tempdir().unwrap();
    write_global(
        root.path(),
        "config_version=2\n[vector_cache]\nmax_bytes=1000\n",
    );
    let project = register(root.path(), "project");
    fs::write(
        project.join(".docsbase.toml"),
        "[hybrid.cache]\nmax_bytes=2000\n",
    )
    .unwrap();
    let view = accepted(root.path());
    assert_eq!(
        view["summary"]["scope_conflicts"],
        json!([{"source":{"kind":"project", "value":project.join(".docsbase.toml")}, "legacy_vector_max_bytes":2000}])
    );
}

#[test]
fn migration_cli_never_provisions_selected_local_or_remote_providers() {
    let root = tempfile::tempdir().unwrap();
    write_global(
        root.path(),
        "[providers.openrouter]\nenabled=true\n[providers.fastembed]\nenabled=true\nruntime='path'\nruntime_path='/missing-ort-no-load'\n[hybrid]\nenabled=true\n[hybrid.embedding]\nprovider='openrouter'\n[hybrid.reranker]\nprovider='fastembed'\nmodel='bge-reranker-base'\n",
    );
    if !cfg!(feature = "hybrid-local") {
        // The existing lean resolver has no local model catalog. Keep its
        // admission behavior, but still exercise enabled remote stages and
        // persisted local provider records without provisioning either one.
        let path = root.path().join("config/config.toml");
        let source = fs::read_to_string(&path)
            .unwrap()
            .replace("provider='fastembed'", "provider='openrouter'");
        fs::write(path, source).unwrap();
    }
    let before = snapshot(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args([
            "config",
            "migrate",
            "--to",
            "2",
            "--accept-suggested-vector-budget",
        ])
        .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
        .env("DOCSBASE_CACHE_DIR", root.path().join("cache"))
        .env("DOCSBASE_DATA_DIR", root.path().join("data"))
        .env("DOCSBASE_DAEMON_EXE", root.path().join("must-not-start"))
        .env("ORT_DYLIB_PATH", root.path().join("must-not-load"))
        .env_remove("OPENROUTER_API_KEY")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot(root.path()), before);
}

#[cfg(windows)]
mod windows {
    use super::*;
    #[test]
    fn final_reparse_point_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("config")).unwrap();
        fs::write(root.path().join("target"), "auto_index=true").unwrap();
        std::os::windows::fs::symlink_file(
            root.path().join("target"),
            root.path().join("config/config.toml"),
        )
        .unwrap();
        let output = observe::migration_plan(
            &root.path().join("config"),
            &root.path().join("cache"),
            &settings::standalone().unwrap(),
            PlanningLimits::standard(),
            None,
        )
        .unwrap();
        assert!(output.to_json(100_000).unwrap().contains("non_regular"));
        assert_eq!(
            fs::read_to_string(root.path().join("target")).unwrap(),
            "auto_index=true"
        );
    }
}

#[test]
fn migration_cli_uses_complete_persisted_inventory() {
    let root = tempfile::tempdir().unwrap();
    write_global(root.path(), "[hybrid.cache]\nmax_bytes=1000\n");
    let project = register(root.path(), "first");
    fs::write(
        project.join(".docsbase.toml"),
        "[hybrid.cache]\nmax_bytes=4000\n",
    )
    .unwrap();
    let missing = register(root.path(), "missing");
    fs::remove_dir(missing).unwrap();
    let expected = accepted(root.path());
    let before = snapshot(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_docsbase"))
        .args([
            "config",
            "migrate",
            "--to",
            "2",
            "--accept-suggested-vector-budget",
        ])
        .env("DOCSBASE_CONFIG_DIR", root.path().join("config"))
        .env("DOCSBASE_CACHE_DIR", root.path().join("cache"))
        .env("DOCSBASE_DATA_DIR", root.path().join("data"))
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    assert_eq!(snapshot(root.path()), before);
}
