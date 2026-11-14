//! Full index job: walk → hash → diff → tantivy + `SQLite` (FR-16, FR-18; A4; R2).

use std::collections::{HashMap, HashSet};
use std::io::Read;
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
use crate::vector_cache::{Embedder, VectorCacheSource};

/// Embedding decision prepared by the composition root for one index job.
///
/// The caller supplies the embedder matching its config snapshot; `None`
/// explicitly selects lexical-only indexing. These jobs use `Config` for
/// indexing limits and never resolve another provider from it. The borrowed
/// source retains its owner's lifetime for the complete synchronous job.
pub struct IndexEmbedding<'a> {
    source: &'a VectorCacheSource,
    embedder: Option<&'a dyn Embedder>,
}

impl<'a> IndexEmbedding<'a> {
    /// Bind an already selected provider and connection owner, without I/O.
    #[must_use]
    pub fn new(source: &'a VectorCacheSource, embedder: Option<&'a dyn Embedder>) -> Self {
        Self { source, embedder }
    }

    fn embed(&self, root: &Path, texts: &[String]) -> Result<u64> {
        self.source.validate_root(root)?;
        self.embedder.map_or(Ok(0), |embedder| {
            crate::hybrid::jobs::embed_texts_from(root, embedder, texts, self.source)
        })
    }
}

/// Longest section body kept in one chunk when no paragraph boundary splits
/// it, in Unicode characters (not bytes; FR-5). Initial calibration ≈300–500
/// tokens; refined by the S2 measurement (ADR-S1).
pub const MAX_CHUNK_CHARS: usize = 1_500;

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

fn doc_limit_warning(stats: &mut JobStats, path: &str) {
    push_warning(
        stats,
        path.to_owned(),
        "max_docs_per_project reached; file skipped",
    );
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
/// new/changed files or files with stale extracted metadata are re-chunked;
/// documents whose files disappeared are purged. Per-file failures are counted
/// in [`JobStats::errors`] and never abort the job (A4).
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
    run_full_using(
        db,
        index,
        project,
        config,
        || Ok(()),
        |root, texts| {
            crate::hybrid::jobs::embed_texts(root, &config.hybrid_cfg, &config.providers, texts)
        },
    )
}

/// Full index through an explicit embedding decision and vector owner.
///
/// Config supplies indexing limits; its hybrid stage is not resolved here.
/// Root admission precedes registry reads, markers and index writes, even
/// when the supplied decision is lexical-only.
///
/// # Errors
/// Admission for another owner root, plus the failures of [`run_full`] and
/// the supplied embedder. Failed embedding leaves retry markers intact.
pub fn run_full_with_embedding(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    config: &Config,
    embedding: &IndexEmbedding<'_>,
) -> Result<JobStats> {
    embedding.source.validate_root(db.cache_root())?;
    run_full_using(
        db,
        index,
        project,
        config,
        || Ok(()),
        |root, texts| embedding.embed(root, texts),
    )
}

fn run_full_using(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    config: &Config,
    before_apply: impl FnOnce() -> Result<()>,
    embed: impl FnOnce(&Path, &[String]) -> Result<u64>,
) -> Result<JobStats> {
    super::outcome::run(
        db,
        project,
        config,
        super::outcome::SurveyScope::FullSurvey,
        |db| run_full_core(db, index, project, config, before_apply, embed),
    )
}

fn run_full_core(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    config: &Config,
    before_apply: impl FnOnce() -> Result<()>,
    embed: impl FnOnce(&Path, &[String]) -> Result<u64>,
) -> Result<JobStats> {
    let mut prepared = capture_full(db, project, config, index.was_recreated())?;
    before_apply()?;
    prepared.begin(db, index, project.id)?;
    let texts = prepared.texts();
    embed(db.cache_root(), &texts)?;
    prepared.commit(db, index, project.id)?;
    Ok(prepared.stats)
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
    run_incremental_checked(db, index, project, changed, config, || Ok(()))
}

/// Prepares the complete payload before asking the caller to admit its
/// application. A rejected plan has no marker rows or provider calls. This
/// check is also used when incremental indexing falls back to a full rebuild.
pub(crate) fn run_incremental_checked(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    before_apply: impl FnOnce() -> Result<()>,
) -> Result<JobStats> {
    run_incremental_using(
        db,
        index,
        project,
        changed,
        config,
        before_apply,
        |root, texts| {
            crate::hybrid::jobs::embed_texts(root, &config.hybrid_cfg, &config.providers, texts)
        },
    )
}

/// Incremental index through the same explicit embedding decision as full jobs.
/// A recreated index falls back to a full rebuild with this decision.
///
/// # Errors
/// Same admission/provider/index failures as [`run_full_with_embedding`].
pub fn run_incremental_with_embedding(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    embedding: &IndexEmbedding<'_>,
) -> Result<JobStats> {
    run_incremental_checked_with_embedding(
        db,
        index,
        project,
        changed,
        config,
        embedding,
        || Ok(()),
    )
}

/// Admit the prepared payload before markers or embedding, including full recovery.
pub(crate) fn run_incremental_checked_with_embedding(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    embedding: &IndexEmbedding<'_>,
    before_apply: impl FnOnce() -> Result<()>,
) -> Result<JobStats> {
    embedding.source.validate_root(db.cache_root())?;
    run_incremental_using(
        db,
        index,
        project,
        changed,
        config,
        before_apply,
        |root, texts| embedding.embed(root, texts),
    )
}

fn run_incremental_using(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    before_apply: impl FnOnce() -> Result<()>,
    embed: impl FnOnce(&Path, &[String]) -> Result<u64>,
) -> Result<JobStats> {
    if index.was_recreated() {
        // Incremental batches cannot repopulate a freshly recreated index;
        // rebuild everything under the same writer lease instead (T27).
        return run_full_using(db, index, project, config, before_apply, embed);
    }
    super::outcome::run(
        db,
        project,
        config,
        super::outcome::SurveyScope::Subset,
        |db| run_incremental_core(db, index, project, changed, config, before_apply, embed),
    )
}

fn run_incremental_core(
    db: &mut Db,
    index: &mut IndexHandle,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    before_apply: impl FnOnce() -> Result<()>,
    embed: impl FnOnce(&Path, &[String]) -> Result<u64>,
) -> Result<JobStats> {
    let mut prepared = capture_incremental(db, project, changed, config)?;
    before_apply()?;
    prepared.begin(db, index, project.id)?;
    let texts = prepared.texts();
    embed(db.cache_root(), &texts)?;
    prepared.commit(db, index, project.id)?;
    Ok(prepared.stats)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum CaptureEvent {
    Reserved(u64),
    Refused(u64),
    Copy(&'static str),
}

pub(crate) struct CaptureBudget<'a> {
    pub(crate) account: &'a mut crate::search::budget::PayloadPermit,
    pub(crate) operation: &'a crate::search::operation::SearchOperation,
    #[cfg(test)]
    pub(crate) observed: Option<&'a dyn Fn(CaptureEvent)>,
}
impl CaptureBudget<'_> {
    pub(crate) fn reserve(&mut self, bytes: u64) -> Result<()> {
        let result = (|| {
            self.operation.check()?;
            let total =
                self.account
                    .bytes()
                    .checked_add(bytes)
                    .ok_or_else(|| Error::Admission {
                        message: "index capture size overflow".into(),
                    })?;
            self.account.grow_to(total)
        })();
        #[cfg(test)]
        if let Some(observer) = self.observed {
            observer(if result.is_ok() {
                CaptureEvent::Reserved(self.account.bytes())
            } else {
                CaptureEvent::Refused(self.account.bytes())
            });
        }
        result
    }
    fn warning(&mut self, message: &impl std::fmt::Display) -> Result<()> {
        // Walk errors can include long native paths; count without materializing
        // the message before reserving its formatted/transient retained copies.
        struct Count(u64);
        impl std::fmt::Write for Count {
            fn write_str(&mut self, value: &str) -> std::fmt::Result {
                self.0 = self
                    .0
                    .checked_add(value.len() as u64)
                    .ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        let mut count = Count(0);
        std::fmt::write(&mut count, format_args!("{message}")).map_err(|_| Error::Admission {
            message: "warning capture size overflow".into(),
        })?;
        self.reserve(count.0.checked_mul(2).ok_or_else(|| Error::Admission {
            message: "warning capture size overflow".into(),
        })?)
    }
    fn metadata(&mut self, db: &Db, project: i64) -> Result<()> {
        let (rows, bytes): (i64, i64) = db.connection().query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(rel_path AS BLOB))+length(CAST(content_hash AS BLOB))+COALESCE(length(CAST(frontmatter_json AS BLOB)),0)),0) FROM docs WHERE project_id=?1", [project], |row| Ok((row.get(0)?, row.get(1)?))
        ).map_err(repo::db_error)?;
        let rows = u64::try_from(rows).map_err(|_| Error::internal("negative metadata count"))?;
        let bytes =
            u64::try_from(bytes).map_err(|_| Error::internal("negative metadata byte count"))?;
        let charge = rows
            .checked_mul(1024)
            .and_then(|n| bytes.checked_mul(8).and_then(|b| n.checked_add(b)))
            .and_then(|n| n.checked_add(4096 * MAX_WARNINGS as u64))
            .ok_or_else(|| Error::Admission {
                message: "index metadata preflight overflow".into(),
            })?;
        self.reserve(charge)?;
        #[cfg(test)]
        if let Some(observer) = self.observed {
            observer(CaptureEvent::Copy("doc-state/map/removal capture"));
        }
        Ok(())
    }
    fn path(&mut self, path: &Path) -> Result<()> {
        let bytes = u64::try_from(path.as_os_str().len())
            .map_err(|_| Error::internal("path size overflow"))?;
        self.reserve(
            bytes
                .checked_mul(16)
                .and_then(|n| n.checked_add(8192))
                .ok_or_else(|| Error::Admission {
                    message: "index path preflight overflow".into(),
                })?,
        )?;
        #[cfg(test)]
        if let Some(observer) = self.observed {
            observer(CaptureEvent::Copy("path/set capture"));
        }
        Ok(())
    }
    fn read(&mut self, path: &Path, max: u64) -> Result<String> {
        let limit = max.checked_add(1).ok_or_else(|| Error::Admission {
            message: "file capture limit overflow".into(),
        })?;
        let mut file = std::fs::File::open(path)
            .map_err(|e| file_error(path, &e))?
            .take(limit);
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file.read(&mut buffer).map_err(|e| file_error(path, &e))?;
            if read == 0 {
                break;
            }
            let needed = bytes
                .len()
                .checked_add(read)
                .ok_or_else(|| Error::Admission {
                    message: "file capture size overflow".into(),
                })?;
            if needed > bytes.capacity() {
                let next = needed.max(bytes.capacity().saturating_mul(2));
                let previous = bytes.capacity();
                self.reserve(
                    u64::try_from(next)
                        .map_err(|_| Error::internal("file capture capacity overflow"))?,
                )?;
                bytes.try_reserve_exact(next - bytes.len()).map_err(|e| {
                    Error::internal_with_source("allocate accounted file capture", e)
                })?;
                self.account
                    .shrink_to(self.account.bytes().saturating_sub(previous as u64));
            }
            #[cfg(test)]
            if let Some(observer) = self.observed {
                observer(CaptureEvent::Copy("actual bounded file read"));
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.len() as u64 > max {
                return Err(Error::Index {
                    path: Some(path.to_path_buf()),
                    message: format!("file is over max_file_size {max}"),
                });
            }
        }
        String::from_utf8(bytes).map_err(|_| Error::Index {
            path: Some(path.to_path_buf()),
            message: "stream did not contain valid UTF-8".into(),
        })
    }
}

/// Captured lexical plan, without native writer state or an open transaction.
pub(crate) struct PreparedPlan {
    plan: Vec<Pending>,
    removed: Vec<DocState>,
    pub(crate) stats: JobStats,
}
impl PreparedPlan {
    pub(crate) fn begin(
        &mut self,
        db: &mut Db,
        index: &mut IndexHandle,
        project_id: i64,
    ) -> Result<()> {
        if index.was_recreated() {
            repo::delete_project_docs(db.connection(), project_id)?;
        }
        let removed: Vec<_> = self.removed.iter().collect();
        mark_pending(db, project_id, &mut self.plan, &removed)?;
        for pending in &mut self.plan {
            let id = pending
                .id
                .ok_or_else(|| Error::internal("pending document without id"))?;
            index.delete_doc(id);
            for chunk in &mut pending.chunks {
                chunk.doc_id = id;
            }
            index.add_chunks(&pending.chunks)?;
            self.stats.docs += 1;
            self.stats.chunks += pending.chunks.len();
        }
        for state in &self.removed {
            index.delete_doc(state.id);
        }
        self.stats.removed = self.removed.len();
        Ok(())
    }
    pub(crate) fn chunk_count(&self) -> usize {
        self.plan.iter().map(|p| p.chunks.len()).sum()
    }
    pub(crate) fn next_texts(
        &self,
        cursor: &mut usize,
        max_count: usize,
        max_bytes: usize,
    ) -> Vec<String> {
        let mut batch = Vec::new();
        let mut bytes = 0_usize;
        for chunk in self.plan.iter().flat_map(|p| &p.chunks).skip(*cursor) {
            if !batch.is_empty()
                && (batch.len() >= max_count || bytes.saturating_add(chunk.text.len()) > max_bytes)
            {
                break;
            }
            bytes = bytes.saturating_add(chunk.text.len());
            batch.push(chunk.text.clone());
            *cursor += 1;
        }
        batch
    }
    pub(crate) fn texts(&self) -> Vec<String> {
        self.plan
            .iter()
            .flat_map(|p| p.chunks.iter().map(|c| c.text.clone()))
            .collect()
    }
    pub(crate) fn commit(
        &self,
        db: &mut Db,
        index: &mut IndexHandle,
        project_id: i64,
    ) -> Result<()> {
        index.commit()?;
        let removed: Vec<_> = self.removed.iter().collect();
        apply_plan(db, project_id, &self.plan, &removed)?;
        if index.was_recreated() {
            index.mark_rebuilt()?;
        }
        Ok(())
    }
}

pub(crate) enum JobInput<'a> {
    Full,
    Sync,
    Incremental(std::borrow::Cow<'a, [PathBuf]>),
}

pub(crate) fn capture_input(
    db: &Db,
    project: &Project,
    config: &Config,
    recreated: bool,
    input: &JobInput<'_>,
    capture: Option<&mut CaptureBudget<'_>>,
) -> Result<(PreparedPlan, super::outcome::SurveyScope)> {
    match input {
        JobInput::Incremental(changed) if !recreated => Ok((
            capture_incremental_accounted(db, project, changed, config, capture)?,
            super::outcome::SurveyScope::Subset,
        )),
        _ => Ok((
            capture_full_accounted(db, project, config, recreated, capture)?,
            super::outcome::SurveyScope::FullSurvey,
        )),
    }
}

pub(crate) fn capture_full(
    db: &Db,
    project: &Project,
    config: &Config,
    recreated: bool,
) -> Result<PreparedPlan> {
    capture_input(db, project, config, recreated, &JobInput::Full, None).map(|(plan, _)| plan)
}
pub(crate) fn capture_full_accounted(
    db: &Db,
    project: &Project,
    config: &Config,
    recreated: bool,
    mut capture: Option<&mut CaptureBudget<'_>>,
) -> Result<PreparedPlan> {
    if let Some(budget) = capture.as_deref_mut() {
        budget.metadata(db, project.id)?;
    }
    // A recreated index needs every document re-added (T27). Delay the SQL
    // wipe until the prepared payload has passed the caller's config check.
    let existing = if recreated {
        Vec::new()
    } else {
        repo::doc_states(db.connection(), project.id)?
    };
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
            Ok(path) => {
                if let Some(budget) = capture.as_deref_mut() {
                    budget.path(&path)?;
                }
                paths.push(path);
            }
            Err(err) => {
                walk_errors += 1;
                if let Some(budget) = capture.as_deref_mut() {
                    budget.warning(&err)?;
                }
                push_warning(&mut stats, "", err.to_string());
            }
        }
    }
    stats.errors += walk_errors;
    paths.sort();

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for path in paths {
        if let Some(rel) = relative_to_root(&path, &project.canonical_root) {
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
        match prepare_doc_accounted(path, rel, previous, config, capture.as_deref_mut()) {
            Ok(Some(mut prepared)) => {
                if previous.is_none() {
                    if new_budget == 0 {
                        stats.errors += 1;
                        doc_limit_warning(&mut stats, rel);
                        continue;
                    }
                    new_budget -= 1;
                }
                prepared.id = previous.map(|state| state.id);
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(err @ Error::Admission { .. }) => return Err(err),
            Err(err) => {
                stats.errors += 1;
                push_warning(&mut stats, rel.clone(), err.to_string());
            }
        }
    }

    Ok(PreparedPlan {
        removed: removed.into_iter().cloned().collect(),
        plan,
        stats,
    })
}

pub(crate) fn capture_incremental(
    db: &Db,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
) -> Result<PreparedPlan> {
    capture_input(
        db,
        project,
        config,
        false,
        &JobInput::Incremental(std::borrow::Cow::Borrowed(changed)),
        None,
    )
    .map(|(plan, _)| plan)
}
pub(crate) fn capture_incremental_accounted(
    db: &Db,
    project: &Project,
    changed: &[PathBuf],
    config: &Config,
    mut capture: Option<&mut CaptureBudget<'_>>,
) -> Result<PreparedPlan> {
    if let Some(budget) = capture.as_deref_mut() {
        budget.metadata(db, project.id)?;
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
        if let Some(budget) = capture.as_deref_mut() {
            budget.path(path)?;
        }
        let Some(rel) = relative_to_root(path, &project.canonical_root) else {
            stats.errors += 1;
            push_warning(
                &mut stats,
                path.display().to_string(),
                "path is not inside the project root",
            );
            continue;
        };
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
                doc_limit_warning(&mut stats, &rel);
                continue;
            }
            new_budget -= 1;
        }
        match prepare_doc_accounted(path, &rel, previous, config, capture.as_deref_mut()) {
            Ok(Some(mut prepared)) => {
                prepared.id = previous.map(|state| state.id);
                plan.push(prepared);
            }
            Ok(None) => stats.skipped += 1,
            Err(err @ Error::Admission { .. }) => return Err(err),
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

    Ok(PreparedPlan {
        removed: removed.into_iter().cloned().collect(),
        plan,
        stats,
    })
}

fn mark_pending(
    db: &mut Db,
    project_id: i64,
    plan: &mut [Pending],
    removed: &[&DocState],
) -> Result<()> {
    let tx = db.begin_write()?;
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

/// Project-relative slash path for one input of a full or incremental run.
///
/// Watcher events arrive canonical, but callers may pass the OS temp spelling
/// (Windows 8.3 short names) or a case variant; resolve those before giving
/// up. Missing files resolve through their parent directory.
fn relative_to_root(path: &Path, root: &Path) -> Option<String> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(crate::index::walk::portable_rel(rel));
    }
    let resolved = if let Ok(resolved) = path.canonicalize() {
        resolved
    } else {
        let parent = path.parent()?.canonicalize().ok()?;
        parent.join(path.file_name()?)
    };
    resolved
        .strip_prefix(root)
        .ok()
        .map(crate::index::walk::portable_rel)
}

/// Reads and hashes one walked file. Returns `Ok(None)` when both the source
/// hash and extracted metadata match, `Err` for per-file problems (A4).
fn prepare_doc_accounted(
    path: &Path,
    rel_path: &str,
    existing: Option<&DocState>,
    config: &Config,
    mut capture: Option<&mut CaptureBudget<'_>>,
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
    let text = match capture.as_deref_mut() {
        Some(budget) => budget.read(path, config.max_file_size)?,
        None => std::fs::read_to_string(path).map_err(|err| file_error(path, &err))?,
    };
    if let Some(budget) = capture {
        budget.reserve(crate::index::chunk::capture_charge(&text, MAX_CHUNK_CHARS)?)?;
        #[cfg(test)]
        if let Some(observer) = budget.observed {
            observer(CaptureEvent::Copy(
                "hash/frontmatter/chunks/headings/JSON/apply workspace",
            ));
        }
    }
    let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    let (meta, body) = frontmatter::parse(&text);
    let frontmatter_json = serialize_metadata(&meta)?;
    // Parser fixes must refresh derived metadata even when the source bytes
    // have not changed since an older build indexed the file.
    if existing.is_some_and(|state| {
        !state.content_hash.is_empty()
            && state.content_hash == content_hash
            && state.frontmatter_json == frontmatter_json
    }) {
        return Ok(None);
    }

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
        frontmatter_json,
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime: mtime_seconds(&metadata),
        content_hash,
        chunks,
    }))
}

fn apply_plan(db: &mut Db, project_id: i64, plan: &[Pending], removed: &[&DocState]) -> Result<()> {
    let tx = db.begin_write()?;
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
    // The generation changes only when the chunk set does: dense caches key
    // on it (DELTA-7), and a no-op sync must not invalidate them.
    if !plan.is_empty() || !removed.is_empty() {
        repo::bump_index_generation(&tx, project_id)?;
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

pub(super) fn unix_now() -> i64 {
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
    fn prepared_embedding_gate_precedes_markers_in_incremental_and_full_recovery() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counting(AtomicUsize);
        impl Embedder for Counting {
            fn model_id(&self) -> &'static str {
                "index-gate@d2@l2"
            }
            fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
            }
        }

        for recovery in [false, true] {
            let cache = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("a.md");
            std::fs::write(&path, "alphawidget").unwrap();
            std::fs::write(root.path().join("b.md"), "betawidget").unwrap();
            let mut db = Db::open(cache.path()).unwrap();
            let project = crate::daemon::registry::ensure_project(&mut db, root.path()).unwrap();
            let dir = crate::daemon::lifecycle::index_dir(cache.path(), project.id);
            let mut index = IndexHandle::open_or_create(&dir).unwrap();
            let config = Config::default();
            run_full(&mut db, &mut index, &project, &config).unwrap();
            let original: Vec<_> = repo::doc_states(db.connection(), project.id)
                .unwrap()
                .into_iter()
                .map(|state| (state.id, state.content_hash))
                .collect();
            if recovery {
                drop(index);
                std::fs::write(
                    dir.join(crate::index::tantivy_index::TOKENIZER_VERSION_FILE),
                    b"1\n",
                )
                .unwrap();
                index = IndexHandle::open_or_create(&dir).unwrap();
                assert!(index.was_recreated());
            }
            std::fs::write(&path, "gammawidget").unwrap();
            let settings = crate::vector_cache::settings::standalone().unwrap();
            let source = VectorCacheSource::Owned(
                crate::vector_cache::VectorStore::acquire(cache.path(), settings.settings())
                    .unwrap(),
            );
            let provider = Counting(AtomicUsize::new(0));
            let embedding = IndexEmbedding::new(&source, Some(&provider));
            let error = run_incremental_checked_with_embedding(
                &mut db,
                &mut index,
                &project,
                std::slice::from_ref(&path),
                &config,
                &embedding,
                || Err(Error::internal("stale prepared config")),
            )
            .unwrap_err();
            assert!(error.to_string().contains("stale prepared config"));
            let after: Vec<_> = repo::doc_states(db.connection(), project.id)
                .unwrap()
                .into_iter()
                .map(|state| (state.id, state.content_hash))
                .collect();
            assert_eq!(after, original, "gate before invalidation/deletion");
            assert_eq!(
                repo::index_generation(db.connection(), project.id).unwrap(),
                1
            );
            assert_eq!(provider.0.load(Ordering::SeqCst), 0);
            assert_eq!(
                source
                    .connect(cache.path())
                    .unwrap()
                    .count_present(
                        provider.model_id(),
                        &[crate::vector_cache::chunk_hash("gammawidget")]
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                dir.join(crate::index::tantivy_index::REBUILD_MARKER)
                    .exists(),
                recovery
            );
            let stats = run_incremental_checked_with_embedding(
                &mut db,
                &mut index,
                &project,
                &[path],
                &config,
                &embedding,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(stats.docs, if recovery { 2 } else { 1 });
            assert_eq!(index.search("gammawidget", 10).unwrap().len(), 1);
            assert_eq!(index.search("alphawidget", 10).unwrap().len(), 0);
            assert_eq!(provider.0.load(Ordering::SeqCst), 1);
        }
    }

    /// Deferred read-then-write upgrades fail with `SQLITE_BUSY_SNAPSHOT`
    /// (extended 517; the busy handler does not cover them) once another
    /// connection commits in between - the Windows CI incident also showed an
    /// immediate `database is locked`. Write transactions go through
    /// [`Db::begin_write`] (`BEGIN IMMEDIATE`), so the interleaving cannot
    /// happen; with a deferred tx the "write a after b attempted" step below
    /// panics once the writer thread committed within the sleep window.
    #[test]
    fn read_then_write_tx_survives_concurrent_writer() {
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

        let tx_a = db_a.begin_write().expect("tx a");
        let name: String = tx_a
            .query_row("SELECT name FROM projects WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read inside tx a");
        assert_eq!(name, "p");

        let handle = std::thread::spawn(move || {
            let tx_b = db_b.begin_write().expect("tx b waits for a");
            tx_b.execute("UPDATE projects SET name = 'b' WHERE id = 1", [])
                .expect("write b");
            tx_b.commit().expect("commit b");
        });
        std::thread::sleep(Duration::from_millis(100));
        tx_a.execute("UPDATE projects SET name = 'a' WHERE id = 1", [])
            .expect("write a after b attempted");
        tx_a.commit().expect("commit a");
        handle.join().expect("join b");

        let name: String = db_a
            .connection()
            .query_row("SELECT name FROM projects WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("final read");
        assert_eq!(name, "b", "b committed after a released the lock");
    }

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

        let tx = db_a.begin_write().expect("tx a holds the write lock");
        let id_a = repo::insert_pending_doc(&tx, 1, "a.md", "/p/a.md").expect("alloc a");

        let handle = std::thread::spawn(move || {
            let tx = db_b.begin_write().expect("tx b blocks until a commits");
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
            frontmatter_json: None,
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

#[cfg(test)]
#[path = "capture_budget_tests.rs"]
mod capture_budget_tests;
