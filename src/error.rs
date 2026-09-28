//! Error taxonomy (design §9): stable categories mapped to MCP error codes.

use std::path::{Path, PathBuf};

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Crate-wide error. One variant per category from the design's error table.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Build/schema/cache-root mismatch; refuse before doing work.
    #[error("admission refused: {message}")]
    Admission { message: String },
    /// IPC protocol violation (version, framing, unknown tool).
    #[error("protocol error: {message}")]
    Protocol { message: String },
    /// Project is not registered or a path is outside its roots.
    #[error("project error: {message}{}", instruction_hint(.instruction.as_deref()))]
    Project {
        message: String,
        instruction: Option<String>,
    },
    /// A single document failed; the job continues.
    #[error("index error{}: {message}", path_suffix(.path.as_deref()))]
    Index {
        path: Option<PathBuf>,
        message: String,
    },
    /// Invalid or empty query.
    #[error("query error: {message}")]
    Query { message: String },
    /// Frontend↔daemon transport failure (socket/framing), never a daemon
    /// error answer; callers may reconnect and retry.
    #[error("daemon transport: {message}")]
    Transport {
        message: String,
        /// Underlying socket failure kept for logs.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Anything else; masked to the client.
    #[error("internal error: {message}")]
    Internal {
        message: String,
        /// Underlying failure kept for logs; never serialized to clients.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl Error {
    /// Transport failure without an underlying socket error.
    #[must_use]
    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport {
            message: message.into(),
            source: None,
        }
    }

    /// Transport failure that keeps `source` for diagnostics.
    #[must_use]
    pub fn transport_with_source(
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Transport {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Internal error without an underlying failure.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            source: None,
        }
    }

    /// Internal error that keeps `source` for diagnostics (`Error::source`).
    #[must_use]
    pub fn internal_with_source(
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Internal {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Rebuilds a category error from an MCP code sent by the daemon.
    #[must_use]
    pub fn from_mcp_code(code: i32, message: String) -> Self {
        match code {
            -32010 => Self::Admission { message },
            -32011 => Self::Protocol { message },
            -32012 => Self::Project {
                message,
                instruction: None,
            },
            -32013 => Self::Index {
                path: None,
                message,
            },
            -32014 => Self::Query { message },
            _ => Self::internal(message),
        }
    }

    /// Stable JSON-RPC/MCP error code for the category (design §9).
    #[must_use]
    pub fn mcp_code(&self) -> i32 {
        match self {
            Error::Admission { .. } => -32010,
            Error::Protocol { .. } => -32011,
            Error::Project { .. } => -32012,
            Error::Index { .. } => -32013,
            Error::Query { .. } => -32014,
            Error::Transport { .. } | Error::Internal { .. } => -32603,
        }
    }
}

fn path_suffix(path: Option<&Path>) -> String {
    path.map_or_else(String::new, |p| format!(" in {}", p.display()))
}

fn instruction_hint(instruction: Option<&str>) -> String {
    instruction.map_or_else(String::new, |hint| format!(" (hint: {hint})"))
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::internal_with_source(err.to_string(), err)
    }
}
