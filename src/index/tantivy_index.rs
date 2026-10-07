//! Tantivy schema and per-project index handle (FR-19, FR-21; NFR-1).

use std::path::{Path, PathBuf};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::error::{Error, Result};
use crate::index::chunk::{CHUNK_OVERLAP, Chunk};
use crate::index::job::MAX_CHUNK_CHARS;
use crate::index::tokenizer::{self, IdentifierTokenizer};

/// Writer heap: tantivy's minimum is 15 MB; 20 MB keeps a single indexing
/// thread without growing the RSS of short-lived CLI runs much.
const WRITER_HEAP_BYTES: usize = 20_000_000;

/// Score multiplier for chunks above [`MAX_CHUNK_CHARS`]: oversized
/// sections (giant tables or fences) must not outrank compact answers
/// (FR-20, T27).
const LONG_CHUNK_PENALTY: f32 = 0.5;

/// Upper bound for a single search's `limit`. Tantivy's top-k collector
/// allocates proportional to the requested limit, so an unbounded
/// client-supplied value would abort the shared daemon (final review C1).
pub const MAX_HITS: usize = 1_000;

/// Marker written when the on-disk index must be rebuilt from `SQLite` (T27).
/// It survives crashes: the wipe and reindex happen in the next job, and
/// reads refuse an index that still carries it.
pub const REBUILD_MARKER: &str = "docsbase.rebuild";

/// A search hit: composite chunk id, owning document, BM25 score.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Composite id `doc_id << 32 | seq`.
    pub chunk_id: u64,
    /// Owning document id.
    pub doc_id: i64,
    /// BM25 score.
    pub score: f32,
}

/// Composite chunk id used across the store and the index.
#[must_use]
pub fn chunk_id(doc_id: i64, seq: u32) -> u64 {
    debug_assert!(doc_id >= 0, "doc ids are positive row ids");
    debug_assert!(doc_id < (1_i64 << 32), "doc id must fit 32 bits");
    u64::try_from(doc_id)
        .unwrap_or(0)
        .wrapping_shl(32)
        .wrapping_add(u64::from(seq))
}

/// Splits a composite id back into `(doc_id, seq)` for citation joins.
#[must_use]
pub fn chunk_id_parts(chunk_id_value: u64) -> (i64, u32) {
    let doc_id = i64::try_from(chunk_id_value >> 32).unwrap_or(i64::MAX);
    let seq = u32::try_from(chunk_id_value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    (doc_id, seq)
}

#[derive(Debug, Clone, Copy)]
struct Fields {
    chunk_id: Field,
    doc_id: Field,
    text: Field,
    title: Field,
    heading_path: Field,
    identifiers: Field,
    text_len: Field,
}

/// Open tantivy index plus writer/reader; single writer per project (I5).
pub struct IndexHandle {
    writer: IndexWriter,
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
    dir: PathBuf,
    recreated: bool,
}

impl IndexHandle {
    /// Opens the index in `dir`, creating it when absent.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] on IO/tantivy failures or a schema mismatch.
    pub fn open_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|err| {
            Error::internal_with_source(format!("create index dir {}: {err}", dir.display()), err)
        })?;
        let marker = dir.join(REBUILD_MARKER);
        let mut recreated = false;
        let index = if dir.join("meta.json").exists() {
            match open_index(dir) {
                Ok(existing) if fields_from(&existing.schema()).is_ok() && !marker.exists() => {
                    existing
                }
                Ok(existing) => {
                    // An index written by an older build, or one whose
                    // rebuild was interrupted: the directory is a derived
                    // cache, so recreate it and signal the caller to
                    // re-index from SQLite (T27).
                    drop(existing);
                    create_fresh(dir, &marker)?;
                    recreated = true;
                    open_index(dir)?
                }
                Err(err) => {
                    // Corrupt/truncated `meta.json` must not brick search and
                    // indexing forever: the derived cache is recreated and
                    // rebuilt (final review).
                    eprintln!(
                        "warning: index {} is unreadable ({err}); recreating",
                        dir.display()
                    );
                    create_fresh(dir, &marker)?;
                    recreated = true;
                    open_index(dir)?
                }
            }
        } else {
            // A brand-new directory cannot hold any chunks, so whatever
            // SQLite remembers must be re-added (covers manual deletion).
            // The marker is written *before* the index exists so even a hard
            // crash mid-creation leaves a durable rebuild requirement (T27).
            write_rebuild_marker(dir, &marker)?;
            let index = Index::create_in_dir(dir, build_schema()).map_err(tantivy_error)?;
            register_tokenizer(&index);
            recreated = true;
            index
        };
        let fields = fields_from(&index.schema())?;
        let writer: IndexWriter = index.writer(WRITER_HEAP_BYTES).map_err(tantivy_error)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(tantivy_error)?;
        let parser = build_parser(&index, &fields);

        Ok(Self {
            writer,
            reader,
            parser,
            fields,
            dir: dir.to_path_buf(),
            recreated,
        })
    }

    /// True when the index was created fresh or a stale/incomplete one was
    /// replaced on open: callers must rebuild from `SQLite` instead of trusting
    /// incremental state. Survives crashes via [`REBUILD_MARKER`].
    #[must_use]
    pub fn was_recreated(&self) -> bool {
        self.recreated
    }

    /// Clears [`REBUILD_MARKER`] after a successful full rebuild.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the marker cannot be removed.
    pub fn mark_rebuilt(&mut self) -> Result<()> {
        let marker = self.dir.join(REBUILD_MARKER);
        match std::fs::remove_file(&marker) {
            Ok(()) => {
                self.recreated = false;
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.recreated = false;
                Ok(())
            }
            Err(err) => Err(Error::internal_with_source(
                format!("remove {}: {err}", marker.display()),
                err,
            )),
        }
    }

    /// Reader for the current index generation (used by read-only CLI paths).
    #[must_use]
    pub fn reader(&self) -> &IndexReader {
        &self.reader
    }

    /// Adds chunks to the pending segment (visible after [`Self::commit`]).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when tantivy rejects a document.
    pub fn add_chunks(&mut self, chunks: &[Chunk]) -> Result<()> {
        for chunk in chunks {
            let mut document = TantivyDocument::new();
            document.add_u64(self.fields.chunk_id, chunk_id(chunk.doc_id, chunk.seq));
            document.add_i64(self.fields.doc_id, chunk.doc_id);
            document.add_text(self.fields.text, &chunk.text);
            // The title field carries the document-level heading (outermost
            // breadcrumb entry), not the deepest one (T10 minor, T27).
            document.add_text(
                self.fields.title,
                chunk.heading_path.first().map_or("", String::as_str),
            );
            document.add_text(self.fields.heading_path, chunk.heading_path.join(" > "));
            document.add_text(self.fields.identifiers, extract_identifiers(&chunk.text));
            document.add_u64(
                self.fields.text_len,
                u64::try_from(chunk.text.chars().count()).unwrap_or(u64::MAX),
            );
            self.writer.add_document(document).map_err(tantivy_error)?;
        }
        Ok(())
    }

    /// Deletes every chunk of `doc_id` (visible after [`Self::commit`]).
    pub fn delete_doc(&self, doc_id: i64) {
        self.writer
            .delete_term(Term::from_field_i64(self.fields.doc_id, doc_id));
    }

    /// Commits pending operations and reloads the reader.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when commit or reload fails.
    pub fn commit(&mut self) -> Result<()> {
        self.writer.commit().map_err(tantivy_error)?;
        self.reader.reload().map_err(tantivy_error)?;
        Ok(())
    }

    /// Runs a BM25 search over all fields with identifier boosts.
    ///
    /// # Errors
    /// Returns [`Error::Query`] for an unparsable query and [`Error::Internal`]
    /// for search failures.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        search_reader(&self.reader, &self.parser, &self.fields, query, limit)
    }
}

/// Read-only handle for search-only consumers (CLI snapshots, FR-30).
pub struct ReadIndex {
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
}

impl ReadIndex {
    /// Opens an existing index without creating a writer (I5-friendly).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the index is missing or unreadable.
    pub fn open(dir: &Path) -> Result<Self> {
        if dir.join(REBUILD_MARKER).exists() {
            return Err(Error::Project {
                message: format!("index {} needs to be rebuilt", dir.display()),
                instruction: Some("run `docsbase index` to rebuild".to_owned()),
            });
        }
        let index = open_index(dir)?;
        let fields = fields_from(&index.schema()).map_err(|err| Error::Project {
            message: format!("index {} uses an outdated schema: {err}", dir.display()),
            instruction: Some("run `docsbase index` to rebuild".to_owned()),
        })?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(tantivy_error)?;
        let parser = build_parser(&index, &fields);
        Ok(Self {
            reader,
            parser,
            fields,
        })
    }

    /// Reader for the snapshot.
    #[must_use]
    pub fn reader(&self) -> &IndexReader {
        &self.reader
    }

    /// Runs a BM25 search over all fields with identifier boosts.
    ///
    /// # Errors
    /// Returns [`Error::Query`] for an unparsable query and [`Error::Internal`]
    /// for search failures.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        search_reader(&self.reader, &self.parser, &self.fields, query, limit)
    }
}

fn search_reader(
    reader: &IndexReader,
    parser: &QueryParser,
    fields: &Fields,
    query: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    if query.trim().is_empty() {
        return Err(Error::Query {
            message: "empty query".to_owned(),
        });
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = limit.min(MAX_HITS);
    let parsed = parser.parse_query(query).map_err(|err| Error::Query {
        message: format!("parse query {query:?}: {err}"),
    })?;
    let searcher = reader.searcher();
    // Over-fetch so the long-chunk penalty can reorder before truncation.
    let fetch = limit.saturating_mul(4).max(limit.saturating_add(16));
    let top = searcher
        .search(&parsed, &TopDocs::with_limit(fetch).order_by_score())
        .map_err(tantivy_error)?;

    let mut hits = Vec::with_capacity(top.len());
    for (score, address) in top {
        let document: TantivyDocument = searcher.doc(address).map_err(tantivy_error)?;
        let chunk = document
            .get_first(fields.chunk_id)
            .and_then(|value| value.as_u64())
            .ok_or_else(|| Error::internal(format!("index doc {address:?}: missing chunk_id")))?;
        let doc = document
            .get_first(fields.doc_id)
            .and_then(|value| value.as_i64())
            .ok_or_else(|| Error::internal(format!("index doc {address:?}: missing doc_id")))?;
        let text_len = document
            .get_first(fields.text_len)
            .and_then(|value| value.as_u64())
            .ok_or_else(|| Error::internal(format!("index doc {address:?}: missing text_len")))?;
        let score = if text_len > (MAX_CHUNK_CHARS + CHUNK_OVERLAP) as u64 {
            score * LONG_CHUNK_PENALTY
        } else {
            score
        };
        hits.push(Hit {
            chunk_id: chunk,
            doc_id: doc,
            score,
        });
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    Ok(hits)
}

/// Removes a stale index directory and creates a current-schema one with the
/// rebuild marker already in place.
fn create_fresh(dir: &Path, marker: &Path) -> Result<()> {
    std::fs::remove_dir_all(dir).map_err(|err| {
        Error::internal_with_source(format!("remove stale index {}: {err}", dir.display()), err)
    })?;
    std::fs::create_dir_all(dir).map_err(|err| {
        Error::internal_with_source(format!("recreate index dir {}: {err}", dir.display()), err)
    })?;
    // Marker first: a crash before `meta.json` exists must still be seen as
    // "needs rebuild" by the next open (T27).
    write_rebuild_marker(dir, marker)?;
    let index = Index::create_in_dir(dir, build_schema()).map_err(tantivy_error)?;
    register_tokenizer(&index);
    drop(index);
    Ok(())
}

fn write_rebuild_marker(dir: &Path, marker: &Path) -> Result<()> {
    if let Err(err) = std::fs::write(marker, b"rebuild required\n") {
        // Without the marker the empty index would look current; delete it so
        // the next open recreates it (T27).
        let _ = std::fs::remove_dir_all(dir);
        return Err(Error::internal_with_source(
            format!("write rebuild marker {}: {err}", marker.display()),
            err,
        ));
    }
    Ok(())
}

fn open_index(dir: &Path) -> Result<Index> {
    let index = Index::open_in_dir(dir).map_err(tantivy_error)?;
    register_tokenizer(&index);
    Ok(index)
}

fn register_tokenizer(index: &Index) {
    index
        .tokenizers()
        .register(tokenizer::NAME, IdentifierTokenizer);
}

fn build_parser(index: &Index, fields: &Fields) -> QueryParser {
    let mut parser = QueryParser::for_index(
        index,
        vec![
            fields.text,
            fields.title,
            fields.heading_path,
            fields.identifiers,
        ],
    );
    parser.set_field_boost(fields.text, 1.0);
    parser.set_field_boost(fields.title, 2.0);
    parser.set_field_boost(fields.heading_path, 1.5);
    parser.set_field_boost(fields.identifiers, 2.5);
    parser
}

fn build_schema() -> Schema {
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(tokenizer::NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let numeric = NumericOptions::default().set_stored().set_indexed();

    let mut builder = Schema::builder();
    builder.add_u64_field("chunk_id", numeric.clone());
    builder.add_i64_field("doc_id", numeric.clone());
    builder.add_u64_field("text_len", numeric);
    builder.add_text_field("text", text.clone());
    builder.add_text_field("title", text.clone());
    builder.add_text_field("heading_path", text.clone());
    builder.add_text_field("identifiers", text);
    builder.build()
}

fn fields_from(schema: &Schema) -> Result<Fields> {
    let get = |name: &str| -> Result<Field> {
        schema
            .get_field(name)
            .map_err(|err| Error::internal(format!("schema field {name}: {err}")))
    };
    Ok(Fields {
        chunk_id: get("chunk_id")?,
        doc_id: get("doc_id")?,
        text: get("text")?,
        title: get("title")?,
        heading_path: get("heading_path")?,
        identifiers: get("identifiers")?,
        text_len: get("text_len")?,
    })
}

fn extract_identifiers(text: &str) -> String {
    text.split_whitespace()
        .filter(|word| {
            word.chars().any(char::is_alphanumeric) && word.chars().any(|ch| !ch.is_alphanumeric())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tantivy_error(err: tantivy::TantivyError) -> Error {
    Error::internal_with_source(format!("tantivy: {err}"), err)
}
