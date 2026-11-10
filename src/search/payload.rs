//! Owned candidate data captured after byte admission in one short SQL snapshot.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, Transaction, TransactionBehavior, params_from_iter};

use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::index::tantivy_index::{MAX_HITS, chunk_id, chunk_id_parts};
use crate::store::repo::{BATCH_KEYS, Citation, DocMeta, db_error};
use crate::store::{Db, repo};

use super::budget::{PayloadBudget, PayloadPermit};

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;

/// Explicit candidate identity and requested payload fields, with no defaults.
#[derive(Debug, Clone, Copy)]
pub struct PayloadRequest<'a> {
    /// Expected metadata/cache root.
    pub cache_root: &'a Path,
    /// Project owning the candidate index.
    pub project_id: i64,
    /// Generation fixed during candidate preparation.
    pub generation: i64,
    /// Unique candidate ids, in the original provider input order.
    pub chunk_ids: &'a [u64],
    /// Capture full chunk text for reranking or snippets.
    pub include_text: bool,
    /// Capture distinct document metadata for post-filters.
    pub include_filter_metadata: bool,
}

#[derive(Clone, Copy)]
struct Charge {
    records: u64,
    total: u64,
}

struct PayloadData {
    // Gate actual data destruction before releasing any of its allocations.
    #[cfg(test)]
    drop_gate: Option<tests::DataDropGate>,
    root: PathBuf,
    project_id: i64,
    generation: i64,
    ids: Vec<u64>,
    positions: HashMap<u64, usize>,
    citations: HashMap<u64, Citation>,
    documents: Option<Vec<String>>,
    doc_meta: HashMap<i64, DocMeta>,
    charge: Charge,
}

/// No SQL/index handle escapes capture. Data must drop BEFORE the final permit
/// returns its charge, including constructor error/unwind cleanup.
pub struct OwnedPayload {
    data: PayloadData,
    _permit: PayloadPermit,
}

impl OwnedPayload {
    /// Preflights and copies candidate data from one read snapshot. The caller
    /// separately owns the cooperative project lease and validates index/status.
    /// Ends only its own SQL transaction; caller transactions remain caller-owned.
    ///
    /// # Errors
    /// Cancellation, identity/pending/generation mismatch, malformed candidates,
    /// byte admission or `SQLite` errors. Refusal never truncates provider text.
    pub fn capture(
        db: &Db,
        request: PayloadRequest<'_>,
        budget: &PayloadBudget,
        cancellation: &Cancellation,
    ) -> Result<Self> {
        Self::capture_with(db, request, budget, cancellation, || Ok(()), |_| Ok(()))
    }

    // Private seams force snapshot and drop schedules without global hooks.
    fn capture_with(
        db: &Db,
        request: PayloadRequest<'_>,
        budget: &PayloadBudget,
        cancellation: &Cancellation,
        after_preflight: impl FnOnce() -> Result<()>,
        after_copy: impl FnOnce(&mut PayloadData) -> Result<()>,
    ) -> Result<Self> {
        validate_request(db, request, cancellation)?;
        if !db.connection().is_autocommit() {
            return Self::read(
                db.connection(),
                request,
                budget,
                cancellation,
                after_preflight,
                after_copy,
            );
        }
        let transaction =
            Transaction::new_unchecked(db.connection(), TransactionBehavior::Deferred)
                .map_err(|err| Error::internal_with_source("begin search payload snapshot", err))?;
        let payload = Self::read(
            &transaction,
            request,
            budget,
            cancellation,
            after_preflight,
            after_copy,
        )?;
        transaction
            .commit()
            .map_err(|err| Error::internal_with_source("end search payload snapshot", err))?;
        Ok(payload)
    }

    fn read(
        connection: &Connection,
        request: PayloadRequest<'_>,
        budget: &PayloadBudget,
        cancellation: &Cancellation,
        after_preflight: impl FnOnce() -> Result<()>,
        after_copy: impl FnOnce(&mut PayloadData) -> Result<()>,
    ) -> Result<Self> {
        validate_snapshot(connection, request)?;
        let docs = document_ids(request);
        let charge = preflight(connection, request, &docs, cancellation)?;
        // Declare before allocating data: partial data drops before the permit.
        let permit = budget.reserve(charge.total)?;
        after_preflight()?;
        cancellation.check()?;
        let mut data = PayloadData::new(request, docs.len(), charge);
        read_citations(connection, request, &mut data, cancellation)?;
        if request.include_filter_metadata {
            read_doc_meta(
                connection,
                request.project_id,
                &docs,
                &mut data,
                cancellation,
            )?;
        }
        if data.string_bytes()? > charge.total - charge.records {
            return Err(Error::internal(
                "search payload exceeded its byte preflight",
            ));
        }
        after_copy(&mut data)?;
        cancellation.check()?;
        Ok(Self {
            data,
            _permit: permit,
        })
    }

    /// Root validated before capture.
    #[must_use]
    pub fn cache_root(&self) -> &Path {
        &self.data.root
    }

    /// Project identity of the SQL snapshot.
    #[must_use]
    pub const fn project_id(&self) -> i64 {
        self.data.project_id
    }

    /// Generation captured with text and citations.
    #[must_use]
    pub const fn generation(&self) -> i64 {
        self.data.generation
    }

    /// Logical preflight allowance retained by this payload, not process RSS.
    #[must_use]
    pub const fn charged_bytes(&self) -> u64 {
        self.data.charge.total
    }

    /// Captured citation, absent for a missing or foreign candidate.
    #[must_use]
    pub fn citation(&self, id: u64) -> Option<&Citation> {
        self.data.citations.get(&id)
    }

    /// Captured chunk text, only when text mode and the row were present.
    #[must_use]
    pub fn chunk_text(&self, id: u64) -> Option<&str> {
        self.citation(id)?;
        let position = *self.data.positions.get(&id)?;
        self.data
            .documents
            .as_ref()?
            .get(position)
            .map(String::as_str)
    }

    /// Captured post-filter metadata, absent when not requested or not present.
    #[must_use]
    pub fn doc_meta(&self, id: i64) -> Option<&DocMeta> {
        self.data.doc_meta.get(&id)
    }

    /// Full documents in original candidate order, with legacy rerank admission.
    ///
    /// # Errors
    /// Text mode was not requested or an original candidate has no chunk text.
    pub fn rerank_documents(&self) -> Result<&[String]> {
        let documents = self
            .data
            .documents
            .as_deref()
            .ok_or_else(|| Error::internal("rerank text was not captured"))?;
        if let Some(missing) = self
            .data
            .ids
            .iter()
            .find(|id| !self.data.citations.contains_key(id))
        {
            return Err(Error::internal(format!(
                "chunk text missing for rerank (chunk {missing})"
            )));
        }
        Ok(documents)
    }
}

impl PayloadData {
    fn new(request: PayloadRequest<'_>, docs: usize, charge: Charge) -> Self {
        Self {
            #[cfg(test)]
            drop_gate: None,
            root: request.cache_root.to_path_buf(),
            project_id: request.project_id,
            generation: request.generation,
            ids: request.chunk_ids.to_vec(),
            positions: request
                .chunk_ids
                .iter()
                .copied()
                .enumerate()
                .map(|(pos, id)| (id, pos))
                .collect(),
            citations: HashMap::with_capacity(request.chunk_ids.len()),
            documents: request
                .include_text
                .then(|| vec![String::new(); request.chunk_ids.len()]),
            doc_meta: HashMap::with_capacity(docs),
            charge,
        }
    }

    fn string_bytes(&self) -> Result<u64> {
        let citations = self
            .citations
            .values()
            .flat_map(|citation| [&citation.path, &citation.heading_path]);
        let texts = self.documents.iter().flatten();
        let meta = self
            .doc_meta
            .values()
            .flat_map(|meta| std::iter::once(&meta.rel_path).chain(meta.frontmatter_json.iter()));
        citations
            .chain(texts)
            .chain(meta)
            .try_fold(0, |sum, text| add(sum, bytes(text.len())?))
    }
}

fn validate_request(
    db: &Db,
    request: PayloadRequest<'_>,
    cancellation: &Cancellation,
) -> Result<()> {
    cancellation.check()?;
    if db.cache_root() != request.cache_root {
        return Err(Error::Admission {
            message: "search payload belongs to a different cache root".to_owned(),
        });
    }
    if request.chunk_ids.len() > MAX_HITS {
        return Err(Error::Admission {
            message: "search payload candidate set exceeds the search window".to_owned(),
        });
    }
    let mut seen = HashSet::with_capacity(request.chunk_ids.len());
    if request.chunk_ids.iter().any(|id| !seen.insert(*id)) {
        return Err(Error::internal(
            "duplicate chunk id in search payload request",
        ));
    }
    Ok(())
}

fn validate_snapshot(connection: &Connection, request: PayloadRequest<'_>) -> Result<()> {
    let generation = repo::index_generation(connection, request.project_id)?;
    if generation != request.generation {
        return Err(Error::Project {
            message: "project generation changed during search preparation".to_owned(),
            instruction: Some("retry search".to_owned()),
        });
    }
    if repo::has_pending_docs(connection, request.project_id)? {
        return Err(Error::Project {
            message: "project has an incomplete index update".to_owned(),
            instruction: Some(
                "run `docsbase index` to complete the index before searching".to_owned(),
            ),
        });
    }
    Ok(())
}

fn document_ids(request: PayloadRequest<'_>) -> Vec<i64> {
    if !request.include_filter_metadata {
        return Vec::new();
    }
    request
        .chunk_ids
        .iter()
        .map(|id| chunk_id_parts(*id).0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn bytes(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| charge_overflow())
}

fn stored_bytes(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|err| Error::internal_with_source("negative SQLite payload length", err))
}
fn add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).ok_or_else(charge_overflow)
}
fn multiply(left: u64, right: u64) -> Result<u64> {
    left.checked_mul(right).ok_or_else(charge_overflow)
}
fn charge_overflow() -> Error {
    Error::Admission {
        message: "search payload charge exceeds the supported byte domain".to_owned(),
    }
}

enum StorageEncoding {
    Utf8,
    Utf16,
}

impl StorageEncoding {
    fn read(connection: &Connection) -> Result<Self> {
        let encoding: String = connection
            .pragma_query_value(None, "encoding", |row| row.get(0))
            .map_err(db_error)?;
        match encoding.as_str() {
            "UTF-8" => Ok(Self::Utf8),
            "UTF-16le" | "UTF-16be" => Ok(Self::Utf16),
            _ => Err(Error::internal(
                "unsupported SQLite text encoding in search snapshot",
            )),
        }
    }

    fn utf8_bound(&self, stored: u64) -> Result<u64> {
        match self {
            Self::Utf8 => Ok(stored),
            Self::Utf16 => multiply(add(stored, 1)? / 2, 3),
        }
    }
}

fn record_charge(request: PayloadRequest<'_>, docs: usize) -> Result<u64> {
    let mut per_candidate = add(bytes(size_of::<u64>())?, bytes(size_of::<(u64, usize)>())?)?;
    per_candidate = add(per_candidate, bytes(size_of::<(u64, Citation)>())?)?;
    if request.include_text {
        per_candidate = add(per_candidate, bytes(size_of::<String>())?)?;
    }
    let mut total = add(
        bytes(size_of::<OwnedPayload>())?,
        bytes(request.cache_root.as_os_str().as_encoded_bytes().len())?,
    )?;
    total = add(
        total,
        multiply(bytes(request.chunk_ids.len())?, per_candidate)?,
    )?;
    add(
        total,
        multiply(bytes(docs)?, bytes(size_of::<(i64, DocMeta)>())?)?,
    )
}

fn preflight(
    connection: &Connection,
    request: PayloadRequest<'_>,
    docs: &[i64],
    cancellation: &Cancellation,
) -> Result<Charge> {
    let encoding = StorageEncoding::read(connection)?;
    let records = record_charge(request, docs.len())?;
    let mut total = records;
    for batch in request.chunk_ids.chunks(BATCH_KEYS) {
        cancellation.check()?;
        let (sql, values) = citation_query(request, batch, true);
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            cancellation.check()?;
            for index in 2..=4 {
                let length: i64 = row.get(index).map_err(db_error)?;
                total = add(total, encoding.utf8_bound(stored_bytes(length)?)?)?;
            }
        }
    }
    for batch in docs.chunks(BATCH_KEYS) {
        cancellation.check()?;
        let (sql, values) = meta_query(request.project_id, batch, true);
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            cancellation.check()?;
            let path: i64 = row.get(1).map_err(db_error)?;
            total = add(total, encoding.utf8_bound(stored_bytes(path)?)?)?;
            let optional: Option<i64> = row.get(2).map_err(db_error)?;
            // Absent frontmatter has no copied string bytes.
            total = add(
                total,
                encoding.utf8_bound(stored_bytes(optional.unwrap_or(0))?)?,
            )?;
        }
    }
    Ok(Charge { records, total })
}

fn citation_query(
    request: PayloadRequest<'_>,
    batch: &[u64],
    lengths: bool,
) -> (String, Vec<Value>) {
    let text = if request.include_text {
        if lengths {
            "octet_length(c.text)"
        } else {
            "c.text"
        }
    } else if lengths {
        "0"
    } else {
        "NULL"
    };
    let columns = if lengths {
        format!("c.doc_id, c.seq, octet_length(d.rel_path), octet_length(c.heading_path), {text}")
    } else {
        format!("c.doc_id, c.seq, d.rel_path, c.heading_path, c.line_start, c.line_end, {text}")
    };
    let placeholders = std::iter::repeat_n("(?, ?)", batch.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {columns} FROM chunks c JOIN docs d ON d.id=c.doc_id WHERE d.project_id=? AND (c.doc_id,c.seq) IN ({placeholders})"
    );
    let mut values = Vec::with_capacity(1 + batch.len() * 2);
    values.push(Value::Integer(request.project_id));
    for id in batch {
        let (doc, seq) = chunk_id_parts(*id);
        values.push(Value::Integer(doc));
        values.push(Value::Integer(i64::from(seq)));
    }
    (sql, values)
}

fn meta_query(project: i64, batch: &[i64], lengths: bool) -> (String, Vec<Value>) {
    let columns = if lengths {
        "d.id, octet_length(d.rel_path), octet_length(d.frontmatter_json)"
    } else {
        "d.id, d.rel_path, d.frontmatter_json"
    };
    let placeholders = std::iter::repeat_n("?", batch.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql =
        format!("SELECT {columns} FROM docs d WHERE d.project_id=? AND d.id IN ({placeholders})");
    let values = std::iter::once(Value::Integer(project))
        .chain(batch.iter().copied().map(Value::Integer))
        .collect();
    (sql, values)
}

fn read_citations(
    connection: &Connection,
    request: PayloadRequest<'_>,
    data: &mut PayloadData,
    cancellation: &Cancellation,
) -> Result<()> {
    for batch in request.chunk_ids.chunks(BATCH_KEYS) {
        cancellation.check()?;
        let (sql, values) = citation_query(request, batch, false);
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            cancellation.check()?;
            let id = chunk_id(row.get(0).map_err(db_error)?, row.get(1).map_err(db_error)?);
            let citation = Citation {
                path: row.get(2).map_err(db_error)?,
                heading_path: row.get(3).map_err(db_error)?,
                line_start: row.get(4).map_err(db_error)?,
                line_end: row.get(5).map_err(db_error)?,
            };
            if let Some(documents) = data.documents.as_mut() {
                let position = data
                    .positions
                    .get(&id)
                    .and_then(|position| documents.get_mut(*position))
                    .ok_or_else(|| Error::internal("unexpected chunk in search payload query"))?;
                *position = row.get(6).map_err(db_error)?;
            }
            data.citations.insert(id, citation);
        }
    }
    Ok(())
}

fn read_doc_meta(
    connection: &Connection,
    project: i64,
    docs: &[i64],
    data: &mut PayloadData,
    cancellation: &Cancellation,
) -> Result<()> {
    for batch in docs.chunks(BATCH_KEYS) {
        cancellation.check()?;
        let (sql, values) = meta_query(project, batch, false);
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            cancellation.check()?;
            let id: i64 = row.get(0).map_err(db_error)?;
            data.doc_meta.insert(
                id,
                DocMeta {
                    rel_path: row.get(1).map_err(db_error)?,
                    frontmatter_json: row.get(2).map_err(db_error)?,
                },
            );
        }
    }
    Ok(())
}
