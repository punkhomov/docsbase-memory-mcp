use std::fs;
use std::path::Path;

use assert_cmd::Command;
use fd_lock::RwLock;
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

const DOC: &str = "# Guide\n\ninstaller prose for the indexer.\n";

#[test]
fn registers_and_indexes() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let output = env
        .cmd()
        .args(["index", "."])
        .assert()
        .success()
        .get_output()
        .clone();

    let json = stdout_json(&output);
    assert_eq!(json["project_id"], 1);
    assert_eq!(json["stats"]["docs"], 1);
    assert!(json["stats"]["chunks"].as_u64().expect("chunks") > 0);
    assert!(env.cache.path().join("registry.db").exists());
    assert!(env.cache.path().join("projects/1/tantivy").exists());
    assert!(env.cache.path().join("projects/1/.writer.lock").exists());
}

#[test]
fn second_run_incremental() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.cmd().args(["index", "."]).assert().success();

    let output = env
        .cmd()
        .args(["index", "."])
        .assert()
        .success()
        .get_output()
        .clone();
    let json = stdout_json(&output);
    assert_eq!(json["project_id"], 1);
    assert_eq!(json["stats"]["docs"], 0);
    assert_eq!(json["stats"]["skipped"], 1);
}

#[test]
fn outside_root_error() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    let output = env
        .cmd()
        .arg("index")
        .arg(env.cache.path())
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        stderr_text(&output).contains("project root"),
        "{}",
        stderr_text(&output)
    );
}

#[test]
fn lease_blocks_second_writer() {
    let env = Env::new(&[("docs/a.md", DOC)]);
    env.cmd().args(["index", "."]).assert().success();

    let lock_path = env.cache.path().join("projects/1/.writer.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("lock file");
    let mut lock = RwLock::new(file);
    let guard = lock.write().expect("acquire lease");

    let output = env
        .cmd()
        .args(["index", "."])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        stderr_text(&output).contains("another writer"),
        "{}",
        stderr_text(&output)
    );

    drop(guard);
    env.cmd().args(["index", "."]).assert().success();
}
