use docsbase_memory::index::chunk::{ChunkKind, chunk_markdown};

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
    let body = "# T\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n";
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
fn nested_fence_not_split() {
    let body = "# T\n\n````\n```\ninner\n\nstill inner\n```\n````\n";
    let chunks = chunk_markdown(body, 6);
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert!(chunks[0].text.contains("still inner"), "{}", chunks[0].text);
}

#[test]
fn unterminated_fence_not_split() {
    let body = "# T\n\n```\ncode\n\nmore\n";
    let chunks = chunk_markdown(body, 4);
    let fenced: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("```"))
        .collect();
    assert_eq!(fenced.len(), 1, "{chunks:?}");
    assert!(fenced[0].text.contains("more"), "{}", fenced[0].text);
    assert!(
        fenced[0].line_start < fenced[0].line_end,
        "unterminated fence must stay one chunk: {chunks:?}"
    );
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
fn kind_is_per_chunk() {
    let body = "# T\n\nintro paragraph\n\n```rust\ncode();\n```\n";
    let chunks = chunk_markdown(body, 12);
    assert!(chunks.len() >= 2, "{chunks:?}");
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk.kind, ChunkKind::Code { .. })),
        "code chunk must be classified as Code: {chunks:?}"
    );
    let prose = chunks
        .iter()
        .find(|chunk| chunk.text.contains("intro paragraph"))
        .expect("prose chunk");
    assert_eq!(prose.kind, ChunkKind::Prose);
}

#[test]
fn long_section_split_at_paragraph() {
    let body = "# T\n\npara one text\n\npara two text\n\npara three text\n\npara four text\n";
    let chunks = chunk_markdown(body, 24);
    assert!(chunks.len() > 1, "{chunks:?}");
    let joined: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
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
fn oversized_paragraph_not_cut() {
    let body = "# T\n\none very long paragraph without blank lines at all\n";
    let chunks = chunk_markdown(body, 4);
    let long: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("one very long"))
        .collect();
    assert_eq!(long.len(), 1, "{chunks:?}");
    assert!(
        long[0].text.contains("without blank lines at all"),
        "{}",
        long[0].text
    );
}

#[test]
fn empty_doc() {
    assert_eq!(chunk_markdown("", 100), Vec::new());
}

#[test]
fn leading_blank_lines_filtered() {
    let body = "\n\n# H\n\ntext\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert_eq!(chunks[0].heading_path, vec!["H"]);
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
fn skipped_heading_levels() {
    let body = "# A\n\n### C\n\nx\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks[0].heading_path, vec!["A"]);
    assert_eq!(chunks[1].heading_path, vec!["A", "C"]);
}

#[test]
fn setext_heading() {
    let body = "Title\n=====\n\ntext\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert_eq!(chunks[0].heading_path, vec!["Title"]);
}

#[test]
fn heading_inline_code_normalized() {
    let body = "# Use `foo` now\n\nx\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks[0].heading_path, vec!["Use foo now"]);
}

#[test]
fn multibyte_offsets() {
    let body = "# Заголовок\n\nтекст про assessment_plan_id\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].line_start, 1);
    assert!(chunks[0].text.contains("assessment_plan_id"));
}

#[test]
fn crlf_line_ranges() {
    let body = "# A\r\n\r\ntext\r\n";
    let chunks = chunk_markdown(body, 10_000);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("text"));
}
