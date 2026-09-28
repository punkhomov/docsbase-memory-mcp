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
    #[error("project error: {message}")]
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
    /// Anything else; masked to the client.
    #[error("internal error: {message}")]
    Internal { message: String },
}

impl Error {
    /// Stable JSON-RPC/MCP error code for the category (design §9).
    #[must_use]
    pub fn mcp_code(&self) -> i32 {
        match self {
            Error::Admission { .. } => -32010,
            Error::Protocol { .. } => -32011,
            Error::Project { .. } => -32012,
            Error::Index { .. } => -32013,
            Error::Query { .. } => -32014,
            Error::Internal { .. } => -32603,
        }
    }
}

fn path_suffix(path: Option<&Path>) -> String {
    path.map_or_else(String::new, |p| format!(" in {}", p.display()))
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Internal {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Protocol {
            message: err.to_string(),
        }
    }
}
