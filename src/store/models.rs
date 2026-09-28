//! Row models for the registry and index state (requirements §7).

use std::path::PathBuf;
use std::str::FromStr;

/// Error returned when a stored enum value is not recognized.
#[derive(Debug, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct ParseEnumError {
    /// Which enum was being parsed.
    pub kind: &'static str,
    /// The unrecognized stored value.
    pub value: String,
}

impl ParseEnumError {
    fn new(kind: &'static str, value: &str) -> Self {
        Self {
            kind,
            value: value.to_owned(),
        }
    }
}

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
}

impl FromStr for ProjectStatus {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "not_indexed" => Ok(Self::NotIndexed),
            "indexing" => Ok(Self::Indexing),
            "indexed" => Ok(Self::Indexed),
            "error" => Ok(Self::Error),
            _ => Err(ParseEnumError::new("project status", value)),
        }
    }
}

/// Chunk content kind stored in `chunks.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Regular markdown prose.
    Prose,
    /// Fenced code block.
    Code,
    /// Markdown table.
    Table,
}

impl ChunkKind {
    /// Stable string stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Code => "code",
            Self::Table => "table",
        }
    }
}

impl FromStr for ChunkKind {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "prose" => Ok(Self::Prose),
            "code" => Ok(Self::Code),
            "table" => Ok(Self::Table),
            _ => Err(ParseEnumError::new("chunk kind", value)),
        }
    }
}

/// Lifecycle state of a sync job stored in `sync_jobs.state` (FR-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Created, not started.
    Queued,
    /// Currently running.
    Running,
    /// Finished successfully.
    Done,
    /// Finished with an error.
    Error,
}

impl SyncState {
    /// Stable string stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

impl FromStr for SyncState {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            _ => Err(ParseEnumError::new("sync state", value)),
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
    /// Content kind.
    pub kind: ChunkKind,
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
    /// Job lifecycle state.
    pub state: SyncState,
    /// Unix timestamp of creation.
    pub started_at: i64,
    /// Unix timestamp of completion.
    pub finished_at: Option<i64>,
    /// Serialized statistics.
    pub stats_json: Option<String>,
}
