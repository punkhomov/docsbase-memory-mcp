//! stdio MCP server proxying tool calls to the daemon (FR-7, FR-34; I8).

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{Value, json};

use crate::config::paths;
use crate::daemon::lifecycle::ensure_daemon;
use crate::error::Error;
use crate::ipc::client::{Client, is_transport_error};
use crate::ipc::protocol::{MAX_FRAME_BYTES, TOOL_ALLOWLIST};
use crate::mcp::tools;

struct Conn {
    client: Client,
    cwd: PathBuf,
    project_hint: Option<(String, Option<String>)>,
}

/// MCP frontend state shared across requests.
#[derive(Clone)]
pub struct Frontend {
    cache: PathBuf,
    conn: Arc<Mutex<Option<Conn>>>,
}

impl Frontend {
    /// Builds a frontend for `cache`.
    #[must_use]
    pub fn new(cache: PathBuf) -> Self {
        Self {
            cache,
            conn: Arc::new(Mutex::new(None)),
        }
    }

    async fn proxy(&self, name: &str, args: Value) -> Result<Value, Error> {
        let cache = self.cache.clone();
        let conn = Arc::clone(&self.conn);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || -> Result<Value, Error> {
            ensure_daemon(&cache)?;
            let mut guard = conn.lock().unwrap_or_else(PoisonError::into_inner);
            for attempt in 0..2 {
                if guard.is_none() {
                    *guard = Some(open_conn(&cache)?);
                }
                let conn = guard
                    .as_mut()
                    .ok_or_else(|| Error::internal("daemon connection lost"))?;
                if tools::PROJECT_TOOLS.contains(&name.as_str()) && conn.project_hint.is_some() {
                    // The project may have been indexed out-of-band (e.g.
                    // `docsbase index` in a terminal): retry the bind before
                    // serving the stale hint.
                    if conn.client.handshake(&conn.cwd).is_ok() {
                        conn.project_hint = None;
                    }
                }
                if tools::PROJECT_TOOLS.contains(&name.as_str())
                    && let Some((hint, instruction)) = &conn.project_hint
                {
                    return Err(Error::Project {
                        message: hint.clone(),
                        instruction: instruction.clone(),
                    });
                }
                let timeout = if tools::LONG_TOOLS.contains(&name.as_str()) {
                    tools::LONG_TIMEOUT
                } else {
                    tools::QUICK_TIMEOUT
                };
                let timeout_set = conn.client.set_read_timeout(Some(timeout));
                let result = match timeout_set {
                    Ok(()) => conn.client.call_tool(&name, args.clone()),
                    Err(err) => Err(err),
                };
                match result {
                    Err(err) if is_transport_error(&err) && attempt == 0 => {
                        // The daemon restarted underneath us: revive and retry once.
                        *guard = None;
                        ensure_daemon(&cache)?;
                    }
                    Ok(value) => {
                        if name == "index_project" {
                            // The cwd project is registered now: rebind so the
                            // same frontend can search without a restart.
                            rebind_after_index(&mut guard);
                        }
                        return Ok(value);
                    }
                    Err(err) => return Err(err),
                }
            }
            Err(Error::internal("daemon unreachable after reconnect"))
        })
        .await
        .map_err(|err| Error::internal_with_source("mcp proxy join", err))?
    }
}

fn open_conn(cache: &Path) -> Result<Conn, Error> {
    let mut client = Client::connect(cache)
        .ok_or_else(|| Error::internal("daemon socket missing after ensure_daemon"))?;
    let cwd =
        std::env::current_dir().map_err(|err| Error::internal_with_source("resolve cwd", err))?;
    let project_hint = match client.handshake(&cwd) {
        Ok(()) => None,
        Err(Error::Project {
            message,
            instruction,
        }) => {
            client.handshake_registry()?;
            client.register_unbound(&cwd)?;
            Some((message, instruction))
        }
        Err(err) => return Err(err),
    };
    Ok(Conn {
        client,
        cwd,
        project_hint,
    })
}

/// After a successful `index_project`, tries to bind the (previously
/// unbound) session to the now-registered project (FR-11, C8).
fn rebind_after_index(guard: &mut Option<Conn>) {
    let Some(conn) = guard.as_mut() else {
        return;
    };
    let Conn {
        client,
        cwd,
        project_hint,
    } = conn;
    if project_hint.is_some() && client.handshake(cwd).is_ok() {
        *project_hint = None;
    }
}

impl ServerHandler for Frontend {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "docsbase-memory-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Local-first documentation memory. Index a project with `index_project`, then search with `search_docs`.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> {
        std::future::ready(Ok(ListToolsResult {
            tools: tools::catalogue(),
            ..ListToolsResult::default()
        }))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !TOOL_ALLOWLIST.contains(&name.as_str()) {
            return Ok(tool_error(&Error::Protocol {
                message: format!("unknown tool: {name}"),
            })
            .into());
        }
        let args = request.arguments.map_or_else(|| json!({}), Value::Object);
        match self.proxy(&name, args).await {
            Ok(value) => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
                result.structured_content = Some(value);
                Ok(result.into())
            }
            Err(err) => Ok(tool_error(&err).into()),
        }
    }
}

/// Tool error result carrying the stable MCP code and, when available, the
/// actionable instruction (design §9).
fn tool_error(err: &Error) -> CallToolResult {
    let text = err.to_string();
    let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
    let mut structured = serde_json::Map::new();
    structured.insert("code".to_owned(), json!(err.mcp_code()));
    structured.insert("message".to_owned(), json!(err.inner_message()));
    if let Some(instruction) = err.instruction() {
        structured.insert("instruction".to_owned(), json!(instruction));
    }
    result.structured_content = Some(Value::Object(structured));
    result
}

/// Caps a single JSON-RPC frame before it reaches the MCP service (T45).
///
/// The agent-facing side is the untrusted one: a client that never sends a
/// newline must not grow the transport buffer without bound.
struct CappedRead<R> {
    inner: R,
    limit: usize,
    pending: usize,
}

impl<R> CappedRead<R> {
    fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            limit,
            pending: 0,
        }
    }
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CappedRead<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            for byte in &buf.filled()[before..] {
                if *byte == b'\n' {
                    this.pending = 0;
                } else {
                    this.pending += 1;
                }
            }
            if this.pending > this.limit {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "frame exceeds the protocol limit",
                )));
            }
        }
        result
    }
}

/// Runs the stdio MCP server until stdin closes.
///
/// # Errors
/// Returns an error when the runtime or MCP transport fails.
pub async fn run() -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    let service = Frontend::new(cache);
    let stdin = CappedRead::new(tokio::io::stdin(), MAX_FRAME_BYTES);
    let transport = rmcp::transport::async_rw::AsyncRwTransport::<RoleServer, _, _>::new_server(
        stdin,
        tokio::io::stdout(),
    );
    let running = rmcp::serve_server(service, transport)
        .await
        .map_err(|err| anyhow::anyhow!("mcp server init: {err}"))?;
    running
        .waiting()
        .await
        .map_err(|err| anyhow::anyhow!("mcp server: {err}"))?;
    Ok(())
}

/// Blocking entry point used by the CLI.
///
/// # Errors
/// Returns an error when the tokio runtime cannot start or the server fails.
pub fn run_blocking() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}
