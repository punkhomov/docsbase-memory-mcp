//! Tantivy schema and per-project index handle (FR-19, FR-21; NFR-1).

use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::error::{Error, Result};
use crate::index::chunk::Chunk;
use crate::index::tokenizer::{self, IdentifierTokenizer};

/// Writer heap: tantivy's minimum is 15 MB; 20 MB keeps a single indexing
/// thread without growing the RSS of short-lived CLI runs much.
const WRITER_HEAP_BYTES: usize = 20_000_000;

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

#[derive(Debug, Clone, Copy)]
struct Fields {
    chunk_id: Field,
    doc_id: Field,
    text: Field,
    title: Field,
    heading_path: Field,
    identifiers: Field,
}

/// Open tantivy index plus writer/reader; single writer per project (I5).
pub struct IndexHandle {
    writer: IndexWriter,
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
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
        let index = if dir.join("meta.json").exists() {
            Index::open_in_dir(dir).map_err(tantivy_error)?
        } else {
            Index::create_in_dir(dir, build_schema()).map_err(tantivy_error)?
        };
        index
            .tokenizers()
            .register(tokenizer::NAME, IdentifierTokenizer);
        let fields = fields_from(&index.schema())?;
        let writer: IndexWriter = index.writer(WRITER_HEAP_BYTES).map_err(tantivy_error)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(tantivy_error)?;

        let mut parser = QueryParser::for_index(
            &index,
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

        Ok(Self {
            writer,
            reader,
            parser,
            fields,
        })
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
            document.add_text(
                self.fields.title,
                chunk.heading_path.last().map_or("", String::as_str),
            );
            document.add_text(self.fields.heading_path, chunk.heading_path.join(" > "));
            document.add_text(self.fields.identifiers, extract_identifiers(&chunk.text));
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
        if query.trim().is_empty() {
            return Err(Error::Query {
                message: "empty query".to_owned(),
            });
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let parsed = self.parser.parse_query(query).map_err(|err| Error::Query {
            message: format!("parse query {query:?}: {err}"),
        })?;
        let searcher = self.reader.searcher();
        let top = searcher
            .search(&parsed, &TopDocs::with_limit(limit))
            .map_err(tantivy_error)?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let document: TantivyDocument = searcher.doc(address).map_err(tantivy_error)?;
            let chunk = document
                .get_first(self.fields.chunk_id)
                .and_then(|value| value.as_u64())
                .ok_or_else(|| {
                    Error::internal(format!("index doc {address:?}: missing chunk_id"))
                })?;
            let doc = document
                .get_first(self.fields.doc_id)
                .and_then(|value| value.as_i64())
                .ok_or_else(|| Error::internal(format!("index doc {address:?}: missing doc_id")))?;
            hits.push(Hit {
                chunk_id: chunk,
                doc_id: doc,
                score,
            });
        }
        Ok(hits)
    }
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
    builder.add_i64_field("doc_id", numeric);
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
