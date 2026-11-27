//! stdio MCP server proxying tool calls to the daemon (FR-7, FR-34; I8).

use std::future::{Future, poll_fn};
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
use crate::ipc::protocol::{
    ACTIVE_WEBSITE_CAPABILITY, CHUNK_REF_CAPABILITY, MAX_FRAME_BYTES, TOOL_ALLOWLIST,
    ensure_config_version,
};
use crate::ipc::settings::{ClientSettings, ClientSpec};
use crate::mcp::tools;
use crate::search::operation::SearchOperation;
use crate::search::settings::{SearchSettings, SearchSpec};

struct Conn {
    client: AsyncClient,
    cwd: PathBuf,
    config_v2: Option<bool>,
    project_hint: Option<(String, Option<String>)>,
}

/// MCP frontend state shared across requests.
#[derive(Clone)]
pub struct Frontend {
    cache: PathBuf,
    transport: crate::platform::transport::Transport,
    conn: Arc<Mutex<Option<Conn>>>,
    search_policy: SearchSettings,
    client_settings: ClientSettings,
    config_v2: Option<bool>,
}

impl Frontend {
    /// Compatibility constructor using the search owner's explicit defaults.
    /// Normal startup passes the resolved global configuration.
    #[must_use]
    pub fn new(transport: crate::platform::transport::Transport, cache: PathBuf) -> Self {
        let defaults = SearchSpec::defaults(&DefaultsContext::standalone());
        Self::with_search_policy(transport, cache, SearchSpec::initial(&defaults))
    }

    /// Builds with complete owner policy; each search validates it before IO.
    #[must_use]
    pub fn with_search_policy(
        transport: crate::platform::transport::Transport,
        cache: PathBuf,
        search_policy: SearchSettings,
    ) -> Self {
        Self::with_policies(transport, cache, search_policy, ClientSpec::DEFAULTS, None)
    }

    fn with_policies(
        transport: crate::platform::transport::Transport,
        cache: PathBuf,
        search_policy: SearchSettings,
        client_settings: ClientSettings,
        config_v2: Option<bool>,
    ) -> Self {
        Self {
            cache,
            transport,
            conn: Arc::new(Mutex::new(None)),
            search_policy,
            client_settings,
            config_v2,
        }
    }

    fn for_config(
        transport: crate::platform::transport::Transport,
        cache: PathBuf,
        config: Config,
    ) -> Self {
        Self::with_policies(
            transport,
            cache,
            config.search,
            config.client,
            Some(config.vector_store.is_some()),
        )
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
        } else if name == "ask_docs" {
            Some(SearchOperation::with_timeout(
                started,
                crate::ask::AskLimits::TIMEOUT,
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
                    let open = open_conn(&self.transport, &self.cache, self.config_v2);
                    lease = Some(if let Some(operation) = operation {
                        open_for_search(operation, open).await?
                    } else {
                        let deadline =
                            tokio::time::Instant::now() + self.client_settings.mcp_quick_timeout();
                        open_before(open, deadline, tokio::time::sleep_until(deadline)).await?
                    });
                }
                let conn = lease
                    .as_mut()
                    .ok_or_else(|| Error::internal("daemon connection lost"))?;
                let timeout = if tools::LONG_TOOLS.contains(&name) {
                    self.client_settings.mcp_long_timeout()
                } else {
                    self.client_settings.mcp_quick_timeout()
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
                Err(error) if is_transport_error(&error) && attempt == 0 && name != "ask_docs" => {
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
                            self.client_settings.mcp_quick_timeout(),
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

async fn open_for_search<T>(
    operation: &SearchOperation,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(work);
    poll_fn(|cx| {
        operation.check()?;
        work.as_mut().poll(cx)
    })
    .await
}

async fn open_before<T>(
    work: impl Future<Output = Result<T>>,
    deadline: tokio::time::Instant,
    expiry: impl Future<Output = ()>,
) -> Result<T> {
    tokio::pin!(work, expiry);
    poll_fn(|cx| {
        // Capacity can wake the task before the timer driver observes the deadline.
        if expiry.as_mut().poll(cx).is_ready() || tokio::time::Instant::now() >= deadline {
            return Poll::Ready(Err(Error::transport("timed out waiting for the daemon")));
        }
        work.as_mut().poll(cx)
    })
    .await
}

async fn open_conn(
    transport: &crate::platform::transport::Transport,
    cache: &Path,
    config_v2: Option<bool>,
) -> Result<Conn> {
    #[cfg(windows)]
    let client = AsyncClient::connect_with_capacity(transport, cache).await;
    #[cfg(unix)]
    let client = AsyncClient::connect(transport, cache).await;
    let mut client = client
        .map_err(|error| Error::transport_with_source("connect daemon after startup", error))?;
    let cwd = std::env::current_dir()
        .map_err(|error| Error::internal_with_source("resolve cwd", error))?;
    let project_hint = match bind_project(&mut client, &cwd, config_v2).await {
        Ok(()) => None,
        Err(Error::Project {
            message,
            instruction,
        }) => {
            negotiate_config(&mut client, config_v2).await?;
            client.register_unbound(&cwd).await?;
            Some((message, instruction))
        }
        Err(error) => return Err(error),
    };
    Ok(Conn {
        client,
        cwd,
        config_v2,
        project_hint,
    })
}

async fn negotiate_config(client: &mut AsyncClient, config_v2: Option<bool>) -> Result<()> {
    client.handshake_registry().await?;
    if let Some(expected) = config_v2 {
        ensure_config_version(client.capabilities(), expected)?;
    }
    Ok(())
}

async fn bind_project(client: &mut AsyncClient, cwd: &Path, config_v2: Option<bool>) -> Result<()> {
    negotiate_config(client, config_v2).await?;
    let capabilities = client.capabilities();
    if [ACTIVE_WEBSITE_CAPABILITY, CHUNK_REF_CAPABILITY]
        .into_iter()
        .all(|expected| capabilities.iter().any(|value| value == expected))
    {
        return bind_catalog_website(client, cwd).await;
    }
    client.register_session(cwd).await
}

// Website's list is already confined to the fixed project. Resolve the
// frontend cwd before asking the daemon to bind that existing ID read-only.
async fn bind_catalog_website(client: &mut AsyncClient, cwd: &Path) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct FixedProject {
        id: i64,
        root: PathBuf,
    }

    let listed = client.call_tool("list_projects", json!({})).await?;
    let invalid = || Error::Protocol {
        message: "invalid fixed Website project from the daemon".into(),
    };
    let [project]: [FixedProject; 1] = serde_json::from_value(listed).map_err(|_| invalid())?;
    if project.id <= 0 || !project.root.is_absolute() {
        return Err(invalid());
    }
    let cwd = cwd.to_owned();
    let cwd = tokio::task::spawn_blocking(move || cwd.canonicalize())
        .await
        .map_err(|error| Error::internal_with_source("Website project binding join", error))?
        .map_err(|error| Error::internal_with_source("resolve MCP cwd", error))?;
    // Both paths are canonical. Keep the case of the selected filesystem entries.
    if !cwd.starts_with(&project.root) {
        return Err(Error::Admission {
            message: "MCP working directory is outside the fixed Website project".into(),
        });
    }
    client.bind_read_only(Some(project.id)).await
}

async fn call_bound(
    conn: &mut Conn,
    name: &str,
    args: Value,
    operation: Option<&SearchOperation>,
) -> Result<Value> {
    if tools::PROJECT_TOOLS.contains(&name) && conn.project_hint.is_some() {
        match bind_project(&mut conn.client, &conn.cwd, conn.config_v2).await {
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
        Some(operation) if name == "search_docs" => conn.client.search(args, operation).await,
        _ => conn.client.call_tool(name, args).await,
    }
}

async fn rebind_after_index(conn: &mut Conn) -> Result<()> {
    if conn.project_hint.is_some() {
        match bind_project(&mut conn.client, &conn.cwd, conn.config_v2).await {
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
pub async fn run(transport: crate::platform::transport::Transport) -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    serve_stdio(Frontend::for_config(transport, cache, Config::global()?)).await
}

async fn serve_stdio(service: Frontend) -> anyhow::Result<()> {
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
    let config = Config::global()?;
    let policy = crate::ipc::transport::policy(config.daemon.max_connections, &config.daemon)?;
    crate::platform::transport::run(&policy, move |transport| {
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let service = Frontend::for_config(transport, paths::cache_dir()?, config);
            runtime.block_on(serve_stdio(service))
        })();
        Ok(result)
    })?
}

#[cfg(test)]
#[path = "frontend_frame_tests.rs"]
mod frame_tests;

#[cfg(test)]
#[path = "frontend_proxy_tests.rs"]
mod proxy_tests;
