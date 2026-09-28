//! Repository helpers for documents and chunks (FR-16).

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// Identity and hash of an already indexed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocState {
    /// Row id.
    pub id: i64,
    /// Path relative to the project root.
    pub rel_path: String,
    /// Hash recorded by the last successful run.
    pub content_hash: String,
}

/// Fields written when a document is indexed (new or changed).
#[derive(Debug)]
pub struct NewDoc<'a> {
    /// Path relative to the project root.
    pub rel_path: &'a str,
    /// Absolute path used for reads.
    pub abs_path: &'a str,
    /// Title from frontmatter or the first heading.
    pub title: Option<&'a str>,
    /// Raw frontmatter serialized as JSON.
    pub frontmatter_json: Option<&'a str>,
    /// File size in bytes.
    pub size: i64,
    /// File mtime (Unix seconds).
    pub mtime: i64,
    /// Content hash used for incremental indexing.
    pub content_hash: &'a str,
    /// Unix timestamp of this index run.
    pub indexed_at: i64,
}

/// One chunk row to persist.
#[derive(Debug)]
pub struct NewChunk<'a> {
    /// Sequence within the document.
    pub seq: u32,
    /// Breadcrumb `H1 > H2 > H3`.
    pub heading_path: &'a str,
    /// `prose` / `code` / `table`.
    pub kind: &'a str,
    /// Code fence language when `kind = code`.
    pub lang: Option<&'a str>,
    /// First line (1-based, inclusive).
    pub line_start: u32,
    /// Last line (1-based, inclusive).
    pub line_end: u32,
    /// Chunk text.
    pub text: &'a str,
}

/// Loads `(id, rel_path, hash)` for every document of `project_id`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn doc_states(conn: &Connection, project_id: i64) -> Result<Vec<DocState>> {
    let mut stmt = conn
        .prepare("SELECT id, rel_path, content_hash FROM docs WHERE project_id = ?1")
        .map_err(db_error)?;
    let rows = stmt
        .query_map([project_id], |row| {
            Ok(DocState {
                id: row.get(0)?,
                rel_path: row.get(1)?,
                content_hash: row.get(2)?,
            })
        })
        .map_err(db_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)
}

/// Invalidates the stored hash of a document slated for removal (R2).
///
/// A crash after the tantivy purge but before the SQLite delete must not let a
/// rerun skip a restored file whose bytes still match the old hash.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn invalidate_doc(conn: &Connection, doc_id: i64) -> Result<()> {
    conn.execute("UPDATE docs SET content_hash = '' WHERE id = ?1", [doc_id])
        .map_err(db_error)?;
    Ok(())
}

/// Inserts an in-flight document row (R2) and returns its SQLite row id.
///
/// Allocation happens inside the write transaction, so concurrent index jobs
/// (different projects, same registry) cannot compute the same id.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn insert_pending_doc(
    conn: &Connection,
    project_id: i64,
    rel_path: &str,
    abs_path: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO docs (project_id, rel_path, abs_path, title, frontmatter_json,
                           size, mtime, content_hash, indexed_at)
         VALUES (?1, ?2, ?3, NULL, NULL, 0, 0, '', 0)",
        params![project_id, rel_path, abs_path],
    )
    .map_err(db_error)?;
    Ok(conn.last_insert_rowid())
}

/// Inserts or updates one document row.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn upsert_doc(conn: &Connection, project_id: i64, doc_id: i64, doc: &NewDoc<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO docs (id, project_id, rel_path, abs_path, title, frontmatter_json,
                           size, mtime, content_hash, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
             project_id = excluded.project_id,
             rel_path = excluded.rel_path,
             abs_path = excluded.abs_path,
             title = excluded.title,
             frontmatter_json = excluded.frontmatter_json,
             size = excluded.size,
             mtime = excluded.mtime,
             content_hash = excluded.content_hash,
             indexed_at = excluded.indexed_at",
        params![
            doc_id,
            project_id,
            doc.rel_path,
            doc.abs_path,
            doc.title,
            doc.frontmatter_json,
            doc.size,
            doc.mtime,
            doc.content_hash,
            doc.indexed_at
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

/// Deletes all chunk rows of `doc_id` and inserts `chunks`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn replace_chunks(conn: &Connection, doc_id: i64, chunks: &[NewChunk<'_>]) -> Result<()> {
    conn.execute("DELETE FROM chunks WHERE doc_id = ?1", [doc_id])
        .map_err(db_error)?;
    let mut stmt = conn
        .prepare(
            "INSERT INTO chunks (doc_id, seq, heading_path, kind, lang, line_start, line_end, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(db_error)?;
    for chunk in chunks {
        stmt.execute(params![
            doc_id,
            chunk.seq,
            chunk.heading_path,
            chunk.kind,
            chunk.lang,
            chunk.line_start,
            chunk.line_end,
            chunk.text
        ])
        .map_err(db_error)?;
    }
    Ok(())
}

/// Deletes a document row; `chunks` rows follow via `ON DELETE CASCADE`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn delete_doc(conn: &Connection, doc_id: i64) -> Result<()> {
    conn.execute("DELETE FROM docs WHERE id = ?1", [doc_id])
        .map_err(db_error)?;
    Ok(())
}

pub(crate) fn db_error(err: rusqlite::Error) -> Error {
    Error::internal_with_source(format!("sqlite: {err}"), err)
}

/// Citation metadata for one stored chunk (FR-20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// Path relative to the project root.
    pub path: String,
    /// Breadcrumb `H1 > H2 > H3`.
    pub heading_path: String,
    /// First line (1-based, inclusive).
    pub line_start: u32,
    /// Last line (1-based, inclusive).
    pub line_end: u32,
}

/// Looks up citation metadata for `(doc_id, seq)`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn citation_for(conn: &Connection, doc_id: i64, seq: u32) -> Result<Option<Citation>> {
    conn.query_row(
        "SELECT d.rel_path, c.heading_path, c.line_start, c.line_end
         FROM chunks c JOIN docs d ON d.id = c.doc_id
         WHERE c.doc_id = ?1 AND c.seq = ?2",
        params![doc_id, seq],
        |row| {
            Ok(Citation {
                path: row.get(0)?,
                heading_path: row.get(1)?,
                line_start: row.get(2)?,
                line_end: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(db_error)
}

/// One `docsbase list` row (FR-25 snapshot view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocOverview {
    /// Path relative to the project root.
    pub path: String,
    /// Title from frontmatter or the first heading.
    pub title: Option<String>,
    /// File size in bytes.
    pub size: i64,
    /// Chunk count for this document.
    pub chunks: i64,
}

/// Lists documents of `project_id` ordered by path.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn docs_overview(conn: &Connection, project_id: i64) -> Result<Vec<DocOverview>> {
    let mut stmt = conn
        .prepare(
            "SELECT d.rel_path, d.title, d.size,
                    (SELECT COUNT(*) FROM chunks c WHERE c.doc_id = d.id)
             FROM docs d WHERE d.project_id = ?1 ORDER BY d.rel_path",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map([project_id], |row| {
            Ok(DocOverview {
                path: row.get(0)?,
                title: row.get(1)?,
                size: row.get(2)?,
                chunks: row.get(3)?,
            })
        })
        .map_err(db_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)
}

/// Aggregate document/chunk counts for one project (FR-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectCounts {
    /// Indexed documents.
    pub docs: i64,
    /// Indexed chunks.
    pub chunks: i64,
}

/// Counts documents and chunks of `project_id`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn project_counts(conn: &Connection, project_id: i64) -> Result<ProjectCounts> {
    conn.query_row(
        "SELECT (SELECT COUNT(*) FROM docs WHERE project_id = ?1),
                (SELECT COUNT(*) FROM chunks c JOIN docs d ON d.id = c.doc_id
                 WHERE d.project_id = ?1)",
        [project_id],
        |row| {
            Ok(ProjectCounts {
                docs: row.get(0)?,
                chunks: row.get(1)?,
            })
        },
    )
    .map_err(db_error)
}
