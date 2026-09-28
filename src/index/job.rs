//! Full index job: walk → hash → diff → tantivy + SQLite (FR-16, FR-18; A4; R2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::chunk::{Chunk, ChunkKind, chunk_markdown};
use crate::index::frontmatter::{self, Frontmatter};
use crate::index::tantivy_index::IndexHandle;
use crate::index::walk;
use crate::store::Db;
use crate::store::models::{ChunkKind as StoredChunkKind, Project};
use crate::store::repo::{self, DocState, NewChunk, NewDoc};

/// Longest section body kept in one chunk when no paragraph boundary splits it.
pub const MAX_CHUNK_CHARS: usize = 4_000;

/// Outcome of one indexing run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobStats {
    /// New or changed documents written.
    pub docs: usize,
    /// Chunks added to tantivy and SQLite.
    pub chunks: usize,
    /// Documents skipped because the content hash matched.
    pub skipped: usize,
    /// Documents removed because the file is gone.
    pub removed: usize,
    /// Files skipped non-fatally (IO/parse/size/limit problems, A4).
    pub errors: usize,
}

#[derive(Debug)]
struct Pending {
    id: i64,
    rel_path: String,
    abs_path: String,
    title: Option<String>,
    frontmatter_json: Option<String>,
    size: i64,
    mtime: i64,
    content_hash: String,
    chunks: Vec<Chunk>,
}

/// Indexes `project` fully: every `.md` under its root is hashed and only
/// new/changed files are re-chunked; documents whose files disappeared are
/// purged. Per-file failures are counted in [`JobStats::errors`] and never
/// abort the job (A4).
///
/// Commit order is tantivy first, SQLite second (R2). Before touching tantivy
/// every planned document is marked in-flight (empty `content_hash`), so a
/// crash between the commits makes the next run reprocess the document
/// instead of trusting a stale hash.
///
/// # Errors
/// Returns errors for walk setup failures and SQLite/tantivy failures that
/// affect the whole job.
pub fn run_full(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    let existing = repo::doc_states(db.connection(), project.id)?;
    let by_path: HashMap<&str, &DocState> = existing
        .iter()
        .map(|state| (state.rel_path.as_str(), state))
        .collect();

    let mut stats = JobStats::default();
    let mut plan: Vec<Pending> = Vec::new();
    let mut walked: HashSet<String> = HashSet::new();

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut walk_errors = 0_usize;
    for entry in walk::walk(&project.canonical_root, config)? {
        match entry {
            Ok(path) => paths.push(path),
            Err(_) => walk_errors += 1,
        }
    }
    stats.errors += walk_errors;
    paths.sort();

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for path in paths {
        match path.strip_prefix(&project.canonical_root) {
            Ok(rel) => {
                let rel = rel.to_string_lossy().into_owned();
                walked.insert(rel.clone());
                files.push((path, rel));
            }
            Err(_) => stats.errors += 1,
        }
    }

    // An incomplete walk cannot prove absence: skip purging when any entry
    // failed, otherwise one unreadable directory would drop its whole subtree.
    let removed: Vec<&DocState> = if walk_errors == 0 {
        existing
            .iter()
            .filter(|state| !walked.contains(&state.rel_path))
            .collect()
    } else {
        Vec::new()
    };

    let mut next_id = repo::next_doc_id(db.connection())?;
    let mut new_budget = config
        .max_docs_per_project
        .saturating_sub(existing.len().saturating_sub(removed.len()));

    for (path, rel) in &files {
        let previous = by_path.get(rel.as_str()).copied();
        match prepare_doc(path, rel, previous, config) {
            Ok(Some(mut prepared)) => {
                if previous.is_none() {
                    if new_budget == 0 {
                        stats.errors += 1;
                        continue;
                    }
                    new_budget -= 1;
                }
                let doc_id = previous.map_or(next_id, |state| state.id);
                if previous.is_none() {
                    next_id += 1;
                }
                prepared.id = doc_id;
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(_) => stats.errors += 1,
        }
    }

    finish_plan(db, index, project.id, &mut plan, &removed, &mut stats)?;
    Ok(stats)
}

/// Incrementally reindexes only `changed` paths (FR-16): created/modified files
/// are hashed and reindexed when needed, missing files are purged. Paths
/// outside the project root and unknown deletions count as warnings (A4).
///
/// # Errors
/// Same as [`run_full`].
pub fn run_incremental(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
) -> Result<JobStats> {
    run_incremental_with(db, index, project, changed, &Config::default())
}

/// [`run_incremental`] with explicit limits for watcher/CLI callers.
///
/// # Errors
/// Same as [`run_full`].
pub fn run_incremental_with(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
) -> Result<JobStats> {
    let existing = repo::doc_states(db.connection(), project.id)?;
    let by_path: HashMap<&str, &DocState> = existing
        .iter()
        .map(|state| (state.rel_path.as_str(), state))
        .collect();

    let mut stats = JobStats::default();
    let mut plan: Vec<Pending> = Vec::new();
    let mut removed: Vec<&DocState> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut next_id = repo::next_doc_id(db.connection())?;
    let mut new_budget = config.max_docs_per_project.saturating_sub(existing.len());

    for path in changed {
        let Ok(rel) = path.strip_prefix(&project.canonical_root) else {
            stats.errors += 1;
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        if !seen.insert(rel.clone()) {
            continue;
        }
        let previous = by_path.get(rel.as_str()).copied();
        if !path.exists() {
            match previous {
                Some(state) => removed.push(state),
                None => stats.errors += 1,
            }
            continue;
        }
        match prepare_doc(path, &rel, previous, config) {
            Ok(Some(mut prepared)) => {
                if previous.is_none() {
                    if new_budget == 0 {
                        stats.errors += 1;
                        continue;
                    }
                    new_budget -= 1;
                }
                let doc_id = previous.map_or(next_id, |state| state.id);
                if previous.is_none() {
                    next_id += 1;
                }
                prepared.id = doc_id;
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(_) => stats.errors += 1,
        }
    }

    finish_plan(db, index, project.id, &mut plan, &removed, &mut stats)?;
    Ok(stats)
}

fn finish_plan(
    db: &mut Db,
    index: &mut IndexHandle,
    project_id: i64,
    plan: &mut [Pending],
    removed: &[&DocState],
    stats: &mut JobStats,
) -> Result<()> {
    mark_pending(db, project_id, plan, removed)?;

    for pending in plan.iter_mut() {
        index.delete_doc(pending.id);
        for chunk in &mut pending.chunks {
            chunk.doc_id = pending.id;
        }
        index.add_chunks(&pending.chunks)?;
        stats.docs += 1;
        stats.chunks += pending.chunks.len();
    }
    for state in removed {
        index.delete_doc(state.id);
    }
    stats.removed = removed.len();

    apply_plan(db, index, project_id, plan, removed)
}

fn mark_pending(
    db: &mut Db,
    project_id: i64,
    plan: &[Pending],
    removed: &[&DocState],
) -> Result<()> {
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))?;
    for pending in plan {
        repo::mark_pending(
            &tx,
            project_id,
            pending.id,
            &pending.rel_path,
            &pending.abs_path,
        )?;
    }
    for state in removed {
        repo::invalidate_doc(&tx, state.id)?;
    }
    tx.commit()
        .map_err(|err| Error::internal_with_source(format!("commit sqlite: {err}"), err))
}

/// Reads and hashes one walked file. Returns `Ok(None)` when the stored hash
/// matches, `Err` for per-file problems the caller counts as a warning (A4).
fn prepare_doc(
    path: &Path,
    rel_path: &str,
    existing: Option<&DocState>,
    config: &Config,
) -> Result<Option<Pending>> {
    let metadata = std::fs::metadata(path).map_err(|err| file_error(path, &err))?;
    if metadata.len() > config.max_file_size {
        return Err(Error::Index {
            path: Some(path.to_path_buf()),
            message: format!(
                "file is {} bytes, over max_file_size {}",
                metadata.len(),
                config.max_file_size
            ),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|err| file_error(path, &err))?;
    let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    if existing
        .is_some_and(|state| !state.content_hash.is_empty() && state.content_hash == content_hash)
    {
        return Ok(None);
    }

    let (meta, body) = frontmatter::parse(&text);
    let line_shift = frontmatter_line_shift(&text, body);
    let mut chunks = chunk_markdown(body, MAX_CHUNK_CHARS);
    for chunk in &mut chunks {
        chunk.line_start = chunk.line_start.saturating_add(line_shift);
        chunk.line_end = chunk.line_end.saturating_add(line_shift);
    }
    let title = meta
        .title
        .clone()
        .or_else(|| chunks.iter().find_map(|c| c.heading_path.first().cloned()));
    Ok(Some(Pending {
        id: 0,
        rel_path: rel_path.to_owned(),
        abs_path: path.to_string_lossy().into_owned(),
        title,
        frontmatter_json: serialize_metadata(&meta)?,
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime: mtime_seconds(&metadata),
        content_hash,
        chunks,
    }))
}

fn apply_plan(
    db: &mut Db,
    index: &mut IndexHandle,
    project_id: i64,
    plan: &[Pending],
    removed: &[&DocState],
) -> Result<()> {
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))?;
    let now = unix_now();

    for pending in plan {
        let doc = NewDoc {
            rel_path: &pending.rel_path,
            abs_path: &pending.abs_path,
            title: pending.title.as_deref(),
            frontmatter_json: pending.frontmatter_json.as_deref(),
            size: pending.size,
            mtime: pending.mtime,
            content_hash: &pending.content_hash,
            indexed_at: now,
        };
        repo::upsert_doc(&tx, project_id, pending.id, &doc)?;

        let paths: Vec<String> = pending
            .chunks
            .iter()
            .map(|chunk| chunk.heading_path.join(" > "))
            .collect();
        let rows: Vec<NewChunk<'_>> = pending
            .chunks
            .iter()
            .zip(&paths)
            .map(|(chunk, heading_path)| {
                let (kind, lang) = split_kind(&chunk.kind);
                NewChunk {
                    seq: chunk.seq,
                    heading_path,
                    kind: kind.as_str(),
                    lang,
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    text: &chunk.text,
                }
            })
            .collect();
        repo::replace_chunks(&tx, pending.id, &rows)?;
    }
    for state in removed {
        repo::delete_doc(&tx, state.id)?;
    }

    index.commit()?;
    tx.commit()
        .map_err(|err| Error::internal_with_source(format!("commit sqlite: {err}"), err))
}

fn split_kind(kind: &ChunkKind) -> (StoredChunkKind, Option<&str>) {
    match kind {
        ChunkKind::Prose => (StoredChunkKind::Prose, None),
        ChunkKind::Code { lang } => (StoredChunkKind::Code, lang.as_deref()),
        ChunkKind::Table => (StoredChunkKind::Table, None),
    }
}

fn file_error(path: &Path, err: &std::io::Error) -> Error {
    Error::Index {
        path: Some(path.to_path_buf()),
        message: err.to_string(),
    }
}

/// Number of newlines in the frontmatter prefix, so cached `line_start` /
/// `line_end` are file lines, not body lines.
///
/// Relies on `frontmatter::parse` returning `body` as a suffix slice of
/// `text`; both sides are byte-exact slice boundaries.
fn frontmatter_line_shift(text: &str, body: &str) -> u32 {
    debug_assert!(text.ends_with(body));
    let prefix = text.len().saturating_sub(body.len());
    let lines = text[..prefix].bytes().filter(|byte| *byte == b'\n').count();
    u32::try_from(lines).unwrap_or(u32::MAX)
}

fn serialize_metadata(meta: &Frontmatter) -> Result<Option<String>> {
    if meta == &Frontmatter::default() {
        return Ok(None);
    }
    serde_json::to_string(meta)
        .map(Some)
        .map_err(|err| Error::internal_with_source(format!("frontmatter json: {err}"), err))
}

fn mtime_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{DB_FILE, Db};
    use rusqlite::Connection;

    #[test]
    fn mark_pending_tombstones_planned_and_removed_docs() {
        let cache = tempfile::tempdir().expect("cache");
        let mut db = Db::open(cache.path()).expect("db");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (1, '/p', 'p', 'indexed', 1, 0)",
            [],
        )
        .expect("project");
        conn.execute(
            "INSERT INTO docs (id, project_id, rel_path, abs_path, content_hash, size, mtime, indexed_at)
             VALUES (1, 1, 'gone.md', '/p/gone.md', 'abc', 1, 1, 1)",
            [],
        )
        .expect("doc");
        drop(conn);

        let plan = vec![Pending {
            id: 2,
            rel_path: "new.md".to_owned(),
            abs_path: "/p/new.md".to_owned(),
            title: None,
            frontmatter_json: None,
            size: 0,
            mtime: 0,
            content_hash: "def".to_owned(),
            chunks: Vec::new(),
        }];
        let removed_state = DocState {
            id: 1,
            rel_path: "gone.md".to_owned(),
            content_hash: "abc".to_owned(),
        };
        let removed = [&removed_state];

        mark_pending(&mut db, 1, &plan, &removed).expect("mark pending");

        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        let mut stmt = conn
            .prepare("SELECT content_hash FROM docs ORDER BY id")
            .expect("prepare");
        let hashes: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(hashes, vec![String::new(), String::new()]);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 2, "removed row is kept until the final commit");
    }
}
