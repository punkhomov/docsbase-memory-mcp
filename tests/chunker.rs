use docsbase_memory::index::chunk::{ChunkKind, chunk_markdown};

fn texts(body: &str, max: usize) -> Vec<String> {
    chunk_markdown(body, max)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
}

#[test]
fn breadcrumb_path() {
    let body = "# A\n\ntext\n\n## B\n\nmore";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[0].heading_path, vec!["A"]);
    assert_eq!(chunks[1].heading_path, vec!["A", "B"]);
    assert_eq!(chunks[0].seq, 0);
    assert_eq!(chunks[1].seq, 1);
}

#[test]
fn fence_not_split() {
    let body = "# T\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n";
    let chunks = chunk_markdown(body, 8);
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert_eq!(
        chunks[0].kind,
        ChunkKind::Code {
            lang: Some("rust".to_owned())
        }
    );
    assert!(chunks[0].text.contains("let y = 2;"), "{}", chunks[0].text);
}

#[test]
fn fenced_section_is_code() {
    let body = "# T\n\n```\nplain\n```\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks[0].kind, ChunkKind::Code { lang: None });
}

#[test]
fn table_intact() {
    let body = "# T\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let chunks = chunk_markdown(body, 4);
    let table_chunks: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("| a | b |"))
        .collect();
    assert_eq!(table_chunks.len(), 1, "{chunks:?}");
    let table = table_chunks[0];
    assert_eq!(table.kind, ChunkKind::Table);
    assert!(table.text.contains("| 3 | 4 |"), "{}", table.text);
}

#[test]
fn long_section_split_at_paragraph() {
    let body = "# T\n\npara one text\n\npara two text\n\npara three text\n\npara four text\n";
    let chunks = chunk_markdown(body, 24);
    assert!(chunks.len() > 1, "{chunks:?}");
    let joined: String = chunks.iter().map(|c| c.text.as_str()).collect();
    for needle in ["para one", "para two", "para three", "para four"] {
        assert!(joined.contains(needle), "missing {needle}: {joined}");
    }
    for chunk in &chunks {
        assert!(
            !chunk.text.trim().is_empty(),
            "empty chunk produced: {chunks:?}"
        );
    }
}

#[test]
fn empty_doc() {
    assert_eq!(chunk_markdown("", 100), Vec::new());
}

#[test]
fn line_ranges_match() {
    let body = "# Title\n\npara one\n\npara two";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].line_start, 1);
    assert_eq!(chunks[0].line_end, 5);
    assert_eq!(chunks[0].text, body);
}

#[test]
fn preamble_has_empty_breadcrumb() {
    let body = "intro text\n\n# H\n\nbody\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[0].heading_path, Vec::<String>::new());
    assert_eq!(chunks[1].heading_path, vec!["H"]);
}

#[test]
fn multibyte_offsets() {
    let body = "# Заголовок\n\nтекст про assessment_plan_id\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].line_start, 1);
    assert!(chunks[0].text.contains("assessment_plan_id"));
    let _ = texts(body, 10_000);
}
