//! Tool catalogue exposed over MCP (FR-20…FR-27, I8).

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool};
use serde_json::{Value, json};

/// Tools that need a session bound to a registered project (I6).
pub const PROJECT_TOOLS: &[&str] = &["search_docs", "get_doc", "read_neighbors", "list_docs"];

/// Tools that may run for the full indexing budget (NFR-1); `sync_start` and
/// `sync_status` only enqueue/read job rows and stay quick.
pub const LONG_TOOLS: &[&str] = &["index_project"];

/// Default IPC read timeout for fast tools.
pub const QUICK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// IPC read timeout for long-running tools.
pub const LONG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// MCP tool definitions with JSON input schemas.
#[must_use]
pub fn catalogue() -> Vec<Tool> {
    vec![
        tool(
            "search_docs",
            "Search indexed documentation chunks; returns citations (path, heading, lines, score).",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max hits (default 10)." },
                    "scope": { "type": "string", "enum": ["project"], "description": "Reserved; v1 supports only the current project." }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "get_doc",
            "Return a full document from the project root by relative path.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
        tool(
            "read_neighbors",
            "Return chunks adjacent to the given chunk id for progressive disclosure.",
            json!({
                "type": "object",
                "properties": {
                    "chunk_id": { "type": "integer", "description": "Chunk id from search_docs." },
                    "before": { "type": "integer", "minimum": 0, "description": "Earlier chunks (default 1)." },
                    "after": { "type": "integer", "minimum": 0, "description": "Later chunks (default 1)." }
                },
                "required": ["chunk_id"]
            }),
        ),
        tool(
            "list_docs",
            "List indexed documents with title, size and chunk count; keyset-paginated.",
            json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "description": "Page size (default 50)." },
                    "cursor": { "type": "string", "description": "next_cursor from the previous page." }
                }
            }),
        ),
        tool(
            "list_projects",
            "List registered projects and their indexing status.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "index_project",
            "Register and index a project (defaults to the session's directory).",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }),
        ),
        tool(
            "sync_start",
            "Start (or return the already active) full synchronization job; returns its job_id.",
            json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "integer", "description": "Defaults to the session's project." }
                }
            }),
        ),
        tool(
            "sync_status",
            "Report the persistent state and statistics of a sync job.",
            json!({
                "type": "object",
                "properties": { "job_id": { "type": "integer" } },
                "required": ["job_id"]
            }),
        ),
        tool(
            "status",
            "Registry-wide status: projects, documents, chunks, sessions, watcher and versions.",
            json!({ "type": "object", "properties": {} }),
        ),
    ]
}

fn tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    Tool::new(name, description, schema_object(schema))
}

fn schema_object(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(JsonObject::new()),
    }
}
