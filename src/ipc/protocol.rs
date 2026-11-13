//! Wire types and framing: one JSON message per line (NDJSON).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Wire protocol version; a mismatch refuses the connection (FR-7).
pub const PROTOCOL_VERSION: u32 = 1;

/// Opt-in search lifetime semantics; fixed protocol identity, not a timeout.
pub const SEARCH_CONTEXT_CAPABILITY: &str = "search-context-v1";

/// Explicit literal interpretation; old peers may otherwise ignore the argument.
pub const PANEL_READ_CAPABILITY: &str = "panel-read-v1";

/// Literal query interpretation.
pub const LITERAL_QUERY_CAPABILITY: &str = "literal-query-v1";

pub(crate) fn ensure_search_query_mode(
    args: &serde_json::Value,
    capabilities: &[String],
) -> Result<()> {
    let mode = crate::index::QueryMode::from_argument(args.get("query_mode"))?;
    if mode == crate::index::QueryMode::Literal
        && !capabilities
            .iter()
            .any(|capability| capability == LITERAL_QUERY_CAPABILITY)
    {
        return Err(Error::Protocol {
            message: format!(
                "daemon does not advertise {LITERAL_QUERY_CAPABILITY}; restart the daemon and frontend"
            ),
        });
    }
    Ok(())
}

/// Marks an ACTIVE fixed lexical Website daemon, not mere implementation support.
/// Absent for Shared. Additive Hello marker; wire shape and required SDK caps stay unchanged.
pub const ACTIVE_WEBSITE_CAPABILITY: &str = "active-website-profile-v1";

/// Durations from one caller's original operation, never fresh stage budgets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCallContext {
    /// Remaining portion of the caller's original budget.
    pub remaining_ms: u64,
    /// Time already spent before sending this request.
    pub elapsed_ms: u64,
    /// Admission delay already included in elapsed time.
    pub queue_ms: u64,
}

impl SearchCallContext {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.remaining_ms == 0 {
            return Err(Error::Admission {
                message: "search request deadline expired before dispatch".to_owned(),
            });
        }
        if self.queue_ms > self.elapsed_ms {
            return Err(Error::Admission {
                message: "scoped search queue exceeds original elapsed time".to_owned(),
            });
        }
        Ok(())
    }
}

/// Maximum accepted NDJSON/JSON-RPC frame on the daemon, client and
/// frontend stdin wires (I-2; T45).
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Reserve for the JSON envelope around a `get_doc` payload so that a
/// response of `Config::max_file_size` bytes still fits [`MAX_FRAME_BYTES`].
pub const FRAME_OVERHEAD: u64 = 64 * 1024;

/// Tools the frontend may proxy to the daemon (I8, FR-34).
pub const TOOL_ALLOWLIST: &[&str] = &[
    "search_docs",
    "get_doc",
    "read_neighbors",
    "list_docs",
    "list_projects",
    "index_project",
    "sync_start",
    "sync_status",
    "status",
    "panel_info",
    "panel_config",
    "panel_projects",
];

/// Frontend -> daemon messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    /// Handshake: versions and build identity.
    Hello {
        /// Peer protocol version.
        protocol_version: u32,
        /// Build identity of the sender.
        build_id: String,
        /// Human-readable client name.
        client: String,
    },
    /// Registers a live session and binds it to a project by working directory.
    RegisterSession {
        /// Frontend process id.
        pid: u32,
        /// Frontend working directory.
        cwd: PathBuf,
    },
    /// Registers a live session without a project binding; used by the
    /// frontend fallback so `index_project` can still default to the cwd
    /// (C8).
    RegisterUnbound {
        /// Frontend process id.
        pid: u32,
        /// Frontend working directory.
        cwd: PathBuf,
    },
    /// Proxied MCP tool call; `name` must be in [`TOOL_ALLOWLIST`].
    CallTool {
        /// Tool name.
        name: String,
        /// Tool arguments.
        args: serde_json::Value,
    },
    /// One cancellable search; EOF abandons this caller and calls do not multiplex.
    Search {
        /// Search arguments, identical to `search_docs`.
        args: serde_json::Value,
        /// Explicit original budget and upstream admission measurements.
        context: SearchCallContext,
    },
    /// Nonmutating session binding for registered projects or registry keepalive.
    BindReadOnly { pid: u32, project_id: Option<i64> },
    /// Graceful daemon shutdown request.
    StopDaemon,
}

/// Daemon -> frontend messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Response {
    /// Handshake reply.
    Hello {
        /// Daemon protocol version.
        protocol_version: u32,
        /// Daemon build identity.
        build_id: String,
        /// Database schema version.
        schema_version: u32,
        /// Additive protocol support; an old daemon advertises none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    /// Successful tool result.
    ToolResult {
        /// Tool-specific payload.
        value: serde_json::Value,
    },
    /// Error reply; `code` follows the design §9 taxonomy.
    Error {
        /// MCP error code.
        code: i32,
        /// Human-readable message.
        message: String,
        /// Actionable instruction, when the category carries one.
        #[serde(default)]
        instruction: Option<String>,
    },
}

impl Request {
    /// Validates the request against local invariants (I8).
    ///
    /// # Errors
    /// Returns [`Error::Protocol`] when a `CallTool` name is not allowlisted, or
    /// [`Error::Admission`] when a `Search` scoped budget has zero `remaining_ms`
    /// or `queue_ms` greater than `elapsed_ms`. Malformed or missing wire fields
    /// are rejected as [`Error::Protocol`] by [`decode_request`] before validation.
    pub fn validate(&self) -> Result<()> {
        if let Self::CallTool { name, .. } = self
            && !TOOL_ALLOWLIST.contains(&name.as_str())
        {
            return Err(Error::Protocol {
                message: format!("unknown tool: {name}"),
            });
        }
        if let Self::Search { context, .. } = self {
            context.validate()?;
        }
        Ok(())
    }
}

/// Checks that the peer speaks the same protocol version.
///
/// # Errors
/// Returns [`Error::Protocol`] on mismatch.
pub fn ensure_protocol_version(remote: u32) -> Result<()> {
    if remote != PROTOCOL_VERSION {
        return Err(Error::Protocol {
            message: format!("protocol version {remote} != supported {PROTOCOL_VERSION}"),
        });
    }
    Ok(())
}

/// Encodes one message as a single NDJSON line.
///
/// # Errors
/// Returns [`Error::Internal`] when serialization fails (a bug in the caller's
/// message type, not a wire problem).
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(message)
        .map_err(|err| Error::internal(format!("encode message: {err}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Build identity advertised in `Hello` and checked at admission (I1).
#[must_use]
pub fn build_id() -> String {
    format!("docsbase {}", env!("CARGO_PKG_VERSION"))
}

/// Decodes one NDJSON line into a [`Response`].
///
/// # Errors
/// Returns [`Error::Protocol`] on malformed JSON.
pub fn decode_response(line: &[u8]) -> Result<Response> {
    serde_json::from_slice(line.trim_ascii_end()).map_err(|err| Error::Protocol {
        message: format!("parse response: {err}"),
    })
}

/// Decodes one NDJSON line into a validated [`Request`].
///
/// # Errors
/// Returns [`Error::Protocol`] on malformed JSON or an invalid tool name, or
/// [`Error::Admission`] when an explicit scoped budget is invalid/expired.
pub fn decode_request(line: &[u8]) -> Result<Request> {
    let request: Request =
        serde_json::from_slice(line.trim_ascii_end()).map_err(|err| Error::Protocol {
            message: format!("parse request: {err}"),
        })?;
    request.validate()?;
    Ok(request)
}
