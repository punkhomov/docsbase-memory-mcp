//! `list_docs` pagination, `read_neighbors` windows and `get_doc` path
//! containment (FR-23…FR-25, FR-32; I4).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use docsbase_memory::daemon::lifecycle::stop_daemon;
use docsbase_memory::ipc::client::Client;

struct Env {
    cache: TempDir,
    root: TempDir,
    outside: TempDir,
    daemon: Option<Child>,
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        let outside = TempDir::new().expect("outside");
        for (rel, body) in files {
            write_file(root.path(), rel, body.as_bytes());
        }
        Self {
            cache,
            root,
            outside,
            daemon: None,
        }
    }

    fn cache(&self) -> &Path {
        self.cache.path()
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn outside(&self) -> &Path {
        self.outside.path()
    }

    fn cmd(&self) -> Command {
        let mut command = Command::new(daemon_bin());
        command
            .env("DOCSBASE_CACHE_DIR", self.cache())
            .env("DOCSBASE_CONFIG_DIR", self.cache().join("config"))
            .current_dir(self.root());
        command
    }

    fn start_daemon(&mut self) {
        let child = self
            .cmd()
            .args(["serve", "--detached", "--grace-ms", "3000"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || self
                .cache()
                .join("state/daemon.sock")
                .exists()),
            "socket must appear"
        );
    }

    fn client(&self) -> Client {
        for _ in 0..100 {
            if let Some(client) = Client::connect(self.cache()) {
                return client;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not connectable");
    }

    fn bound(&mut self) -> Client {
        let mut client = self.client();
        client.handshake(self.root()).expect("bind session");
        client
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = stop_daemon(self.cache());
        if let Some(child) = self.daemon.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn daemon_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin!("docsbase").to_path_buf()
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn index_project(env: &Env) {
    let mut registry = env.client();
    registry.handshake_registry().expect("registry hello");
    registry
        .call_tool(
            "index_project",
            json!({ "path": env.root().to_str().expect("utf8") }),
        )
        .expect("index_project");
}

const GUIDE: &str = "# Guide\n\nintro MARKINTRO.\n\n## Alpha\n\nalpha MARKALPHA paragraph.\n\n## Beta\n\nbeta MARKBETA paragraph.\n\n## Gamma\n\ngamma MARKGAMMA paragraph.\n";

fn page(client: &mut Client, limit: usize, cursor: Option<&str>) -> Value {
    let args = match cursor {
        Some(cursor) => json!({ "limit": limit, "cursor": cursor }),
        None => json!({ "limit": limit }),
    };
    client.call_tool("list_docs", args).expect("list_docs")
}

fn paths_of(value: &Value) -> Vec<String> {
    value["docs"]
        .as_array()
        .expect("docs array")
        .iter()
        .map(|doc| doc["path"].as_str().expect("path").to_owned())
        .collect()
}

#[test]
fn pagination_stable() {
    let mut env = Env::new(&[]);
    for index in 1..=7 {
        write_file(
            env.root(),
            &format!("d/{index:02}.md"),
            format!("# Doc {index}\n\nbody {index} paginawidget.\n").as_bytes(),
        );
    }
    env.start_daemon();
    index_project(&env);
    let mut client = env.bound();

    let first = page(&mut client, 3, None);
    assert_eq!(paths_of(&first), vec!["d/01.md", "d/02.md", "d/03.md"]);
    let cursor = first["next_cursor"].as_str().expect("cursor").to_owned();
    assert_eq!(cursor, "d/03.md", "cursor is the last seen path");

    // A late insert before the cursor must not shift the remaining pages.
    write_file(
        env.root(),
        "d/00-new.md",
        b"# New\n\nlate insert paginawidget.\n",
    );
    // Wait until the watcher indexed it, otherwise the assertion is vacuous
    // (FR-15: search sees a new file within ~2 s).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut visible = false;
    while Instant::now() < deadline {
        let first_page_now = page(&mut client, 2, None);
        if paths_of(&first_page_now).contains(&"d/00-new.md".to_owned()) {
            visible = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(visible, "late insert must become visible before paging on");

    let second = page(&mut client, 3, Some(&cursor));
    assert_eq!(paths_of(&second), vec!["d/04.md", "d/05.md", "d/06.md"]);
    let cursor = second["next_cursor"].as_str().expect("cursor").to_owned();
    let third = page(&mut client, 3, Some(&cursor));
    assert_eq!(paths_of(&third), vec!["d/07.md"]);
    assert!(
        third["next_cursor"].is_null(),
        "last page has no cursor: {third}"
    );

    let mut all: Vec<String> = Vec::new();
    all.extend(paths_of(&first));
    all.extend(paths_of(&second));
    all.extend(paths_of(&third));
    let mut unique = all.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        all.len(),
        "no duplicates across pages: {all:?}"
    );
    assert!(!all.contains(&"d/00-new.md".to_owned()));
}

#[test]
fn neighbors_window() {
    let mut env = Env::new(&[("guide.md", GUIDE)]);
    env.start_daemon();
    index_project(&env);
    let mut client = env.bound();

    let hits = client
        .call_tool("search_docs", json!({ "query": "MARKBETA", "limit": 1 }))
        .expect("search_docs");
    let chunk_id = hits[0]["chunk_id"].as_i64().expect("chunk_id");

    let window = client
        .call_tool(
            "read_neighbors",
            json!({ "chunk_id": chunk_id, "before": 1, "after": 1 }),
        )
        .expect("read_neighbors");
    let chunks = window.as_array().expect("chunks array");
    assert_eq!(chunks.len(), 3, "one chunk on each side: {window}");
    assert!(
        chunks[0]["text"]
            .as_str()
            .expect("text")
            .contains("MARKALPHA")
    );
    assert!(
        chunks[1]["text"]
            .as_str()
            .expect("text")
            .contains("MARKBETA")
    );
    assert_eq!(chunks[1]["chunk_id"], chunk_id);
    assert!(
        chunks[2]["text"]
            .as_str()
            .expect("text")
            .contains("MARKGAMMA")
    );
    assert!(chunks[0]["lines"].is_array() && chunks[0]["heading_path"].is_string());
}

#[test]
fn get_doc_rejects_escape() {
    let mut env = Env::new(&[("ok.md", "# ok\n\nfine.\n")]);
    write_file(env.outside(), "secret.md", b"# secret\n\nclassified.\n");
    std::os::unix::fs::symlink(
        env.outside().join("secret.md"),
        env.root().join("escape.md"),
    )
    .expect("symlink");
    env.start_daemon();
    index_project(&env);
    let mut client = env.bound();

    let err = client
        .call_tool("get_doc", json!({ "path": "escape.md" }))
        .expect_err("symlink out of root must be rejected");
    let text = err.to_string();
    assert!(
        text.contains("outside") || text.contains("root"),
        "unexpected error: {text}"
    );

    let err = client
        .call_tool("get_doc", json!({ "path": "../secret.md" }))
        .expect_err("parent traversal must be rejected");
    assert!(err.to_string().contains("..") || err.to_string().contains("outside"));
}

#[test]
fn get_doc_outside_project() {
    let mut env = Env::new(&[("docs/hello.md", "# Hello\n\nhello world\n")]);
    env.start_daemon();
    index_project(&env);
    let mut client = env.bound();

    let doc = client
        .call_tool("get_doc", json!({ "path": "docs/hello.md" }))
        .expect("get_doc");
    assert_eq!(doc["path"], "docs/hello.md");
    assert_eq!(doc["content"], "# Hello\n\nhello world\n");

    for path in ["/etc/hostname", "docs/../outside.md", ""] {
        let err = client
            .call_tool("get_doc", json!({ "path": path }))
            .expect_err("path must be rejected");
        assert!(
            !err.to_string().is_empty(),
            "error message missing for {path:?}"
        );
    }
}
