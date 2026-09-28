//! Wire types and framing: one JSON message per line (NDJSON).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Wire protocol version; a mismatch refuses the connection (FR-7).
pub const PROTOCOL_VERSION: u32 = 1;

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
    /// Proxied MCP tool call; `name` must be in [`TOOL_ALLOWLIST`].
    CallTool {
        /// Tool name.
        name: String,
        /// Tool arguments.
        args: serde_json::Value,
    },
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
    },
    /// Runtime counters (sessions, descriptors).
    Stats {
        /// Open file descriptors held by the daemon.
        fd_count: u64,
        /// Live sessions.
        sessions: u64,
    },
}

impl Request {
    /// Validates the request against local invariants (I8).
    ///
    /// # Errors
    /// Returns [`Error::Protocol`] when a `CallTool` name is not allowlisted.
    pub fn validate(&self) -> Result<()> {
        if let Self::CallTool { name, .. } = self {
            if !TOOL_ALLOWLIST.contains(&name.as_str()) {
                return Err(Error::Protocol {
                    message: format!("unknown tool: {name}"),
                });
            }
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
/// Returns [`Error::Protocol`] when serialization fails.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decodes one NDJSON line into a validated [`Request`].
///
/// # Errors
/// Returns [`Error::Protocol`] on malformed JSON or an invalid tool name.
pub fn decode_request(line: &[u8]) -> Result<Request> {
    let request: Request = serde_json::from_slice(trim_line(line))?;
    request.validate()?;
    Ok(request)
}

fn trim_line(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}
