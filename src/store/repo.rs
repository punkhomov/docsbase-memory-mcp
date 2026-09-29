//! Repository helpers for documents and chunks (FR-16).

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::store::models::{SyncJob, SyncState};

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

/// Deletes every document (chunks included) of `project`; used when the
/// tantivy index was recreated after a schema upgrade and must be rebuilt
/// from scratch (T27).
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn delete_project_docs(conn: &Connection, project_id: i64) -> Result<usize> {
    conn.execute(
        "DELETE FROM chunks WHERE doc_id IN (SELECT id FROM docs WHERE project_id = ?1)",
        [project_id],
    )
    .map_err(db_error)?;
    conn.execute("DELETE FROM docs WHERE project_id = ?1", [project_id])
        .map_err(db_error)
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

/// Inserts a `queued` sync job unless the project already has an active one
/// (FR-17: at most one queued/running job per project).
///
/// Guard and insert are a single SQL statement, so two concurrent callers
/// cannot both enqueue.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn create_sync_job(conn: &Connection, project_id: i64) -> Result<Option<i64>> {
    let inserted = conn
        .execute(
            "INSERT INTO sync_jobs (project_id, state, started_at)
             SELECT ?1, ?2, ?3
             WHERE NOT EXISTS (
                 SELECT 1 FROM sync_jobs
                 WHERE project_id = ?1 AND state IN (?2, ?4)
             )",
            params![
                project_id,
                SyncState::Queued.as_str(),
                unix_now(),
                SyncState::Running.as_str()
            ],
        )
        .map_err(db_error)?;
    Ok((inserted == 1).then(|| conn.last_insert_rowid()))
}

/// Newest active (`queued`/`running`) job of `project_id`.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures or a corrupt state value.
pub fn active_sync_job(conn: &Connection, project_id: i64) -> Result<Option<SyncJob>> {
    sync_job_query(
        conn,
        "SELECT id, project_id, state, started_at, finished_at, stats_json
         FROM sync_jobs
         WHERE project_id = ?1 AND state IN ('queued', 'running')
         ORDER BY id DESC LIMIT 1",
        params![project_id],
    )
}

/// Loads one sync job by id.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures or a corrupt state value.
pub fn sync_job(conn: &Connection, id: i64) -> Result<Option<SyncJob>> {
    sync_job_query(
        conn,
        "SELECT id, project_id, state, started_at, finished_at, stats_json
         FROM sync_jobs WHERE id = ?1",
        params![id],
    )
}

/// Moves a `queued` job to `running` (FR-17); only the `queued` state is
/// accepted, so a finished job is never resurrected.
///
/// # Errors
/// Returns [`Error::Internal`] for an unknown or non-queued id and on SQLite
/// failures.
pub fn mark_sync_running(conn: &Connection, id: i64) -> Result<()> {
    let updated = conn
        .execute(
            "UPDATE sync_jobs SET state = ?1 WHERE id = ?2 AND state = ?3",
            params![SyncState::Running.as_str(), id, SyncState::Queued.as_str()],
        )
        .map_err(db_error)?;
    if updated == 0 {
        return Err(Error::internal(format!(
            "sync job {id} is unknown or not queued"
        )));
    }
    Ok(())
}

/// Records a terminal job state with its serialized statistics (FR-17);
/// only `queued`/`running` jobs can be finished.
///
/// # Errors
/// Returns [`Error::Internal`] for an unknown or already finished id and on
/// SQLite failures.
pub fn finish_sync_job(
    conn: &Connection,
    id: i64,
    state: SyncState,
    stats_json: Option<&str>,
) -> Result<()> {
    let updated = conn
        .execute(
            "UPDATE sync_jobs SET state = ?1, finished_at = ?2, stats_json = ?3
             WHERE id = ?4 AND state IN (?5, ?6)",
            params![
                state.as_str(),
                unix_now(),
                stats_json,
                id,
                SyncState::Queued.as_str(),
                SyncState::Running.as_str()
            ],
        )
        .map_err(db_error)?;
    if updated == 0 {
        return Err(Error::internal(format!(
            "sync job {id} is unknown or already finished"
        )));
    }
    Ok(())
}

/// Marks jobs left `queued`/`running` by a previous daemon as `error`
/// (their executor is gone); run once at daemon start.
///
/// # Errors
/// Returns [`Error::Internal`] on SQLite failures.
pub fn fail_orphan_sync_jobs(conn: &Connection) -> Result<usize> {
    conn.execute(
        "UPDATE sync_jobs SET state = ?1, finished_at = ?2, stats_json = ?3
         WHERE state IN (?4, ?5)",
        params![
            SyncState::Error.as_str(),
            unix_now(),
            serde_json::json!({ "orphaned": true }).to_string(),
            SyncState::Queued.as_str(),
            SyncState::Running.as_str()
        ],
    )
    .map_err(db_error)
}

type SyncRow = (i64, i64, String, i64, Option<i64>, Option<String>);

fn sync_job_query(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<SyncJob>> {
    let row = conn
        .query_row(sql, params, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()
        .map_err(db_error)?;
    row.map(to_sync_job).transpose()
}

fn to_sync_job(row: SyncRow) -> Result<SyncJob> {
    let (id, project_id, state, started_at, finished_at, stats_json) = row;
    let state = SyncState::from_str(&state)
        .map_err(|err| Error::internal_with_source(format!("sync job {id}: {err}"), err))?;
    Ok(SyncJob {
        id,
        project_id,
        state,
        started_at,
        finished_at,
        stats_json,
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}
