use docsbase_memory::index::chunk::{CHUNK_OVERLAP, ChunkKind, chunk_markdown};

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
    let code = chunks
        .iter()
        .find(|chunk| chunk.text.contains("let y = 2;"))
        .expect("code chunk");
    assert!(
        code.text.contains("let x = 1;"),
        "fence must stay intact: {:?}",
        code.text
    );
    assert_eq!(
        code.kind,
        ChunkKind::Code {
            lang: Some("rust".to_owned())
        }
    );
}

#[test]
fn nested_fence_not_split() {
    let body = "# T\n\n````\n```\ninner\n\nstill inner\n```\n````\n";
    let chunks = chunk_markdown(body, 6);
    let fenced: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("still inner"))
        .collect();
    assert_eq!(fenced.len(), 1, "{chunks:?}");
    assert!(
        fenced[0].text.contains("````"),
        "fence must stay intact: {}",
        fenced[0].text
    );
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
    let code = chunks
        .iter()
        .find(|chunk| chunk.text.contains("plain"))
        .expect("code chunk");
    assert_eq!(code.kind, ChunkKind::Code { lang: None });
}

#[test]
fn fence_with_trailing_prose_is_prose() {
    let body = "# T\n\n```rust\ncode();\n```\ntrailing prose\n";
    let chunks = chunk_markdown(body, 10_000);
    let chunk = chunks
        .iter()
        .find(|chunk| chunk.text.contains("code();"))
        .expect("code chunk");
    assert_eq!(chunk.kind, ChunkKind::Prose, "{chunks:?}");
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
    let chunks = chunk_markdown(body, 16);
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

/// FR-4: an oversized paragraph is cut at line/token boundaries instead of
/// staying unbounded; all words survive, chunks stay ≤ cap + overlap.
#[test]
fn oversized_paragraph_splits_at_token_boundary() {
    let words: Vec<String> = (0..120).map(|i| format!("слово{i:03}")).collect();
    let paragraph = words.join(" ");
    let body = format!("# T\n\n{paragraph}\n");
    let chunks = chunk_markdown(&body, 60);
    assert!(chunks.len() > 1, "{chunks:?}");
    for chunk in &chunks {
        assert!(
            chunk.text.chars().count() <= 60 + CHUNK_OVERLAP,
            "chunk over cap+overlap: {} chars",
            chunk.text.chars().count()
        );
    }
    let joined: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    for word in [&words[0], &words[59], &words[119]] {
        assert!(joined.contains(word.as_str()), "missing {word}");
    }
    // No word may be cut in half: every token is a complete source word.
    for chunk in &chunks {
        for token in chunk.text.split_whitespace() {
            assert!(
                words.iter().any(|word| word == token) || token == "#" || token == "T",
                "broken token {token:?} in {:?}",
                chunk.text
            );
        }
    }
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

/// FR-5: the cap counts Unicode chars, not bytes — a 1402-char RU section
/// (2804 bytes) must not split at a middle blank line against a 1500-char cap.
#[test]
fn cap_counts_unicode_chars_not_bytes() {
    let body = format!("{}\n\n{}", "а".repeat(800), "а".repeat(600));
    assert_eq!(body.chars().count(), 1402);
    assert!(body.len() > 2800, "bytes >> chars: {}", body.len());
    let chunks = chunk_markdown(&body, 1500);
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert_eq!(chunks[0].text.chars().count(), 1402);
}

/// FR-4/SC-5: a long headingless section yields bounded chunks with valid
/// source line ranges.
#[test]
fn section_bounded_with_valid_lines() {
    let mut body = String::new();
    for paragraph in 0..40 {
        let words: Vec<String> = (0..8).map(|w| format!("п{paragraph:02}с{w}")).collect();
        body.push_str(&words.join(" "));
        body.push_str("\n\n");
    }
    let total_lines = body.lines().count();
    let chunks = chunk_markdown(&body, 600);
    assert!(chunks.len() > 1, "section must split: {}", chunks.len());
    for chunk in &chunks {
        let chars = chunk.text.chars().count();
        assert!(chars <= 600 + 600 / 10, "over cap+overlap: {chars}");
        assert!(chunk.line_start >= 1 && chunk.line_end >= chunk.line_start);
        let line_end = usize::try_from(chunk.line_end).expect("u32 fits");
        let line_start = usize::try_from(chunk.line_start).expect("u32 fits");
        assert!(line_end <= total_lines, "{chunk:?}");
        let slice = body
            .lines()
            .skip(line_start - 1)
            .take(line_end - line_start + 1)
            .collect::<Vec<_>>()
            .join("\n");
        let first = chunk.text.split_whitespace().next().expect("token");
        assert!(
            slice.contains(first),
            "line range must cover {first:?}: {chunk:?}"
        );
    }
    let joined: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    assert!(joined.contains("п39с7"), "tail must be covered: {joined}");
}

/// FR-4: consecutive pieces share a tail so citations keep context.
#[test]
fn overlap_carries_tail() {
    let alpha: Vec<String> = (0..40).map(|i| format!("альфа{i:02}")).collect();
    let beta: Vec<String> = (0..40).map(|i| format!("бета{i:02}")).collect();
    let body = format!("{}\n\n{}\n", alpha.join(" "), beta.join(" "));
    let chunks = chunk_markdown(&body, 100);
    assert!(chunks.len() >= 3, "{chunks:?}");
    let shared = chunks
        .windows(2)
        .filter(|pair| {
            pair[0]
                .text
                .split_whitespace()
                .last()
                .is_some_and(|last| pair[1].text.contains(last))
        })
        .count();
    assert!(shared >= 1, "no overlapping tail found: {chunks:?}");
}

/// FR-4: punctuation (except `_`) is a token boundary too — a comma-separated
/// list must not be cut in the middle of an identifier.
#[test]
fn punctuation_line_cuts_at_boundaries() {
    let ids: Vec<String> = (0..200).map(|i| format!("tok{i:03}")).collect();
    let body = format!("{}\n", ids.join(","));
    let chunks = chunk_markdown(&body, 64);
    assert!(chunks.len() > 1, "{chunks:?}");
    let mut seen = 0;
    for chunk in &chunks {
        for part in chunk.text.split(',').filter(|part| !part.trim().is_empty()) {
            let part = part.trim();
            assert!(
                ids.iter().any(|id| id == part),
                "broken token {part:?} in {:?}",
                chunk.text
            );
            seen += 1;
        }
    }
    assert!(seen >= ids.len(), "all ids covered: {seen} < {}", ids.len());
}

/// FR-4: an overlong identifier line is cut at token boundaries; an unbroken
/// blob falls back to a char cut without losing content.
#[test]
fn identifier_line_and_blob_splitting() {
    let ids: Vec<String> = (0..40)
        .map(|i| format!("assessment_plan_id_{i:02}"))
        .collect();
    let body = format!("{}\n", ids.join(" "));
    let chunks = chunk_markdown(&body, 100);
    assert!(chunks.len() > 1, "{chunks:?}");
    for chunk in &chunks {
        for token in chunk.text.split_whitespace() {
            assert!(
                ids.iter().any(|id| id == token),
                "broken identifier {token:?} in {:?}",
                chunk.text
            );
        }
    }
    let joined: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    for id in [&ids[0], &ids[19], &ids[39]] {
        assert!(joined.contains(id.as_str()), "missing {id}");
    }

    let blob = "б".repeat(500);
    let chunks = chunk_markdown(&blob, 100);
    assert_eq!(chunks.len(), 5, "emergency char cuts: {chunks:?}");
    let joined: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    assert_eq!(joined, blob, "emergency cuts must not lose content");
    for chunk in &chunks {
        assert!(chunk.text.chars().count() <= 100);
    }
}

/// FR-4/SC-5 delta (SQ15): an oversized fence stays atomic — code blocks are
/// never split (probe: 5018 chars at max=1500), the content is intact and the
/// line span still points at real source lines. This is the deliberate
/// exception pinned by the SC-5 delta marker.
#[test]
fn oversized_fence_stays_atomic() {
    let code = "x".repeat(5000);
    let body = format!("# T\n\n```rust\n{code}\n```\n\nafter\n");
    let chunks = chunk_markdown(&body, 1500);
    let fence = chunks
        .iter()
        .find(|chunk| chunk.text.contains("xxxx"))
        .expect("fence chunk");
    assert!(
        fence.text.chars().count() > 1500 + CHUNK_OVERLAP,
        "probe premise: fence chunk exceeds cap+overlap: {} chars",
        fence.text.chars().count()
    );
    assert_eq!(
        fence.kind,
        ChunkKind::Code {
            lang: Some("rust".to_owned())
        }
    );
    assert!(fence.text.contains(&code), "fence content must be intact");
    assert_eq!(chunks.len(), 2, "fence + trailing prose: {chunks:?}");
    assert_eq!(
        (fence.line_start, fence.line_end),
        (1, 5),
        "citation lines pin the merged heading + fence span"
    );
}

/// Same pin for tables: a table over cap+overlap stays one atomic chunk.
#[test]
fn oversized_table_stays_atomic() {
    let row = "| a | b |\n";
    let body = format!("# T\n\n{}", row.repeat(400));
    let chunks = chunk_markdown(&body, 1500);
    let table = chunks
        .iter()
        .find(|chunk| chunk.kind == ChunkKind::Table)
        .expect("table chunk");
    assert!(
        table.text.chars().count() > 1500 + CHUNK_OVERLAP,
        "probe premise: table chunk exceeds cap+overlap: {} chars",
        table.text.chars().count()
    );
    assert!(table.text.contains("| a | b |"));
    assert_eq!(chunks.len(), 1, "only the table block: {chunks:?}");
    assert_eq!(
        (table.line_start, table.line_end),
        (1, 402),
        "citation lines pin the merged heading + table span"
    );
    assert_eq!(
        table.text.matches("| a | b |").count(),
        400,
        "all rows intact"
    );
}
