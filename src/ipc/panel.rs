//! Safe, additive read-only panel contracts. No dependency on the HTTP module.
use serde::{Deserialize, Serialize};

/// Registry row without raw diagnostics or provider credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct ProjectView {
    pub id: i64,
    pub name: String,
    pub root: String,
    pub status: String,
    pub docs: i64,
    pub chunks: i64,
    pub watched: bool,
    pub cached_vectors: Option<u64>,
    pub vector_chunks: Option<u64>,
    pub ann_state: Option<String>,
}
/// Explicitly selected non-secret configuration fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct ConfigField {
    pub group: String,
    pub name: String,
    pub value: String,
    pub source: Option<String>,
}
/// Effective project configuration, safe before crossing IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct ConfigView {
    pub fields: Vec<ConfigField>,
    pub search_mode: String,
    pub reranker: bool,
}
/// Daemon-owned search limits and advertised panel protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct PanelInfo {
    pub capabilities: Vec<String>,
    pub request_timeout_ms: u64,
    pub max_request_payload_bytes: u64,
    pub max_retained_payload_bytes: u64,
    pub dedupe_max_candidates: usize,
    pub max_hits: usize,
}
impl ConfigView {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        let mut fields = Vec::new();
        let mut add = |group: &str, name: &str, value: String| {
            fields.push(ConfigField {
                group: group.into(),
                name: name.into(),
                value,
                source: None,
            });
        };
        add("Index", "max_file_size", config.max_file_size.to_string());
        add(
            "Index",
            "max_docs_per_project",
            config.max_docs_per_project.to_string(),
        );
        add("Index", "auto_index", config.auto_index.to_string());
        add("Index", "ignores", config.ignores.join(", "));
        add(
            "Search",
            "request_timeout_ms",
            config.search.request_timeout_ms.to_string(),
        );
        add("Search", "hybrid", config.hybrid.to_string());
        if let Some(embed) = &config.hybrid_cfg.embedding {
            add("Embedding", "provider", embed.provider.clone());
            add("Embedding", "model", embed.model.clone());
            add("Embedding", "dimensions", embed.dimensions.to_string());
        }
        if let Some(rerank) = &config.hybrid_cfg.reranker {
            add("Reranker", "provider", rerank.provider.clone());
            add("Reranker", "model", rerank.model.clone());
            add("Reranker", "window", rerank.window.to_string());
        }
        Self {
            fields,
            search_mode: if config.hybrid
                && config.hybrid_cfg.ranking == crate::hybrid::Ranking::Rrf
            {
                "hybrid"
            } else {
                "lexical"
            }
            .into(),
            reranker: config.hybrid && config.hybrid_cfg.reranker.is_some(),
        }
    }
}

/// Search citation shared by daemon and HTTP contracts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct SearchHit {
    pub chunk_id: u64,
    pub path: String,
    pub heading_path: String,
    pub lines: [u32; 2],
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "web", ts(optional))]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "web", ts(optional))]
    pub snippet_truncated: Option<bool>,
}

/// Fixed Website project identity observed from authoritative metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProject {
    pub id: i64,
    pub name: String,
    pub root: String,
    pub status: String,
}

/// Result of one read-only index compatibility/health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexHealth {
    Healthy,
    IndexUnavailable,
    RebuildRequired,
}

/// Stable recovery category; status never performs the recovery itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    StateChanged,
    Indexing,
    Interrupted,
    Failed,
    Partial,
    SyncFailed,
    IndexUnavailable,
    RebuildRequired,
    UnknownOutcome,
    IncompleteScope,
    RecipeChanged,
}

/// Latest/active durable Sync identity and strict actual lexical counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSync {
    pub job_id: i64,
    pub state: String,
    pub attempt_id: Option<i64>,
    pub stats: Option<crate::index::job::JobStats>,
}

/// Versioned bounded observation of the active fixed Website corpus.
/// Ready describes the indexed corpus/recipe, not filesystem freshness or
/// availability during later in-place mutation. It does not pin a generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub schema_version: u32,
    pub profile: String,
    pub project: ServerProject,
    pub index_generation: i64,
    pub index_attempt: i64,
    pub index_health: IndexHealth,
    pub outcome: Option<crate::index::outcome::IndexOutcome>,
    pub active_sync: Option<ServerSync>,
    pub latest_sync: Option<ServerSync>,
    pub ready: bool,
    pub reason: Option<ReadinessReason>,
    pub restart_required: bool,
    pub notice: Option<String>,
}
