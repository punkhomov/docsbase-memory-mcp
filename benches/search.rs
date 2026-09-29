//! Search latency benchmark (NFR-1): BM25 over a 50k-chunk index.

use std::hint::black_box;
use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::tantivy_index::IndexHandle;
use tempfile::TempDir;

const CHUNKS: usize = 50_000;
const BATCH: usize = 5_000;

fn chunk(doc_id: i64, seq: u32, text: String) -> Chunk {
    Chunk {
        doc_id,
        seq,
        heading_path: vec![format!("Section {seq}")],
        kind: ChunkKind::Prose,
        line_start: 1,
        line_end: 10,
        text,
    }
}

fn synthetic_text(index: usize) -> String {
    let mut text = format!(
        "widget{index} search performance benchmark documentation chunk. \
         The identifier doc_{} tracks indexing throughput and latency budgets. ",
        index % 977
    );
    for word in [
        "alpha", "beta", "gamma", "delta", "timing", "query", "corpus",
    ] {
        text.push_str(word);
        text.push(' ');
    }
    text
}

fn build_corpus(dir: &Path) -> IndexHandle {
    let mut index = IndexHandle::open_or_create(dir).expect("index");
    index.mark_rebuilt().expect("mark rebuilt");
    let mut batch = Vec::with_capacity(BATCH);
    for i in 0..CHUNKS {
        batch.push(chunk(
            i64::try_from(i).unwrap_or(i64::MAX),
            u32::try_from(i % 64).unwrap_or(0),
            synthetic_text(i),
        ));
        if batch.len() == BATCH {
            index.add_chunks(&batch).expect("add");
            index.commit().expect("commit");
            batch.clear();
        }
    }
    if !batch.is_empty() {
        index.add_chunks(&batch).expect("add");
        index.commit().expect("commit");
    }
    index
}

fn search(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    let index = build_corpus(dir.path());
    let queries = [
        "search performance widget42",
        "doc_123 indexing throughput",
        "latency budgets alpha gamma",
        "widget49999 corpus timing",
    ];
    let mut counter = 0_usize;
    c.bench_function("search_50k_chunks", |b| {
        b.iter(|| {
            let query = queries[counter % queries.len()];
            counter = counter.wrapping_add(1);
            black_box(index.search(query, 10).expect("search"));
        });
    });
}

criterion_group!(benches, search);
criterion_main!(benches);
