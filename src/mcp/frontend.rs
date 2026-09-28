//! stdio MCP server proxying tool calls to the daemon (FR-7, FR-34; I8).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{Value, json};

use crate::config::paths;
use crate::daemon::lifecycle::ensure_daemon;
use crate::error::Error;
use crate::ipc::client::{Client, is_transport_error};
use crate::ipc::protocol::TOOL_ALLOWLIST;
use crate::mcp::tools;

struct Conn {
    client: Client,
    project_hint: Option<String>,
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
                if tools::PROJECT_TOOLS.contains(&name.as_str()) {
                    if let Some(hint) = &conn.project_hint {
                        return Err(Error::Project {
                            message: hint.clone(),
                            instruction: None,
                        });
                    }
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
                    other => return other,
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
            let hint =
                instruction.map_or(message.clone(), |hint| format!("{message} (hint: {hint})"));
            client.handshake_registry()?;
            Some(hint)
        }
        Err(err) => return Err(err),
    };
    Ok(Conn {
        client,
        project_hint,
    })
}

impl ServerHandler for Frontend {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
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
    ) -> Result<CallToolResult, McpError> {
        let name = request.name.to_string();
        if !TOOL_ALLOWLIST.contains(&name.as_str()) {
            return Ok(tool_error(format!("unknown tool: {name}")));
        }
        let args = request.arguments.map_or_else(|| json!({}), Value::Object);
        match self.proxy(&name, args).await {
            Ok(value) => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
                result.structured_content = Some(value);
                Ok(result)
            }
            Err(err) => Ok(tool_error(err.to_string())),
        }
    }
}

fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Runs the stdio MCP server until stdin closes.
///
/// # Errors
/// Returns an error when the runtime or MCP transport fails.
pub async fn run() -> anyhow::Result<()> {
    let cache = paths::cache_dir()?;
    let service = Frontend::new(cache);
    let running = rmcp::serve_server(service, rmcp::transport::io::stdio())
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
