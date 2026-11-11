//! HTTP DTOs. Generation is checked by the contract test.
use crate::ipc::panel::PanelInfo;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS)]
pub struct SessionView {
    pub version: String,
    pub poll_interval_ms: u64,
    pub initial_project: Option<i64>,
    pub daemon: PanelInfo,
}
#[derive(Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedupe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_per_doc: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}
#[derive(Serialize, Deserialize, TS)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

/// Deterministic generated declarations, no Node or filesystem access.
#[must_use]
pub fn typescript() -> String {
    use crate::ipc::panel::{ConfigField, ConfigView, ProjectView, SearchHit};
    let declarations = [
        SearchHit::decl(),
        SearchResponse::decl(),
        ProjectView::decl(),
        ConfigField::decl(),
        ConfigView::decl(),
        PanelInfo::decl(),
        SessionView::decl(),
        SearchRequest::decl(),
        ApiError::decl(),
    ];
    format!(
        "// Generated from Rust contracts. Do not edit.\n{}\n",
        declarations
            .map(|s| format!("export {s}"))
            .join("\n")
            .replace("bigint", "number")
    )
}

/// Same additive response shape as `search_docs`, with typed citations.
#[derive(Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum SearchResponse {
    Hits(Vec<crate::ipc::panel::SearchHit>),
    Explained {
        hits: Vec<crate::ipc::panel::SearchHit>,
        #[ts(type = "unknown")]
        meta: serde_json::Value,
    },
}
