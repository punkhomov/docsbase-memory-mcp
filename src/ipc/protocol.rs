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
        if let Self::CallTool { name, .. } = self
            && !TOOL_ALLOWLIST.contains(&name.as_str())
        {
            return Err(Error::Protocol {
                message: format!("unknown tool: {name}"),
            });
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
/// Returns [`Error::Protocol`] on malformed JSON or an invalid tool name.
pub fn decode_request(line: &[u8]) -> Result<Request> {
    let request: Request =
        serde_json::from_slice(line.trim_ascii_end()).map_err(|err| Error::Protocol {
            message: format!("parse request: {err}"),
        })?;
    request.validate()?;
    Ok(request)
}
