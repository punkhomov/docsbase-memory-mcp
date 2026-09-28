//! Tool catalogue exposed over MCP (FR-20…FR-27, I8).

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool};
use serde_json::{Value, json};

/// Frontend allowlist mirroring the daemon IPC allowlist (I8, FR-34).
pub const ALLOWED: &[&str] = &[
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

/// Tools that need a session bound to a registered project (I6).
pub const PROJECT_TOOLS: &[&str] = &["search_docs", "get_doc", "read_neighbors", "list_docs"];

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
            "Return chunks adjacent to the given chunk id.",
            json!({
                "type": "object",
                "properties": { "chunk_id": { "type": "integer" } },
                "required": ["chunk_id"]
            }),
        ),
        tool(
            "list_docs",
            "List indexed documents with title, size and chunk count.",
            json!({ "type": "object", "properties": {} }),
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
            "Start a full synchronization job for the current project.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "sync_status",
            "Report progress of the current project's sync job.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "status",
            "Registry-wide status: projects, documents, chunks, sessions.",
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
