use std::fs;
use std::path::Path;

use assert_cmd::Command;
use docsbase_memory::daemon::registry::ensure_project;
use docsbase_memory::store::Db;
use tempfile::TempDir;

struct Env {
    cache: TempDir,
    root: TempDir,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        Self { cache, root }
    }

    fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("docsbase");
        cmd.env("DOCSBASE_CACHE_DIR", self.cache.path());
        cmd.env("DOCSBASE_CONFIG_DIR", self.cache.path().join("config"));
        cmd.current_dir(self.root.path());
        cmd
    }

    fn index(&self) {
        self.cmd().args(["index", "."]).assert().success();
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("utf8");
    serde_json::from_str(stdout.trim()).expect("json")
}

fn stderr_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const DOC: &str =
    "---\ntitle: Guide\ntags: setup, widgets\n---\n\n# Install\n\ninstaller prose about widgets.\n";

#[test]
fn search_without_daemon() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.index();

    let output = env
        .cmd()
        .args(["search", "widgets", "--limit", "5"])
        .assert()
        .success()
        .get_output()
        .clone();
    let rows = stdout_json(&output);
    let rows = rows.as_array().expect("array");
    assert!(!rows.is_empty(), "expected hits: {rows:?}");
    let first = &rows[0];
    assert_eq!(first["path"], "docs/a.md");
    assert_eq!(first["heading_path"], "Install");
    let lines = first["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], 6, "file-relative first line");
    assert!(first["score"].as_f64().expect("score") > 0.0);
}

#[test]
fn not_indexed_message() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let mut db = Db::open(env.cache.path()).expect("db");
    ensure_project(&mut db, env.root.path()).expect("register");
    drop(db);

    let output = env
        .cmd()
        .args(["search", "widgets"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_text(&output);
    assert!(stderr.contains("not indexed"), "{stderr}");
    assert!(
        stderr.contains("docsbase index"),
        "hint must surface: {stderr}"
    );
}

#[test]
fn unregistered_project_message() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let output = env
        .cmd()
        .args(["search", "widgets"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = stderr_text(&output);
    assert!(stderr.contains("no projects are registered"), "{stderr}");
    assert!(
        stderr.contains("docsbase index"),
        "hint must surface: {stderr}"
    );
}

#[test]
fn status_lists_projects() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.index();

    let output = env
        .cmd()
        .arg("status")
        .assert()
        .success()
        .get_output()
        .clone();
    let json = stdout_json(&output);
    assert_eq!(json["schema_version"], 1);
    let projects = json["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["status"], "indexed");
    assert_eq!(projects[0]["docs"], 1);
    assert!(projects[0]["chunks"].as_i64().expect("chunks") > 0);
    assert!(projects[0]["last_indexed_at"].as_i64().expect("stamp") > 0);
}

#[test]
fn list_documents() {
    let env = Env::new(&[
        ("docs/a.md", DOC),
        ("docs/b.md", "# Other\n\nnothing here\n"),
    ]);
    env.index();

    let output = env
        .cmd()
        .arg("list")
        .assert()
        .success()
        .get_output()
        .clone();
    let json = stdout_json(&output);
    let docs = json["docs"].as_array().expect("docs");
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0]["path"], "docs/a.md");
    assert_eq!(docs[0]["title"], "Guide");
    assert!(docs[0]["size"].as_i64().expect("size") > 0);
    assert!(docs[0]["chunks"].as_i64().expect("chunks") > 0);
}
