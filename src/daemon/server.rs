//! IPC server: handshake, sessions and tool routing (FR-3, FR-7, FR-8, FR-12,
//! FR-33; I2, I6, I8).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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

/// Daemon state shared by all connections.
pub struct Shared {
    /// Cache root being served.
    pub cache: PathBuf,
    db: Mutex<Db>,
    /// Live sessions.
    pub sessions: SessionRegistry,
}

impl Shared {
    /// Opens the registry database and prepares shared state.
    ///
    /// # Errors
    /// Returns [`Error::Internal`]/[`Error::Admission`] from [`Db::open`].
    pub fn open(cache: &Path) -> Result<Arc<Self>> {
        let db = Db::open(cache)?;
        Ok(Arc::new(Self {
            cache: cache.to_path_buf(),
            db: Mutex::new(db),
            sessions: SessionRegistry::new(),
        }))
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Serves one connection until EOF, cleanup included (FR-8, I2).
pub async fn handle_connection(
    shared: Arc<Shared>,
    stream: UnixStream,
    connections: Arc<AtomicUsize>,
    events: mpsc::UnboundedSender<()>,
    shutdown: watch::Sender<bool>,
) {
    let (read_half, mut writer) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut session_id: Option<u64> = None;
    let mut hello_done = false;

    loop {
        let Ok(Some(line)) = lines.next_line().await else {
            break;
        };
        let (response, close) = handle_request(
            &shared,
            &shutdown,
            &mut hello_done,
            &mut session_id,
            line.as_bytes(),
        )
        .await;
        let Ok(bytes) = protocol::encode(&response) else {
            break;
        };
        if writer.write_all(&bytes).await.is_err() {
            break;
        }
        if close {
            break;
        }
    }

    if let Some(id) = session_id {
        shared.sessions.leave(id);
    }
    connections.fetch_sub(1, Ordering::SeqCst);
    let _ = events.send(());
}

async fn handle_request(
    shared: &Arc<Shared>,
    shutdown: &watch::Sender<bool>,
    hello_done: &mut bool,
    session_id: &mut Option<u64>,
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
                let err = Error::Admission {
                    message: format!(
                        "build {build_id:?} != {:?} (I1); restart the frontend",
                        protocol::build_id()
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
            match register_session(shared, pid, &cwd).await {
                Ok((id, value)) => {
                    if let Some(previous) = session_id.replace(id) {
                        shared.sessions.leave(previous);
                    }
                    (Response::ToolResult { value }, false)
                }
                Err(err) => (error_response(&err), false),
            }
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
            let _ = shutdown.send(true);
            (Response::ToolResult { value: Value::Null }, false)
        }
    }
}

async fn register_session(shared: &Arc<Shared>, pid: u32, cwd: &Path) -> Result<(u64, Value)> {
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
    .map_err(|err| Error::internal_with_source("session task join", err))??;

    let id = shared.sessions.join(pid, cwd, Some(outcome.id));
    Ok((
        id,
        json!({
            "session_id": id,
            "project_id": outcome.id,
            "name": outcome.name,
            "status": outcome.status.as_str(),
        }),
    ))
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
            tools::status(&db, stats.sessions, stats.fd_count)
        }
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
            let session_cwd = session_cwd(shared, session_id)?;
            let path = match args.get("path").and_then(Value::as_str) {
                Some(raw) => {
                    let raw = PathBuf::from(raw);
                    if raw.is_absolute() {
                        raw
                    } else {
                        session_cwd.join(raw)
                    }
                }
                None => session_cwd,
            };
            let root = registry::project_root_for(&path, &shared.cache)?;
            let config = Config::load(Some(&root))?;
            let mut job_db = Db::open(&shared.cache)?;
            let project = registry::ensure_project(&mut job_db, &root)?;
            let stats = tools::run_project_index(&mut job_db, &shared.cache, &project, &config)?;
            Ok(json!({ "project_id": project.id, "stats": stats }))
        }
        other => Err(Error::internal(format!(
            "tool {other:?} is not implemented yet"
        ))),
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
