//! Durable file evidence for a local pair whose contents changed after sealing.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::config::capture::read_bounded;
use crate::error::{Error, Result};
use crate::index::tantivy_index::{REBUILD_MARKER, TOKENIZER_VERSION_FILE};
use crate::index::walk::portable_rel;
use crate::limits::MAX_STATE_BYTES;
use crate::store::generation_seal::GenerationSeal;
use crate::store::registry_inspect::{self, InspectionLimits};
use crate::store::repo;

use super::recovery::is_tantivy_file;
use super::{admission, entry_exists, filesystem, open_coordination_file};

type Files = Vec<(String, u64)>;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS generation_retirement_files (
    generation_id INTEGER PRIMARY KEY REFERENCES project_generations (id) ON DELETE CASCADE,
    seal_digest BLOB NOT NULL CHECK (length(seal_digest)=32),
    files BLOB NOT NULL
) STRICT";

pub(super) struct Retirement {
    generation: i64,
    seal: [u8; 32],
    files: Vec<u8>,
}

impl Retirement {
    pub(super) fn capture(
        path: &Path,
        generation: i64,
        seal_digest: [u8; 32],
        limits: InspectionLimits,
    ) -> Result<Self> {
        let bytes = registry_inspect::read_checked_seal(&path.join("seal.json"), &seal_digest)
            .map_err(|reason| {
                admission(format!("retiring generation seal unavailable ({reason:?})"))
            })?;
        let seal: GenerationSeal = serde_json::from_slice(&bytes).map_err(|error| {
            Error::internal_with_source("decode retiring generation seal", error)
        })?;
        let tantivy = path.join("tantivy");
        registry_inspect::check_directory(&tantivy).map_err(|reason| {
            admission(format!(
                "retiring Tantivy directory unavailable ({reason:?})"
            ))
        })?;
        let managed = read_managed_files(&tantivy, limits)?;
        let mut files = Vec::new();
        let mut retained = 0;
        for directory in [path, &tantivy] {
            for entry in filesystem(directory, "list retiring files", fs::read_dir(directory))? {
                let entry = filesystem(directory, "inspect retiring file", entry)?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let in_tantivy = directory == tantivy;
                let known = if in_tantivy {
                    is_tantivy_file(name)
                } else {
                    matches!(
                        name,
                        ".build-owner" | ".read.lock" | "metadata.db" | REBUILD_MARKER
                    )
                };
                if !known {
                    continue;
                }
                let file = entry.path();
                let relative = file
                    .strip_prefix(path)
                    .map(portable_rel)
                    .map_err(|_| admission("retiring file left its generation directory"))?;
                if in_tantivy {
                    let owned = matches!(
                        name,
                        "meta.json"
                            | ".managed.json"
                            | ".tantivy-meta.lock"
                            | ".tantivy-writer.lock"
                            | REBUILD_MARKER
                            | TOKENIZER_VERSION_FILE
                    ) || managed.contains(name)
                        || seal
                            .files
                            .binary_search_by(|(file, _)| file.cmp(&relative))
                            .is_ok();
                    if !owned {
                        continue;
                    }
                }
                retained = take_manifest_space(retained, &relative, limits)?;
                let opened = open_coordination_file(&file, false)?;
                let bytes = filesystem(&file, "measure retiring file", opened.metadata())?.len();
                files.push((relative, bytes));
            }
        }
        files.sort_unstable();
        let files = serde_json::to_vec(&files).map_err(|error| {
            Error::internal_with_source("encode retirement file evidence", error)
        })?;
        if files.len() > limit(limits) {
            return Err(admission(
                "retirement file evidence exceeds the metadata limit",
            ));
        }
        Ok(Self {
            generation,
            seal: seal_digest,
            files,
        })
    }

    pub(super) fn record(self, tx: &Transaction<'_>) -> Result<()> {
        tx.execute_batch(SCHEMA).map_err(repo::db_error)?;
        let inserted = tx
            .execute(
                "INSERT INTO generation_retirement_files (generation_id, seal_digest, files)
             SELECT id, seal_digest, ?3 FROM project_generations
             WHERE id=?1 AND seal_digest=?2 AND phase='retired'",
                params![self.generation, self.seal, self.files],
            )
            .map_err(repo::db_error)?;
        if inserted != 1 {
            return Err(admission(
                "generation changed before recording retirement files",
            ));
        }
        Ok(())
    }
}

fn read_managed_files(tantivy: &Path, limits: InspectionLimits) -> Result<HashSet<String>> {
    let path = tantivy.join(".managed.json");
    if !entry_exists(&path)? {
        return Ok(HashSet::new());
    }
    let bytes = read_bounded(open_coordination_file(&path, false)?, limit(limits) as u64)
        .map_err(|error| admission(format!("Tantivy managed files unavailable: {error}")))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error::internal_with_source("decode retiring Tantivy managed files", error)
    })
}

fn limit(limits: InspectionLimits) -> usize {
    usize::try_from(MAX_STATE_BYTES)
        .unwrap_or(usize::MAX)
        .min(limits.max_metadata_bytes)
}

fn take_manifest_space(current: usize, name: &str, limits: InspectionLimits) -> Result<usize> {
    current
        .checked_add(name.len() + size_of::<(String, u64)>())
        .filter(|total| *total <= limit(limits))
        .ok_or_else(|| admission("retirement file inventory exceeds the metadata limit"))
}

pub(super) fn load(
    registry: &Connection,
    generation: i64,
    seal: &[u8],
    limits: InspectionLimits,
) -> Result<Option<Files>> {
    let available: bool = registry
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema
         WHERE type='table' AND name='generation_retirement_files')",
            [],
            |row| row.get(0),
        )
        .map_err(repo::db_error)?;
    if !available {
        return Ok(None);
    }
    let limit = i64::try_from(limit(limits))
        .map_err(|_| admission("retirement manifest limit exceeds the SQLite integer range"))?;
    let loaded: Option<Option<Vec<u8>>> = registry
        .query_row(
            "SELECT CASE WHEN seal_digest=?2 AND typeof(files)='blob' AND length(files)<=?3
                     THEN files END
         FROM generation_retirement_files WHERE generation_id=?1",
            params![generation, seal, limit],
            |row| row.get(0),
        )
        .optional()
        .map_err(repo::db_error)?;
    let Some(loaded) = loaded else {
        return Ok(None);
    };
    let bytes = loaded.ok_or_else(|| admission("invalid or oversized retirement file evidence"))?;
    let files: Files = serde_json::from_slice(&bytes)
        .map_err(|error| Error::internal_with_source("decode retirement file evidence", error))?;
    if files.is_empty() || files.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(admission("invalid retirement file inventory ordering"));
    }
    Ok(Some(files))
}
