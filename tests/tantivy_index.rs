use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::tantivy_index::{IndexHandle, chunk_id};
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
    let index = IndexHandle::open_or_create(dir.path()).expect("open");
    (dir, index)
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
fn bad_query_is_query_error() {
    let (_dir, index) = handle();
    let err = index.search("\"unbalanced", 5).expect_err("must fail");
    assert!(
        matches!(err, docsbase_memory::error::Error::Query { .. }),
        "{err:?}"
    );
    assert_eq!(err.mcp_code(), -32014);
}
