//! Actual configured direct command boundary, real codecs/HTTP and no paid calls.
use crate::hybrid::work::Cancellation;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn child(name: &str, ranking: &str) -> Option<std::process::Output> {
    const CHILD: &str = "DOCSBASE_B3_COMMAND_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(
        root.path().join("guide.md"),
        "# Direct\n\ncommandsearchwidget\n",
    )
    .unwrap();
    std::fs::write(root.path().join(".docsbase.toml"),format!("[hybrid]\nenabled=true\nranking='{ranking}'\n[hybrid.embedding]\nprovider='openrouter'\nmodel='fixture'\ndimensions=3\nnormalize=true\n")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("cli::command_tests::{name}"),
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("DOCSBASE_CACHE_DIR", cache.path())
        .env("DOCSBASE_CONFIG_DIR", cache.path().join("config"))
        .env("DOCSBASE_DATA_DIR", cache.path().join("data"))
        .env("OPENROUTER_API_KEY", "fixture-secret")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "actual direct command child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output)
}
fn read_request(socket: &std::net::TcpStream) -> Option<serde_json::Value> {
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap() == 0 {
        return None;
    }
    assert!(
        line.starts_with("POST /embeddings "),
        "real configured request path"
    );
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(size) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = size.trim().parse::<usize>().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Some(serde_json::from_slice(&body).unwrap())
}
fn response(socket: &mut std::net::TcpStream) {
    embedding_response(socket, &[1.0, 0.0, 0.0]);
}

fn embedding_response(socket: &mut std::net::TcpStream, vector: &[f32]) {
    let body=serde_json::json!({"object":"list","data":[{"index":0,"embedding":vector}],"model":"fixture","usage":{"prompt_tokens":1,"total_tokens":1}}).to_string();
    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
}
fn http_fixture(
    expected: usize,
) -> (
    std::net::SocketAddr,
    Arc<crate::hybrid::openrouter::test_http::Redirect>,
    std::thread::JoinHandle<Vec<serde_json::Value>>,
) {
    http_fixture_with(expected, |_, socket| response(socket))
}

fn http_fixture_with(
    expected: usize,
    mut respond: impl FnMut(usize, &mut std::net::TcpStream) + Send + 'static,
) -> (
    std::net::SocketAddr,
    Arc<crate::hybrid::openrouter::test_http::Redirect>,
    std::thread::JoinHandle<Vec<serde_json::Value>>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = crate::hybrid::openrouter::test_http::install(format!("http://{addr}"));
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..expected {
            let (mut socket, _) = listener.accept().unwrap();
            let Some(request) = read_request(&socket) else {
                break;
            };
            requests.push(request);
            respond(index, &mut socket);
        }
        requests
    });
    (addr, calls, server)
}

#[test]
#[cfg(unix)]
fn configured_direct_catalog_index_preserves_the_head_on_failure() {
    use crate::store::registry_inspect::{ProjectIndexObservation, inspect_project_index};

    if let Some(output) = child(
        "configured_direct_catalog_index_preserves_the_head_on_failure",
        "rrf",
    ) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("\"guide.md\"") && stdout.contains("\"dense\""),
            "{stdout}"
        );
        return;
    }
    let cache = crate::config::paths::cache_dir().unwrap();
    let config = crate::config::paths::config_dir().unwrap();
    let target = std::env::current_dir().unwrap();
    let project_file = target.join(".docsbase.toml");
    let project_source = std::fs::read_to_string(&project_file).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "config_version = 2\n").unwrap();
    let settings = crate::store::settings::standalone().unwrap();
    crate::store::Db::open_for_index(&cache, &settings, true)
        .unwrap()
        .close()
        .unwrap();
    assert_website_rejects_direct_hybrid_before_registration(&cache, &config, &target, &settings);
    let alter = project_file.clone();
    let changed_source = project_source.replace("model='fixture'", "model='other'");
    let (addr, calls, server) = http_fixture_with(4, move |request, socket| {
        if request == 1 {
            embedding_response(socket, &[1.0]);
        } else {
            if request == 2 {
                std::fs::write(&alter, &changed_source).unwrap();
            }
            response(socket);
        }
    });
    let index = || super::index::run_with_settings(Some(&target), &settings);
    let selected = || {
        let limits = crate::config::observe::PlanningLimits::standard().registry;
        let observation = inspect_project_index(&cache, 1, &settings, limits).unwrap();
        let ProjectIndexObservation::Present(index) = observation else {
            panic!("catalog head unavailable: {observation:?}");
        };
        index
    };
    index().unwrap();
    let previous = selected();
    std::fs::write(
        target.join("guide.md"),
        "# Updated\n\nrevisedsearchwidget\n",
    )
    .unwrap();
    let error = index().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible OpenRouter embeddings schema"),
        "{error}"
    );
    assert_eq!(selected(), previous);

    let error = index().unwrap_err();
    assert!(
        error.to_string().contains("configuration changed"),
        "{error}"
    );
    assert_eq!(selected(), previous);

    std::fs::write(project_file, project_source).unwrap();
    index().unwrap();
    assert_ne!(selected().generation_id, previous.generation_id);
    let options = super::search::Options {
        limit: 10,
        explain: true,
        snippet: false,
        dedupe: "none".into(),
        n_per_doc: None,
        path_prefix: None,
        tags: Vec::new(),
    };
    let search = super::search::run_with_settings("semanticneedle", &options, &settings);
    let _ = std::net::TcpStream::connect(addr);
    let requests = server.join().unwrap();
    search.unwrap();
    assert_eq!(requests.len(), 4, "the retry reuses the valid vector");
    assert!(
        requests[0]["input"]
            .to_string()
            .contains("commandsearchwidget")
    );
    assert_eq!(requests[3]["input"][0], "semanticneedle");
    assert_eq!(calls.synchronous.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
fn assert_website_rejects_direct_hybrid_before_registration(
    cache: &std::path::Path,
    config: &std::path::Path,
    target: &std::path::Path,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) {
    let file = config.join("config.toml");
    std::fs::write(
        &file,
        format!(
            "config_version = 2\n[daemon]\nprofile = {{ mode = 'website', project_root = {} }}\n",
            toml::Value::String(target.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let error = super::index::run_with_settings(Some(target), settings).unwrap_err();
    assert!(error.to_string().contains("lexical"), "{error}");
    let db = crate::store::Db::open_for_index(cache, settings, false).unwrap();
    assert_eq!(
        crate::daemon::registry::list_projects(&db).unwrap(),
        [] as [crate::store::models::Project; 0]
    );
    db.close().unwrap();
    std::fs::write(file, "config_version = 2\n").unwrap();
}

#[test]
fn configured_direct_index_uses_async_http_without_synchronous_transport() {
    if child(
        "configured_direct_index_uses_async_http_without_synchronous_transport",
        "lexical",
    )
    .is_some()
    {
        return;
    }
    let (addr, calls, server) = http_fixture(1);
    let settings = crate::store::settings::standalone().unwrap();
    let target = std::env::current_dir().unwrap();
    let result = super::index::run_with_settings(Some(&target), &settings);
    let _ = std::net::TcpStream::connect(addr);
    let requests = server.join().unwrap();
    result.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "actual configured reqwest HTTP reached fixture"
    );
    let request = &requests[0];
    assert_eq!(request["model"], "fixture");
    assert_eq!(request["dimensions"], 3);
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    assert_eq!(
        calls.synchronous.load(Ordering::SeqCst),
        0,
        "operational direct indexing must not enter synchronous transport/block_on"
    );
}
#[test]
fn configured_direct_search_uses_async_http_without_synchronous_transport() {
    if let Some(output) = child(
        "configured_direct_search_uses_async_http_without_synchronous_transport",
        "rrf",
    ) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"exact-fallback\""), "{stdout}");
        return;
    }
    let config = crate::config::paths::config_dir().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "config_version = 2\n[dense]\nmax_vectors = 0\n",
    )
    .unwrap();
    let (addr, calls, server) = http_fixture(2);
    let settings = crate::store::settings::standalone().unwrap();
    let target = std::env::current_dir().unwrap();
    super::index::run_with_settings(Some(&target), &settings).unwrap();
    let options = super::search::Options {
        limit: 10,
        explain: true,
        snippet: false,
        dedupe: "none".into(),
        n_per_doc: None,
        path_prefix: None,
        tags: Vec::new(),
    };
    let result = super::search::run_with_settings("commandsearchwidget", &options, &settings);
    let _ = std::net::TcpStream::connect(addr);
    let requests = server.join().unwrap();
    result.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "configured document and query HTTP each execute once"
    );
    assert_eq!(
        calls.synchronous.load(Ordering::SeqCst),
        0,
        "operational direct search must not enter synchronous transport/block_on"
    );
    let (retired, on_async) = crate::daemon::server::search_async::test_drop::snapshot();
    assert!(retired > 0, "actual prepared read/model owners retired");
    assert!(
        !on_async,
        "direct search native owners retire outside work future"
    );
}
#[test]
fn direct_command_error_and_unwind_wait_for_actual_native_terminal_retirement() {
    if child(
        "direct_command_error_and_unwind_wait_for_actual_native_terminal_retirement",
        "lexical",
    )
    .is_some()
    {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = crate::hybrid::openrouter::test_http::install(format!("http://{addr}"));
    for unwind in [false, true] {
        let listener = listener.try_clone().unwrap();
        let entered = Cancellation::new();
        let http_entered = entered.clone();
        let (http_release, http_proceed) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let request = read_request(&socket);
            http_entered.cancel();
            http_proceed.recv_timeout(Duration::from_secs(10)).unwrap();
            request
        });
        let (native_entered, native_start) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let proceed = Mutex::new(proceed);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let (finished, done) = std::sync::mpsc::channel();
        let command = std::thread::spawn(move || {
            let result = super::command::run(
                crate::config::Config::global().unwrap(),
                move |context| async move {
                    context.jobs.observe_retirement(Arc::new(move |event| {
                        observed.lock().unwrap().push(event.to_owned());
                        if event == "before_index" {
                            native_entered.send(()).unwrap();
                            proceed
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(10))
                                .unwrap();
                        }
                    }));
                    let settings = crate::store::settings::standalone().unwrap();
                    let target = std::env::current_dir().unwrap();
                    let mut work = Box::pin(super::index::run_in(
                        context,
                        Some(target),
                        settings,
                        Instant::now(),
                    ));
                    tokio::select! {result=&mut work=>result,()=entered.cancelled()=>{assert!(!unwind,"controlled command work unwind");Err(crate::error::Error::internal("controlled command work error"))}}
                },
            );
            finished
                .send(result.map_err(|error| error.inner_message().clone()))
                .unwrap();
        });
        let reached = native_start.recv_timeout(Duration::from_secs(5));
        let blocked = matches!(done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        let _ = release.send(());
        let result = done.recv_timeout(Duration::from_secs(5));
        let _ = http_release.send(());
        let request = server.join().unwrap();
        command.join().unwrap();
        assert!(reached.is_ok(), "actual native terminal entered");
        assert!(
            blocked,
            "command returned before physical native retirement"
        );
        assert!(request.is_some(), "configured actual HTTP reached");
        let message = result.unwrap().unwrap_err();
        assert!(message.contains(if unwind { "work unwound" } else { "work error" }));
        assert_direct_failure_state();
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .position(|event| event == "after_index")
                .unwrap()
                < events
                    .iter()
                    .position(|event| event == "terminal_finished")
                    .unwrap()
        );
    }
    assert_direct_retry(&listener);
    assert_eq!(calls.synchronous.load(Ordering::SeqCst), 0);
}
#[test]
fn current_thread_command_refuses_before_polling_or_effects() {
    if child(
        "current_thread_command_refuses_before_polling_or_effects",
        "lexical",
    )
    .is_some()
    {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let polled = std::sync::atomic::AtomicBool::new(false);
    let result = runtime.block_on(async {
        super::command::run(crate::config::Config::global().unwrap(), |_| async {
            polled.store(true, Ordering::SeqCst);
            Ok(())
        })
    });
    assert!(
        result
            .unwrap_err()
            .inner_message()
            .contains("current-thread")
    );
    assert!(!polled.load(Ordering::SeqCst));
    assert!(
        !crate::config::paths::cache_dir()
            .unwrap()
            .join(crate::store::DB_FILE)
            .exists()
    );
}

fn assert_direct_failure_state() {
    let cache = crate::config::paths::cache_dir().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        cache.join(crate::store::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let status: String = db
        .query_row("SELECT status FROM projects", [], |row| row.get(0))
        .unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM docs WHERE content_hash=''",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "error",
        "failed direct Full must terminalize after physical retirement"
    );
    assert!(
        pending > 0,
        "failed started direct job retains recovery markers"
    );
}

fn assert_direct_retry(listener: &std::net::TcpListener) {
    let retry_listener = listener.try_clone().unwrap();
    let retry = std::thread::spawn(move || {
        let (mut socket, _) = retry_listener.accept().unwrap();
        assert!(read_request(&socket).is_some());
        response(&mut socket);
    });
    let target = std::env::current_dir().unwrap();
    super::index::run_with_settings(
        Some(&target),
        &crate::store::settings::standalone().unwrap(),
    )
    .unwrap();
    retry.join().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        crate::config::paths::cache_dir()
            .unwrap()
            .join(crate::store::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (status,pending,sync):(String,i64,i64)=db.query_row("SELECT status,(SELECT COUNT(*) FROM docs WHERE content_hash=''),(SELECT COUNT(*) FROM sync_jobs) FROM projects",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(status, "indexed");
    assert_eq!(pending, 0);
    assert_eq!(sync, 0, "direct Full never invents a Sync row");
}
