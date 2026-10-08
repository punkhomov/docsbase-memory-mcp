use std::path::Path;

use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::tantivy_index::{
    IndexHandle, ReadIndex, TOKENIZER_VERSION_FILE, chunk_id, chunk_id_parts,
};
use docsbase_memory::index::tokenizer::TOKENIZER_VERSION;
use tempfile::TempDir;

fn chunk(doc_id: i64, seq: u32, heading: &str, text: &str) -> Chunk {
    Chunk {
        doc_id,
        seq,
        heading_path: vec![heading.to_owned()],
        kind: ChunkKind::Prose,
        line_start: 1,
        line_end: 1,
        text: text.to_owned(),
    }
}

fn handle() -> (TempDir, IndexHandle) {
    let dir = TempDir::new().expect("tempdir");
    let mut index = IndexHandle::open_or_create(dir.path()).expect("open");
    // Unit tests populate the index directly, so the fresh-index rebuild
    // marker must be cleared explicitly (production: `run_full` does it).
    index.mark_rebuilt().expect("mark rebuilt");
    (dir, index)
}

fn legacy_schema_index(dir: &Path) {
    use tantivy::Index;
    use tantivy::schema::{
        IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
    };
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
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
    Index::create_in_dir(dir, builder.build()).expect("create legacy index");
}

#[test]
fn legacy_schema_is_recreated_for_writes() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    std::fs::create_dir_all(&path).expect("mkdir");
    legacy_schema_index(&path);

    let mut index = IndexHandle::open_or_create(&path).expect("open");
    assert!(index.was_recreated(), "stale schema must be replaced");
    index
        .add_chunks(&[chunk(1, 0, "T", "searchable prose")])
        .expect("add");
    index.commit().expect("commit");
    assert!(
        !index.search("searchable", 5).expect("search").is_empty(),
        "recreated index must be usable"
    );
    let Err(err) = ReadIndex::open(&path) else {
        panic!("reads must refuse an index awaiting rebuild");
    };
    assert!(err.to_string().contains("rebuild"), "message: {err}");
    index.mark_rebuilt().expect("mark rebuilt");
    let reopened = ReadIndex::open(&path).expect("read-only open after rebuild");
    assert!(
        !reopened.search("searchable", 5).expect("search").is_empty(),
        "rebuilt index must be readable"
    );
}

#[test]
fn corrupt_meta_is_recreated() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    std::fs::create_dir_all(&path).expect("mkdir");
    std::fs::write(path.join("meta.json"), b"not a real index").expect("write corrupt meta");

    let mut index = IndexHandle::open_or_create(&path).expect("corrupt index must be recreated");
    assert!(index.was_recreated(), "corrupt directory is replaced");
    index
        .add_chunks(&[chunk(1, 0, "T", "searchable after recovery")])
        .expect("add");
    index.commit().expect("commit");
    assert!(
        !index.search("searchable", 5).expect("search").is_empty(),
        "recovered index must be usable"
    );
    index.mark_rebuilt().expect("mark rebuilt");
    let reopened = ReadIndex::open(&path).expect("read-only open after recovery");
    assert!(
        !reopened.search("searchable", 5).expect("search").is_empty(),
        "recovered index must stay readable"
    );
}

#[test]
fn legacy_schema_is_rejected_for_reads() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    std::fs::create_dir_all(&path).expect("mkdir");
    legacy_schema_index(&path);

    let Err(err) = ReadIndex::open(&path) else {
        panic!("read-only open must refuse a legacy schema");
    };
    let text = err.to_string();
    assert!(
        text.contains("docsbase index"),
        "instruction missing: {text}"
    );
}

fn versioned_index(path: &Path) -> IndexHandle {
    let mut index = IndexHandle::open_or_create(path).expect("open");
    index.mark_rebuilt().expect("mark rebuilt");
    index
        .add_chunks(&[chunk(1, 0, "T", "versioned prose")])
        .expect("add");
    index.commit().expect("commit");
    assert_eq!(
        std::fs::read_to_string(path.join(TOKENIZER_VERSION_FILE)).expect("version file"),
        format!("{TOKENIZER_VERSION}\n"),
        "fresh index must record the pipeline version"
    );
    index
}

#[test]
fn tokenizer_version_mismatch_is_recreated_for_writes() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    let index = versioned_index(&path);
    assert!(
        !index.search("versioned", 5).expect("search").is_empty(),
        "initial index"
    );

    std::fs::write(path.join(TOKENIZER_VERSION_FILE), b"1\n").expect("downgrade version");
    // Release the writer lock before the reopen: `create_fresh` removes the
    // directory, which Windows refuses while the lockfile is open.
    drop(index);

    let mut index = IndexHandle::open_or_create(&path).expect("reopen");
    assert!(
        index.was_recreated(),
        "version mismatch must force a rebuild from SQLite"
    );
    assert!(
        index.search("versioned", 5).expect("search").is_empty(),
        "stale chunks must not survive the rebuild"
    );
    index.mark_rebuilt().expect("mark rebuilt");
    let reopened = ReadIndex::open(&path).expect("read-only open after rebuild");
    assert!(
        reopened.search("versioned", 5).expect("search").is_empty(),
        "rebuilt index must be readable"
    );
}

#[test]
fn legacy_index_without_version_file_is_recreated() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    let index = versioned_index(&path);
    drop(index);
    std::fs::remove_file(path.join(TOKENIZER_VERSION_FILE)).expect("remove version file");

    let index = IndexHandle::open_or_create(&path).expect("reopen legacy");
    assert!(
        index.was_recreated(),
        "an index without a version file is legacy and must be rebuilt"
    );
}

#[test]
fn tokenizer_version_mismatch_is_rejected_for_reads() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tantivy");
    let index = versioned_index(&path);
    drop(index);
    std::fs::write(path.join(TOKENIZER_VERSION_FILE), b"1\n").expect("downgrade version");

    let Err(err) = ReadIndex::open(&path) else {
        panic!("read-only open must refuse a version mismatch");
    };
    let text = err.to_string();
    assert!(
        text.contains("docsbase index"),
        "instruction missing: {text}"
    );
}

#[test]
fn add_and_search() {
    let (dir, mut index) = handle();
    index
        .add_chunks(&[chunk(
            1,
            0,
            "Auth",
            "authentication tokens are refreshed via the rotate endpoint",
        )])
        .expect("add");
    index.commit().expect("commit");

    let hits = index.search("refreshed", 5).expect("search");
    assert!(!hits.is_empty(), "expected hits");
    assert_eq!(hits[0].doc_id, 1);
    assert_eq!(hits[0].chunk_id, chunk_id(1, 0));

    drop(index);
    let reopened = IndexHandle::open_or_create(dir.path()).expect("reopen");
    let hits = reopened
        .search("refreshed", 5)
        .expect("search after reopen");
    assert_eq!(hits[0].doc_id, 1);
}

#[test]
fn delete_by_doc() {
    let (_dir, mut index) = handle();
    index
        .add_chunks(&[
            chunk(1, 0, "A", "shared keyword alpha document one"),
            chunk(2, 0, "B", "shared keyword alpha document two"),
        ])
        .expect("add");
    index.commit().expect("commit");

    index.delete_doc(1);
    index.commit().expect("commit delete");

    let hits = index.search("shared", 10).expect("search");
    assert!(!hits.is_empty(), "doc 2 must remain");
    assert!(
        hits.iter().all(|hit| hit.doc_id == 2),
        "doc 1 chunks must be gone: {hits:?}"
    );
}

#[test]
fn reload_after_commit() {
    let (_dir, mut index) = handle();
    index
        .add_chunks(&[chunk(1, 0, "A", "first document about caching")])
        .expect("add");
    index.commit().expect("commit");

    assert_eq!(index.search("caching", 5).expect("search").len(), 1);

    index
        .add_chunks(&[chunk(2, 0, "B", "second document about caching layers")])
        .expect("add");
    assert_eq!(
        index
            .search("caching", 5)
            .expect("search before commit")
            .len(),
        1,
        "uncommitted chunks must not be visible"
    );

    index.commit().expect("commit");
    assert_eq!(index.search("caching", 5).expect("search").len(), 2);
}

#[test]
fn exact_identifier_beats_prose() {
    let (_dir, mut index) = handle();
    index
        .add_chunks(&[
            chunk(
                1,
                0,
                "Prose",
                "the assessment plan id field identifies a plan in prose",
            ),
            chunk(
                2,
                0,
                "API",
                "call get_plan with assessment_plan_id parameter",
            ),
        ])
        .expect("add");
    index.commit().expect("commit");

    let hits = index.search("assessment_plan_id", 5).expect("search");
    assert!(!hits.is_empty(), "expected hits");
    assert_eq!(
        hits[0].doc_id, 2,
        "exact identifier must rank first: {hits:?}"
    );
}

#[test]
fn long_chunk_ranks_below_compact_match() {
    let (_dir, mut index) = handle();
    let long_text = "longwidget ".repeat(600);
    index
        .add_chunks(&[
            chunk(1, 0, "Long", &long_text),
            chunk(2, 0, "Short", "longwidget compact"),
        ])
        .expect("add");
    index.commit().expect("commit");

    let hits = index.search("longwidget", 2).expect("search");
    assert_eq!(
        hits[0].doc_id, 2,
        "over-long chunk must be penalized: {hits:?}"
    );
}

#[test]
fn phrase_does_not_match_inside_identifier() {
    // FR-7: variants of `assessment_plan_id` share one position, so the
    // phrase "plan id" must not match it; a real two-word phrase does.
    let (_dir, mut index) = handle();
    index
        .add_chunks(&[
            chunk(1, 0, "A", "assessment_plan_id is configured here"),
            chunk(2, 0, "B", "plan id appears as separate words"),
        ])
        .expect("add");
    index.commit().expect("commit");

    let hits = index.search("\"plan id\"", 10).expect("search");
    assert_eq!(
        hits.len(),
        1,
        "phrase must match exactly one chunk: {hits:?}"
    );
    assert_eq!(hits[0].doc_id, 2, "false match inside identifier: {hits:?}");
}

#[test]
fn empty_query_is_query_error() {
    let (_dir, index) = handle();
    for query in ["", "   "] {
        let err = index.search(query, 5).expect_err("must reject");
        assert!(
            matches!(err, docsbase_memory::error::Error::Query { .. }),
            "{err:?}"
        );
        assert_eq!(err.mcp_code(), -32014);
    }
}

#[test]
fn zero_limit_returns_empty() {
    let (_dir, mut index) = handle();
    index
        .add_chunks(&[chunk(1, 0, "A", "something searchable")])
        .expect("add");
    index.commit().expect("commit");
    assert_eq!(index.search("something", 0).expect("search"), Vec::new());
    let _ = index.reader();
}

#[test]
fn bad_query_is_query_error() {
    let (_dir, index) = handle();
    let err = index.search("\"unbalanced", 5).expect_err("must fail");
    assert!(
        matches!(err, docsbase_memory::error::Error::Query { .. }),
        "{err:?}"
    );
    assert_eq!(err.mcp_code(), -32014);
}

#[test]
fn chunk_id_parts_inverse() {
    assert_eq!(chunk_id_parts(chunk_id(7, 42)), (7, 42));
    assert_eq!(chunk_id_parts(chunk_id(0, 0)), (0, 0));
}

/// FR-5: the stored `text_len` is a Unicode-char count, not a byte count.
#[test]
fn stored_text_len_counts_unicode_chars() {
    use tantivy::collector::TopDocs;
    use tantivy::query::TermQuery;
    use tantivy::schema::{IndexRecordOption, Value};
    use tantivy::{TantivyDocument, Term};

    let (_dir, mut index) = handle();
    let text = format!("{} refreshed", "а".repeat(999));
    assert_eq!(text.chars().count(), 1009);
    index
        .add_chunks(&[chunk(1, 0, "T", &text)])
        .expect("add chunks");
    index.commit().expect("commit");

    let reader = index.reader();
    reader.reload().expect("reload");
    let searcher = reader.searcher();
    let schema = searcher.schema();
    let text_field = schema.get_field("text").expect("text field");
    let len_field = schema.get_field("text_len").expect("text_len field");
    let query = TermQuery::new(
        // SQ7: prose words are indexed by stem, so `refreshed` is `refresh`.
        Term::from_field_text(text_field, "refresh"),
        IndexRecordOption::WithFreqsAndPositions,
    );
    let top = searcher
        .search(&query, &TopDocs::with_limit(1).order_by_score())
        .expect("term search");
    let (_, address) = top[0];
    let document: TantivyDocument = searcher.doc(address).expect("stored doc");
    let stored = document
        .get_first(len_field)
        .and_then(|value| value.as_u64())
        .expect("stored text_len");
    assert_eq!(
        stored,
        1009,
        "text_len must count Unicode chars, not {} bytes",
        text.len()
    );
}

/// FR-5: the long-chunk penalty uses the char cap plus the overlap allowance,
/// so a chunk sized within `MAX_CHUNK_CHARS + CHUNK_OVERLAP` is not punished
/// while an oversized one is.
#[test]
fn penalty_uses_char_threshold_and_overlap() {
    let (_dir, mut index) = handle();
    let legal = format!("{} refreshed", "а".repeat(1590));
    let oversized = format!("{} refreshed", "а".repeat(1690));
    assert_eq!(legal.chars().count(), 1600);
    assert_eq!(oversized.chars().count(), 1700);
    index
        .add_chunks(&[chunk(1, 0, "T", &legal), chunk(2, 0, "T", &oversized)])
        .expect("add chunks");
    index.commit().expect("commit");

    let hits = index.search("refreshed", 5).expect("search");
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].doc_id, 1, "legal chunk must rank first: {hits:?}");
    assert!(
        hits[0].score > hits[1].score * 1.1,
        "oversized chunk must be penalized: {hits:?}"
    );
}
