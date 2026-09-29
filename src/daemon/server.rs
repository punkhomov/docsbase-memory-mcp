//! IPC server: handshake, sessions and tool routing (FR-3, FR-7, FR-8, FR-12,
//! FR-33; I2, I6, I8).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::daemon::registry;
use crate::daemon::session::SessionRegistry;
use crate::daemon::tools;
use crate::error::{Error, Result};
use crate::ipc::protocol::{self, PROTOCOL_VERSION, Request, Response};
use crate::store::Db;
use crate::store::migrations;
use crate::watch::Watchers;

/// Daemon state shared by all connections.
pub struct Shared {
    /// Cache root being served.
    pub cache: PathBuf,
    db: Mutex<Db>,
    /// Live sessions.
    pub sessions: SessionRegistry,
    /// Per-project filesystem watchers (FR-15).
    pub watchers: Arc<Watchers>,
    /// Set by the first `StopDaemon`; later stops return immediately (FR-6).
    stopping: AtomicBool,
}

impl Shared {
    /// Opens the registry database and prepares shared state.
    ///
    /// # Errors
    /// Returns [`Error::Internal`]/[`Error::Admission`] from [`Db::open`].
    pub fn open(cache: &Path) -> Result<Arc<Self>> {
        let db = Db::open(cache)?;
        let orphans = crate::store::repo::fail_orphan_sync_jobs(db.connection())?;
        if orphans > 0 {
            eprintln!("warning: {orphans} interrupted sync job(s) marked as error");
        }
        let shared = Arc::new(Self {
            cache: cache.to_path_buf(),
            db: Mutex::new(db),
            sessions: SessionRegistry::new(),
            watchers: Arc::new(Watchers::new()),
            stopping: AtomicBool::new(false),
        });
        match shared.watchers.start_registered(cache, &shared.db()) {
            Ok(started) if started > 0 => eprintln!("watching {started} indexed project(s)"),
            Ok(_) => {}
            Err(err) => eprintln!("warning: cannot start watchers: {err}"),
        }
        Ok(shared)
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Serves one connection until EOF, cleanup included (FR-8, I2).
pub async fn handle_connection(
    shared: Arc<Shared>,
    stream: UnixStream,
    in_flight: Arc<AtomicUsize>,
    events: mpsc::UnboundedSender<()>,
    shutdown: watch::Sender<bool>,
) {
    let (read_half, mut writer) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut session_id: Option<u64> = None;
    let mut cancel_rx: Option<oneshot::Receiver<()>> = None;
    let mut hello_done = false;

    loop {
        let next_line = match cancel_rx.as_mut() {
            Some(cancel) => tokio::select! {
                line = lines.next_line() => line,
                // The janitor reaped this session: stop holding the socket.
                _ = cancel => break,
            },
            None => lines.next_line().await,
        };
        let Ok(Some(line)) = next_line else {
            break;
        };
        let guard = InFlightGuard::new(&in_flight);
        let (response, close) = handle_request(
            &shared,
            &shutdown,
            &in_flight,
            &mut hello_done,
            &mut session_id,
            &mut cancel_rx,
            line.as_bytes(),
        )
        .await;
        // The request is only settled once its response is on the wire:
        // otherwise an armed grace deadline could tear the socket down
        // between completing the work and delivering the reply.
        let Ok(bytes) = protocol::encode(&response) else {
            drop(guard);
            let _ = events.send(());
            break;
        };
        if writer.write_all(&bytes).await.is_err() {
            drop(guard);
            let _ = events.send(());
            break;
        }
        drop(guard);
        // Any request may change liveness accounting (registration, EOF of a
        // session, completion of a request).
        let _ = events.send(());
        if close {
            break;
        }
    }

    if let Some(id) = session_id {
        shared.sessions.leave(id);
    }
    let _ = events.send(());
}

async fn handle_request(
    shared: &Arc<Shared>,
    shutdown: &watch::Sender<bool>,
    in_flight: &Arc<AtomicUsize>,
    hello_done: &mut bool,
    session_id: &mut Option<u64>,
    cancel_rx: &mut Option<oneshot::Receiver<()>>,
    line: &[u8],
) -> (Response, bool) {
    let request = match protocol::decode_request(line) {
        Ok(request) => request,
        Err(err) => return (error_response(&err), false),
    };
    match request {
        Request::Hello {
            protocol_version,
            build_id,
            ..
        } => {
            if protocol_version != PROTOCOL_VERSION {
                let err = Error::Protocol {
                    message: format!("protocol version {protocol_version} != {PROTOCOL_VERSION}"),
                };
                return (error_response(&err), true);
            }
            if build_id != protocol::build_id() {
                let daemon_build = protocol::build_id();
                crate::conflict::record(
                    &shared.cache,
                    &crate::conflict::Conflict {
                        kind: "hello_build_mismatch",
                        expected: &daemon_build,
                        actual: &build_id,
                        build_id: &build_id,
                        // The refused frontend's schema is unknown; record
                        // the daemon's own expectation here.
                        schema_version: migrations::SCHEMA_VERSION,
                        cache_root: &shared.cache,
                        pid: std::process::id(),
                        recorded_build_id: Some(&daemon_build),
                        recorded_schema_version: Some(migrations::SCHEMA_VERSION),
                        holder_pid: None,
                    },
                );
                let err = Error::Admission {
                    message: format!(
                        "build {build_id:?} != {daemon_build:?}; run `docsbase install` or restart the daemon and frontend"
                    ),
                };
                return (error_response(&err), true);
            }
            *hello_done = true;
            (
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    build_id: protocol::build_id(),
                    schema_version: migrations::SCHEMA_VERSION,
                },
                false,
            )
        }
        Request::RegisterSession { pid, cwd } => {
            if !*hello_done {
                return (hello_required(), true);
            }
            register_and_respond(shared, session_id, cancel_rx, pid, &cwd, false).await
        }
        Request::RegisterUnbound { pid, cwd } => {
            if !*hello_done {
                return (hello_required(), true);
            }
            register_and_respond(shared, session_id, cancel_rx, pid, &cwd, true).await
        }
        Request::CallTool { name, args } => {
            if !*hello_done {
                return (hello_required(), true);
            }
            let value = call_tool(shared, *session_id, &name, args).await;
            match value {
                Ok(value) => (Response::ToolResult { value }, false),
                Err(err) => (error_response(&err), false),
            }
        }
        Request::StopDaemon => {
            // Idempotent: only the first stop drains and signals shutdown, so
            // concurrent stops (install retries) cannot wait on each other
            // (FR-5, FR-6). The drain lets every other request finish; the
            // process then lives until background jobs complete.
            if !shared.stopping.swap(true, Ordering::SeqCst) {
                while in_flight.load(Ordering::SeqCst) > 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                let _ = shutdown.send(true);
            }
            (Response::ToolResult { value: Value::Null }, false)
        }
    }
}

async fn register_and_respond(
    shared: &Arc<Shared>,
    session_id: &mut Option<u64>,
    cancel_rx: &mut Option<oneshot::Receiver<()>>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
) -> (Response, bool) {
    match register_session(shared, pid, cwd, allow_unbound).await {
        Ok((id, value, cancel)) => {
            if let Some(previous) = session_id.replace(id) {
                shared.sessions.leave(previous);
            }
            *cancel_rx = Some(cancel);
            (Response::ToolResult { value }, false)
        }
        Err(err) => (error_response(&err), false),
    }
}

async fn register_session(
    shared: &Arc<Shared>,
    pid: u32,
    cwd: &Path,
    allow_unbound: bool,
) -> Result<(u64, Value, oneshot::Receiver<()>)> {
    let shared_for_task = Arc::clone(shared);
    let cwd = cwd.to_path_buf();
    let cwd_for_task = cwd.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<crate::store::models::Project> {
        // Own connection: the shared read connection must stay free while a
        // full index runs (F4/NFR-1).
        let mut db = Db::open(&shared_for_task.cache)?;
        let project_err = match registry::resolve_by_cwd(&db, &cwd_for_task) {
            Ok(project) => return Ok(project),
            Err(err) => err,
        };
        let Ok(root) = registry::project_root_for(&cwd_for_task, &shared_for_task.cache) else {
            return Err(project_err);
        };
        let config = Config::load(Some(&root))?;
        if !config.auto_index {
            return Err(project_err);
        }
        let project = registry::ensure_project(&mut db, &root)?;
        if let Err(err) =
            tools::run_project_index(&mut db, &shared_for_task.cache, &project, &config)
        {
            // A concurrent registration may have won the writer lease and
            // already indexed the project; bind to that result instead.
            if let Some(indexed) = registry::resolve_by_cwd(&db, &cwd_for_task)
                .ok()
                .filter(|project| project.status == crate::store::models::ProjectStatus::Indexed)
            {
                return Ok(indexed);
            }
            return Err(err);
        }
        registry::project_by_id(&db, project.id)?.ok_or_else(|| {
            Error::internal(format!("project {} vanished after auto-index", project.id))
        })
    })
    .await
    .map_err(|err| Error::internal_with_source("session task join", err))?;

    match outcome {
        Ok(project) => {
            let (id, cancel) = shared.sessions.join(pid, cwd, Some(project.id));
            Ok((
                id,
                json!({
                    "session_id": id,
                    "project_id": project.id,
                    "name": project.name,
                    "status": project.status.as_str(),
                }),
                cancel,
            ))
        }
        Err(err) if allow_unbound => {
            let (id, cancel) = shared.sessions.join(pid, cwd, None);
            Ok((
                id,
                json!({
                    "session_id": id,
                    "project_id": null,
                    "name": null,
                    "status": "unregistered",
                    "hint": err.to_string(),
                }),
                cancel,
            ))
        }
        Err(err) => Err(err),
    }
}

async fn call_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: Value,
) -> Result<Value> {
    let shared = Arc::clone(shared);
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || route_tool(&shared, session_id, &name, &args))
        .await
        .map_err(|err| Error::internal_with_source("tool task join", err))?
}

fn route_tool(
    shared: &Arc<Shared>,
    session_id: Option<u64>,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let db = shared.db();
    match name {
        "status" => {
            let stats = shared.sessions.stats();
            tools::status(&db, &stats, shared.watchers.count())
        }
        "list_projects" => tools::list_projects(&db),
        "search_docs" => {
            let project = bound_project(shared, &db, session_id)?;
            tools::search_docs(&db, &shared.cache, &project, args)
        }
        "list_docs" => {
            let project = bound_project(shared, &db, session_id)?;
            tools::list_docs(&db, &project)
        }
        "index_project" => {
            drop(db);
            let path = match opt_str(args, "path")? {
                Some(raw) => {
                    let raw = PathBuf::from(raw);
                    if raw.is_absolute() {
                        raw
                    } else {
                        session_cwd(shared, session_id)?.join(raw)
                    }
                }
                None => session_cwd(shared, session_id)?,
            };
            let root = registry::project_root_for(&path, &shared.cache)?;
            let config = Config::load(Some(&root))?;
            let mut job_db = Db::open(&shared.cache)?;
            let project = registry::ensure_project(&mut job_db, &root)?;
            let (job_id, created) = tools::enqueue_sync(&job_db, &project)?;
            if created {
                tools::run_sync_job_in(&mut job_db, &shared.cache, job_id, &project, &config)?;
            }
            let fresh =
                registry::project_by_id(&job_db, project.id)?.ok_or_else(|| Error::Project {
                    message: format!("project {} vanished after indexing", project.id),
                    instruction: None,
                })?;
            if let Err(err) = shared.watchers.ensure(&shared.cache, &fresh) {
                eprintln!("warning: cannot watch project {}: {err}", fresh.id);
            }
            current_job(&job_db, job_id)
        }
        "sync_start" => {
            let project = match opt_i64(args, "project_id")? {
                Some(id) => registry::project_by_id(&db, id)?.ok_or_else(|| Error::Project {
                    message: format!("unknown project id {id}"),
                    instruction: Some("call list_projects".to_owned()),
                })?,
                None => bound_project(shared, &db, session_id)?,
            };
            // Fail fast on a broken project config instead of leaving a job
            // that would only error out in the background.
            Config::load(Some(&project.canonical_root))?;
            let (job_id, created) = tools::enqueue_sync(&db, &project)?;
            if created {
                spawn_sync_job(Arc::clone(shared), job_id, project.id);
            }
            current_job(&db, job_id)
        }
        "sync_status" => tools::sync_status(&db, args),
        other => Err(Error::internal(format!(
            "tool {other:?} is not implemented yet"
        ))),
    }
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(Error::Protocol {
            message: format!("{key} must be a string, got {other}"),
        }),
    }
}

fn opt_i64(args: &Value, key: &str) -> Result<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| Error::Protocol {
            message: format!("{key} must be an integer, got {value}"),
        }),
    }
}

fn current_job(db: &Db, job_id: i64) -> Result<Value> {
    let job = crate::store::repo::sync_job(db.connection(), job_id)?
        .ok_or_else(|| Error::internal(format!("sync job {job_id} vanished after enqueue")))?;
    Ok(tools::sync_job_value(&job))
}

fn spawn_sync_job(shared: Arc<Shared>, job_id: i64, project_id: i64) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || {
                match tools::execute_sync_job(&shared.cache, job_id, project_id) {
                    Ok(project) => {
                        if let Err(err) = shared.watchers.ensure(&shared.cache, &project) {
                            eprintln!("warning: cannot watch project {}: {err}", project.id);
                        }
                    }
                    Err(err) => eprintln!("warning: sync job {job_id} failed: {err}"),
                }
            });
        }
        Err(err) => {
            eprintln!("warning: sync job {job_id} not started (no runtime: {err})");
        }
    }
}

fn bound_project(
    shared: &Arc<Shared>,
    db: &Db,
    session_id: Option<u64>,
) -> Result<crate::store::models::Project> {
    let project_id = session_id
        .and_then(|id| shared.sessions.get(id))
        .and_then(|session| session.project_id)
        .ok_or_else(|| Error::Project {
            message: "session is not bound to a project".to_owned(),
            instruction: Some("register a session from your project directory".to_owned()),
        })?;
    registry::project_by_id(db, project_id)?.ok_or_else(|| Error::Project {
        message: format!("project {project_id} disappeared from the registry"),
        instruction: None,
    })
}

fn session_cwd(shared: &Arc<Shared>, session_id: Option<u64>) -> Result<PathBuf> {
    session_id
        .and_then(|id| shared.sessions.get(id))
        .map(|session| session.cwd)
        .ok_or_else(|| Error::Project {
            message: "session is not bound to a project".to_owned(),
            instruction: Some("index_project needs a path or a bound session".to_owned()),
        })
}

/// Keeps `in_flight` balanced even if request handling ever panics.
struct InFlightGuard<'a>(&'a AtomicUsize);

impl<'a> InFlightGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn hello_required() -> Response {
    error_response(&Error::Protocol {
        message: "Hello must be the first request".to_owned(),
    })
}

fn error_response(err: &Error) -> Response {
    Response::Error {
        code: err.mcp_code(),
        message: err.to_string(),
    }
}
