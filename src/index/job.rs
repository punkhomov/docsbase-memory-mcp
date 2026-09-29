//! Full index job: walk → hash → diff → tantivy + `SQLite` (FR-16, FR-18; A4; R2).

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

/// One non-fatal per-file problem, surfaced by `status` (FR-19, A4; NFR-8).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileWarning {
    /// Path relative to the project root when known, absolute otherwise.
    pub path: String,
    /// Human-readable reason (size limit, read/parse failure, budget).
    pub message: String,
}

/// Warnings kept per job before the list is truncated.
pub const MAX_WARNINGS: usize = 100;

/// Collects a warning while staying under [`MAX_WARNINGS`].
fn push_warning(stats: &mut JobStats, path: impl Into<String>, message: impl Into<String>) {
    if stats.warnings.len() < MAX_WARNINGS {
        stats.warnings.push(FileWarning {
            path: path.into(),
            message: message.into(),
        });
    }
}

/// Outcome of one indexing run.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobStats {
    /// New or changed documents written.
    pub docs: usize,
    /// Chunks added to tantivy and `SQLite`.
    pub chunks: usize,
    /// Documents skipped because the content hash matched.
    pub skipped: usize,
    /// Documents removed because the file is gone.
    pub removed: usize,
    /// Files skipped non-fatally (IO/parse/size/limit problems, A4).
    pub errors: usize,
    /// Detailed non-fatal problems (capped at [`MAX_WARNINGS`]).
    pub warnings: Vec<FileWarning>,
}

#[derive(Debug)]
struct Pending {
    id: Option<i64>,
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
/// Order: mark in-flight (allocating ids inside the marker transaction) →
/// tantivy commit → short `SQLite` write transaction (R2). A crash between the
/// commits makes the next run reprocess marked documents instead of trusting
/// a stale hash.
///
/// # Errors
/// Returns errors for walk setup failures and `SQLite`/tantivy failures that
/// affect the whole job.
pub fn run_full(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    config: &Config,
) -> Result<JobStats> {
    if index.was_recreated() {
        // The index was rebuilt from an upgraded schema: SQLite rows still
        // describe the previous index, so every document must be re-added
        // instead of being skipped as unchanged (T27).
        let dropped = repo::delete_project_docs(db.connection(), project.id)?;
        eprintln!(
            "index schema upgraded for project {}; reindexing {dropped} document(s)",
            project.id
        );
    }
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
            Err(err) => {
                walk_errors += 1;
                push_warning(&mut stats, "", err.to_string());
            }
        }
    }
    stats.errors += walk_errors;
    paths.sort();

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for path in paths {
        if let Ok(rel) = path.strip_prefix(&project.canonical_root) {
            let rel = rel.to_string_lossy().into_owned();
            walked.insert(rel.clone());
            files.push((path, rel));
        } else {
            stats.errors += 1;
            push_warning(
                &mut stats,
                path.display().to_string(),
                "path is not inside the project root",
            );
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
                        push_warning(
                            &mut stats,
                            rel.clone(),
                            "max_docs_per_project reached; file skipped",
                        );
                        continue;
                    }
                    new_budget -= 1;
                }
                prepared.id = previous.map(|state| state.id);
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(err) => {
                stats.errors += 1;
                push_warning(&mut stats, rel.clone(), err.to_string());
            }
        }
    }

    finish_plan(db, index, project.id, &mut plan, &removed, &mut stats)?;
    if index.was_recreated() {
        index.mark_rebuilt()?;
    }
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
/// Missing `.md` paths purge that document; other missing paths are treated
/// as removed/renamed directories and purge every document below them
/// (FR-16). The document budget accounts for both subtractions before new
/// documents are admitted, so a rename at the limit still succeeds.
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
    if index.was_recreated() {
        // Incremental batches cannot repopulate a freshly recreated index;
        // rebuild everything under the same writer lease instead (T27).
        return run_full(db, index, project, config);
    }
    let existing = repo::doc_states(db.connection(), project.id)?;
    let by_path: HashMap<&str, &DocState> = existing
        .iter()
        .map(|state| (state.rel_path.as_str(), state))
        .collect();

    let mut stats = JobStats::default();
    let mut plan: Vec<Pending> = Vec::new();
    let mut removed: Vec<&DocState> = Vec::new();
    let mut removed_ids: HashSet<i64> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<(String, &PathBuf)> = Vec::new();

    // Pass 1: deduplicate inputs and collect removals (files and whole
    // subtrees), so pass 2 sees the final document count.
    for path in changed {
        let Ok(rel) = path.strip_prefix(&project.canonical_root) else {
            stats.errors += 1;
            push_warning(
                &mut stats,
                path.display().to_string(),
                "path is not inside the project root",
            );
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        if !seen.insert(rel.clone()) {
            continue;
        }
        unique.push((rel.clone(), path));
        if path.exists() {
            continue;
        }
        let exact = by_path.get(rel.as_str()).copied();
        if let Some(previous) = exact
            && removed_ids.insert(previous.id)
        {
            removed.push(previous);
        }
        // A missing path may also be a removed/renamed directory (even one
        // named like a document); purge everything below its prefix (FR-16).
        let prefix = format!("{}/", rel.trim_end_matches('/'));
        let mut prefixed = false;
        for state in existing
            .iter()
            .filter(|state| state.rel_path.starts_with(&prefix))
        {
            prefixed = true;
            if removed_ids.insert(state.id) {
                removed.push(state);
            }
        }
        if exact.is_none() && !prefixed && walk::is_markdown(path) {
            stats.errors += 1;
            push_warning(&mut stats, rel.clone(), "file disappeared before indexing");
        }
    }

    // Pass 2: hash and chunk new/changed documents under the live budget
    // (removals free slots before additions are admitted).
    let mut new_budget = config
        .max_docs_per_project
        .saturating_sub(existing.len().saturating_sub(removed_ids.len()));
    let mut still_present: HashSet<i64> = HashSet::new();
    for (rel, path) in unique {
        if !path.exists() {
            continue;
        }
        let previous = by_path.get(rel.as_str()).copied();
        if let Some(previous) = previous {
            still_present.insert(previous.id);
            if removed_ids.contains(&previous.id) {
                // Recreated between the passes: still occupies a slot.
                new_budget = new_budget.saturating_sub(1);
            }
        } else {
            if new_budget == 0 {
                stats.errors += 1;
                push_warning(
                    &mut stats,
                    rel.clone(),
                    "max_docs_per_project reached; file skipped",
                );
                continue;
            }
            new_budget -= 1;
        }
        match prepare_doc(path, &rel, previous, config) {
            Ok(Some(mut prepared)) => {
                prepared.id = previous.map(|state| state.id);
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(err) => {
                stats.errors += 1;
                push_warning(&mut stats, rel.clone(), err.to_string());
            }
        }
    }

    // A document whose file exists after both passes must not be purged even
    // if a purge was planned while it briefly looked missing (flip race).
    let planned_ids: HashSet<i64> = plan.iter().filter_map(|pending| pending.id).collect();
    removed.retain(|state| !planned_ids.contains(&state.id) && !still_present.contains(&state.id));

    finish_plan(db, index, project.id, &mut plan, &removed, &mut stats)?;
    if index.was_recreated() {
        index.mark_rebuilt()?;
    }
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
        let id = pending
            .id
            .ok_or_else(|| Error::internal("pending document without id"))?;
        index.delete_doc(id);
        for chunk in &mut pending.chunks {
            chunk.doc_id = id;
        }
        index.add_chunks(&pending.chunks)?;
        stats.docs += 1;
        stats.chunks += pending.chunks.len();
    }
    for state in removed {
        index.delete_doc(state.id);
    }
    stats.removed = removed.len();

    // Tantivy first, then a short SQLite write transaction (R2): the marker
    // rows make a crash in between reprocess on the next run.
    index.commit()?;
    apply_plan(db, project_id, plan, removed)
}

fn mark_pending(
    db: &mut Db,
    project_id: i64,
    plan: &mut [Pending],
    removed: &[&DocState],
) -> Result<()> {
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))?;
    for pending in plan.iter_mut() {
        if let Some(id) = pending.id {
            repo::invalidate_doc(&tx, id)?;
        } else {
            let id =
                repo::insert_pending_doc(&tx, project_id, &pending.rel_path, &pending.abs_path)?;
            pending.id = Some(id);
        }
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
        id: None,
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

fn apply_plan(db: &mut Db, project_id: i64, plan: &[Pending], removed: &[&DocState]) -> Result<()> {
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))?;
    let now = unix_now();

    for pending in plan {
        let id = pending
            .id
            .ok_or_else(|| Error::internal("pending document without id"))?;
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
        repo::upsert_doc(&tx, project_id, id, &doc)?;

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
        repo::replace_chunks(&tx, id, &rows)?;
    }
    for state in removed {
        repo::delete_doc(&tx, state.id)?;
    }

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
    fn concurrent_marker_tx_allocates_distinct_ids() {
        use std::time::Duration;

        let cache = tempfile::tempdir().expect("cache");
        let mut db_a = Db::open(cache.path()).expect("db a");
        let mut db_b = Db::open(cache.path()).expect("db b");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (1, '/p', 'p', 'indexed', 1, 0)",
            [],
        )
        .expect("project");
        drop(conn);

        let tx = db_a
            .connection_mut()
            .transaction()
            .expect("tx a holds the write lock");
        let id_a = repo::insert_pending_doc(&tx, 1, "a.md", "/p/a.md").expect("alloc a");

        let handle = std::thread::spawn(move || {
            let tx = db_b
                .connection_mut()
                .transaction()
                .expect("tx b blocks until a commits");
            let id = repo::insert_pending_doc(&tx, 1, "b.md", "/p/b.md").expect("alloc b");
            tx.commit().expect("commit b");
            id
        });
        std::thread::sleep(Duration::from_millis(100));
        tx.commit().expect("commit a");

        let id_b = handle.join().expect("join");
        assert_ne!(id_a, id_b);
        assert!(id_a > 0 && id_b > 0);
    }

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

        let mut plan = vec![Pending {
            id: None,
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

        mark_pending(&mut db, 1, &mut plan, &removed).expect("mark pending");

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
        let assigned = plan[0].id.expect("new doc id assigned in marker tx");
        assert!(assigned > 1, "SQLite allocated the id: {assigned}");
    }
}
