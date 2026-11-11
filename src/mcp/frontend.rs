//! stdio MCP server proxying tool calls to the daemon (FR-7, FR-34; I8).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::config::bootstrap::DefaultsContext;
use crate::config::schema::SettingsSpec;
use crate::config::{Config, paths};
use crate::daemon::lifecycle::ensure_daemon;
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::ipc::async_client::{AsyncClient, within_operation};
use crate::ipc::client::is_transport_error;
use crate::ipc::protocol::{MAX_FRAME_BYTES, TOOL_ALLOWLIST};
use crate::mcp::tools;
use crate::search::operation::SearchOperation;
use crate::search::settings::{SearchSettings, SearchSpec};

struct Conn {
    client: AsyncClient,
    cwd: PathBuf,
    project_hint: Option<(String, Option<String>)>,
}

/// MCP frontend state shared across requests.
#[derive(Clone)]
pub struct Frontend {
    cache: PathBuf,
    conn: Arc<Mutex<Option<Conn>>>,
    search_policy: SearchSettings,
}

impl Frontend {
    /// Compatibility constructor using the search owner's explicit defaults.
    /// Normal startup passes its parsed global policy to `with_search_policy`.
    #[must_use]
    pub fn new(cache: PathBuf) -> Self {
        let defaults = SearchSpec::defaults(&DefaultsContext::standalone());
        Self::with_search_policy(cache, SearchSpec::initial(&defaults))
    }

    /// Builds with complete owner policy; each search validates it before IO.
    #[must_use]
    pub fn with_search_policy(cache: PathBuf, search_policy: SearchSettings) -> Self {
        Self {
            cache,
            conn: Arc::new(Mutex::new(None)),
            search_policy,
        }
    }

    #[cfg(test)]
    async fn proxy(&self, name: &str, args: Value) -> Result<Value> {
        self.proxy_cancellable(name, args, Instant::now(), std::future::pending())
            .await
    }

    async fn proxy_cancellable(
        &self,
        name: &str,
        args: Value,
        started: Instant,
        cancelled: impl Future<Output = ()>,
    ) -> Result<Value> {
        let operation = if name == "search_docs" {
            Some(SearchOperation::new(
                &self.search_policy,
                started,
                Cancellation::new(),
            )?)
        } else {
            None
        };
        run_cancellable(
            operation.as_ref(),
            self.proxy_in(name, args, operation.as_ref()),
            cancelled,
        )
        .await
    }

    async fn proxy_in(
        &self,
        name: &str,
        args: Value,
        operation: Option<&SearchOperation>,
    ) -> Result<Value> {
        ensure_using(&self.cache, operation, ensure_daemon).await?;
        self.proxy_ready(name, args, operation).await
    }

    async fn proxy_ready(
        &self,
        name: &str,
        args: Value,
        operation: Option<&SearchOperation>,
    ) -> Result<Value> {
        let queued = Instant::now();
        let mut guard = self.conn.lock().await;
        if let Some(operation) = operation {
            operation.record_queue(queued.elapsed());
        }
        check_operation(operation)?;
        // Only this future owns the active connection. Cancellation leaves None
        // for the next caller and cannot expose a partially consumed exchange.
        let mut lease = guard.take();
        for attempt in 0..2 {
            check_operation(operation)?;
            let result = async {
                if lease.is_none() {
                    lease = Some(
                        legacy_budget(operation, tools::QUICK_TIMEOUT, open_conn(&self.cache))
                            .await?,
                    );
                }
                let conn = lease
                    .as_mut()
                    .ok_or_else(|| Error::internal("daemon connection lost"))?;
                let timeout = if tools::LONG_TOOLS.contains(&name) {
                    tools::LONG_TIMEOUT
                } else {
                    tools::QUICK_TIMEOUT
                };
                legacy_budget(
                    operation,
                    timeout,
                    call_bound(conn, name, args.clone(), operation),
                )
                .await
            }
            .await;
            match result {
                Err(error) if is_transport_error(&error) && attempt == 0 => {
                    drop(lease.take());
                    check_operation(operation)?;
                    ensure_using(&self.cache, operation, ensure_daemon).await?;
                }
                result => {
                    let mut healthy = !matches!(
                        &result,
                        Err(Error::Transport { .. }
                            | Error::Admission { .. }
                            | Error::Protocol { .. })
                    );
                    if result.is_ok() && name == "index_project" {
                        let conn = lease
                            .as_mut()
                            .ok_or_else(|| Error::internal("completed index lost connection"))?;
                        healthy = legacy_budget(
                            operation,
                            tools::QUICK_TIMEOUT,
                            rebind_after_index(conn),
                        )
                        .await
                        .is_ok();
                    }
                    if healthy {
                        *guard = lease;
                    }
                    return result;
                }
            }
        }
        Err(Error::internal("daemon unreachable after reconnect"))
    }
}

fn run_cancellable<T>(
    operation: Option<&SearchOperation>,
    work: impl Future<Output = Result<T>>,
    cancelled: impl Future<Output = ()>,
) -> impl Future<Output = Result<T>> {
    // Box before constructing the async state to keep MCP service futures small.
    let work = Box::pin(work);
    async move {
        let request = async {
            match operation {
                Some(operation) => within_operation(operation, work).await,
                None => work.await,
            }
        };
        tokio::select! {
            biased;
            () = cancelled => {
                if let Some(operation) = operation { operation.cancellation().cancel(); }
                Err(Error::Admission { message: "MCP request cancelled".to_owned() })
            }
            result = request => result,
        }
    }
}

fn check_operation(operation: Option<&SearchOperation>) -> Result<()> {
    match operation {
        Some(operation) => operation.check(),
        None => Ok(()),
    }
}

async fn ensure_using(
    cache: &Path,
    operation: Option<&SearchOperation>,
    startup: impl FnOnce(&Path) -> Result<()> + Send + 'static,
) -> Result<()> {
    check_operation(operation)?;
    let cache = cache.to_path_buf();
    let worker_operation = operation.cloned();
    let queued = Instant::now();
    tokio::task::spawn_blocking(move || {
        if let Some(operation) = &worker_operation {
            operation.record_queue(queued.elapsed());
            operation.check()?;
        }
        startup(&cache)
    })
    .await
    .map_err(|error| Error::internal_with_source("MCP daemon startup join", error))??;
    check_operation(operation)
}

async fn legacy_budget<T>(
    operation: Option<&SearchOperation>,
    timeout: Duration,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    if operation.is_some() {
        return work.await;
    }
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Error::transport("timed out waiting for the daemon"))?
}

async fn open_conn(cache: &Path) -> Result<Conn> {
    let mut client = AsyncClient::connect(cache)
        .await
        .map_err(|error| Error::transport_with_source("connect daemon after startup", error))?;
    let cwd = std::env::current_dir()
        .map_err(|error| Error::internal_with_source("resolve cwd", error))?;
    let project_hint = match client.handshake(&cwd).await {
        Ok(()) => None,
        Err(Error::Project {
            message,
            instruction,
        }) => {
            client.handshake_registry().await?;
            client.register_unbound(&cwd).await?;
            Some((message, instruction))
        }
        Err(error) => return Err(error),
    };
    Ok(Conn {
        client,
        cwd,
        project_hint,
    })
}

async fn call_bound(
    conn: &mut Conn,
    name: &str,
    args: Value,
    operation: Option<&SearchOperation>,
) -> Result<Value> {
    if tools::PROJECT_TOOLS.contains(&name) && conn.project_hint.is_some() {
        match conn.client.handshake(&conn.cwd).await {
            Ok(()) => conn.project_hint = None,
            Err(Error::Project {
                message,
                instruction,
            }) => conn.project_hint = Some((message, instruction)),
            Err(error) => return Err(error),
        }
    }
    if tools::PROJECT_TOOLS.contains(&name)
        && let Some((hint, instruction)) = &conn.project_hint
    {
        return Err(Error::Project {
            message: hint.clone(),
            instruction: instruction.clone(),
        });
    }
    match operation {
        Some(operation) => conn.client.search(args, operation).await,
        None => conn.client.call_tool(name, args).await,
    }
}

async fn rebind_after_index(conn: &mut Conn) -> Result<()> {
    if conn.project_hint.is_some() {
        match conn.client.handshake(&conn.cwd).await {
            Ok(()) => conn.project_hint = None,
            Err(
                error
                @ (Error::Transport { .. } | Error::Admission { .. } | Error::Protocol { .. }),
            ) => return Err(error),
            Err(_) => {}
        }
    }
    Ok(())
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
    ) -> impl std::future::Future<Output = std::result::Result<ListToolsResult, McpError>> {
        std::future::ready(Ok(ListToolsResult {
            tools: tools::catalogue(),
            ..ListToolsResult::default()
        }))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let name = request.name.to_string();
        if !TOOL_ALLOWLIST.contains(&name.as_str()) {
            return Ok(tool_error(&Error::Protocol {
                message: format!("unknown tool: {name}"),
            })
            .into());
        }
        let args = request.arguments.map_or_else(|| json!({}), Value::Object);
        match self
            .proxy_cancellable(&name, args, started, context.ct.cancelled())
            .await
        {
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
                    if this.pending > this.limit {
                        return Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "frame exceeds the protocol limit",
                        )));
                    }
                }
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
    let service = Frontend::with_search_policy(cache, Config::global()?.search);
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

#[cfg(test)]
#[path = "frontend_frame_tests.rs"]
mod frame_tests;

#[cfg(test)]
#[path = "frontend_proxy_tests.rs"]
mod proxy_tests;
