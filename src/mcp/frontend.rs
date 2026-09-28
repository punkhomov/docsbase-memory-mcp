//! stdio MCP server proxying tool calls to the daemon (FR-7, FR-34; I8).

use std::path::PathBuf;
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
use crate::ipc::client::Client;
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
            if guard.is_none() {
                let mut client = Client::connect(&cache)
                    .ok_or_else(|| Error::internal("daemon socket missing after ensure_daemon"))?;
                let cwd = std::env::current_dir()
                    .map_err(|err| Error::internal_with_source("resolve cwd", err))?;
                let project_hint = match client.handshake(&cwd) {
                    Ok(()) => None,
                    Err(err @ Error::Project { .. }) => {
                        let hint = err.to_string();
                        client.handshake_registry()?;
                        Some(hint)
                    }
                    Err(err) => return Err(err),
                };
                *guard = Some(Conn {
                    client,
                    project_hint,
                });
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
            conn.client.call_tool(&name, args)
        })
        .await
        .map_err(|err| Error::internal_with_source("mcp proxy join", err))?
    }
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
        if !tools::ALLOWED.contains(&name.as_str()) {
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
