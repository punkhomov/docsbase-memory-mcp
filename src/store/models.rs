//! Row models for the registry and index state (requirements §7).

use std::path::PathBuf;

/// Lifecycle status of a registered project (FR-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    /// Registered, never indexed.
    NotIndexed,
    /// Indexing job is running.
    Indexing,
    /// Index is up to date.
    Indexed,
    /// Last job failed; see `status` output.
    Error,
}

impl ProjectStatus {
    /// Stable string stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotIndexed => "not_indexed",
            Self::Indexing => "indexing",
            Self::Indexed => "indexed",
            Self::Error => "error",
        }
    }

    /// Parses the stored string form.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_indexed" => Some(Self::NotIndexed),
            "indexing" => Some(Self::Indexing),
            "indexed" => Some(Self::Indexed),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Registered project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Row id.
    pub id: i64,
    /// Canonical project root (realpath + git root).
    pub canonical_root: PathBuf,
    /// Display name.
    pub name: String,
    /// Current lifecycle status.
    pub status: ProjectStatus,
    /// Schema version recorded at registration.
    pub schema_version: u32,
    /// Unix timestamp of registration.
    pub created_at: i64,
    /// Unix timestamp of the last successful index.
    pub last_indexed_at: Option<i64>,
}

/// Indexed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// Row id.
    pub id: i64,
    /// Owning project.
    pub project_id: i64,
    /// Path relative to the project root.
    pub rel_path: String,
    /// Absolute path used for reads.
    pub abs_path: String,
    /// Title from frontmatter or first heading.
    pub title: Option<String>,
    /// Raw frontmatter serialized as JSON.
    pub frontmatter_json: Option<String>,
    /// File size in bytes.
    pub size: u64,
    /// File mtime (Unix seconds).
    pub mtime: i64,
    /// Content hash used for incremental indexing.
    pub content_hash: String,
    /// Unix timestamp of the last index of this document.
    pub indexed_at: i64,
}

/// Chunk metadata (text lives in tantivy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkMeta {
    /// Row id.
    pub id: i64,
    /// Owning document.
    pub doc_id: i64,
    /// Sequence within the document.
    pub seq: u32,
    /// Breadcrumb `H1 > H2 > H3`.
    pub heading_path: String,
    /// `prose` / `code` / `table`.
    pub kind: String,
    /// Code fence language when `kind = code`.
    pub lang: Option<String>,
    /// First line (1-based, inclusive).
    pub line_start: u32,
    /// Last line (1-based, inclusive).
    pub line_end: u32,
}

/// Persistent sync job (FR-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncJob {
    /// Row id.
    pub id: i64,
    /// Owning project.
    pub project_id: i64,
    /// `queued` / `running` / `done` / `error`.
    pub state: String,
    /// Unix timestamp of creation.
    pub started_at: i64,
    /// Unix timestamp of completion.
    pub finished_at: Option<i64>,
    /// Serialized statistics.
    pub stats_json: Option<String>,
}
