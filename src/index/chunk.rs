//! Heading-based chunking (FR-22).
//!
//! Splits markdown into sections at headings, keeps a breadcrumb, never cuts
//! inside code fences or tables, and breaks overly long sections only at blank
//! lines outside those regions.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Kind of a chunk's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkKind {
    /// Regular markdown prose.
    Prose,
    /// A section that is a single fenced code block.
    Code {
        /// Fence info string, if any.
        lang: Option<String>,
    },
    /// A markdown table.
    Table,
}

/// One indexed chunk with citation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Owning document; set by the caller, `0` in unit tests.
    pub doc_id: i64,
    /// Sequence within the document.
    pub seq: u32,
    /// Breadcrumb of heading texts, outermost first.
    pub heading_path: Vec<String>,
    /// Content kind.
    pub kind: ChunkKind,
    /// First line, 1-based inclusive.
    pub line_start: u32,
    /// Last line, 1-based inclusive.
    pub line_end: u32,
    /// Chunk text.
    pub text: String,
}

/// Splits `body` into chunks no larger than `max_chunk_chars` where possible.
#[must_use]
pub fn chunk_markdown(body: &str, max_chunk_chars: usize) -> Vec<Chunk> {
    if body.is_empty() {
        return Vec::new();
    }
    let starts = line_starts(body);
    let mut chunks = Vec::new();
    let mut seq = 0_u32;

    for section in sections(body, &starts) {
        if section.start >= section.end {
            continue;
        }
        let text = &body[section.start..section.end];
        for (start, end) in split_ranges(text, max_chunk_chars) {
            let piece = &text[start..end];
            if piece.trim().is_empty() {
                continue;
            }
            chunks.push(Chunk {
                doc_id: 0,
                seq,
                heading_path: section.headings.clone(),
                kind: classify(piece),
                line_start: line_of(&starts, section.start + start),
                line_end: line_of(&starts, section.start + end - 1),
                text: piece.to_owned(),
            });
            seq += 1;
        }
    }
    chunks
}

#[derive(Debug, Clone)]
struct Section {
    start: usize,
    end: usize,
    headings: Vec<String>,
}

fn sections(body: &str, starts: &[usize]) -> Vec<Section> {
    let parser = Parser::new_ext(
        body,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    let mut sections = Vec::new();
    let mut headings: Vec<String> = Vec::new();
    let mut section_start = 0_usize;
    let mut heading: Option<(usize, String)> = None;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let boundary = line_start_at(starts, range.start);
                if boundary > section_start {
                    sections.push(Section {
                        start: section_start,
                        end: boundary,
                        headings: headings.clone(),
                    });
                }
                section_start = boundary;
                heading = Some((level as usize, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, text)) = heading.take() {
                    headings.truncate(level.saturating_sub(1));
                    headings.push(normalize(&text));
                }
            }
            Event::Text(text) | Event::Code(text) | Event::InlineHtml(text) => {
                if let Some((_, buffer)) = heading.as_mut() {
                    buffer.push_str(&text);
                }
            }
            _ => {}
        }
    }
    sections.push(Section {
        start: section_start,
        end: body.len(),
        headings,
    });
    sections
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify(text: &str) -> ChunkKind {
    let mut lines = text.lines();
    if lines
        .clone()
        .next()
        .is_some_and(|first| first.trim_start().starts_with('#'))
    {
        lines.next();
    }
    let content: Vec<&str> = lines.filter(|line| !line.trim().is_empty()).collect();
    let Some(first) = content.first() else {
        return ChunkKind::Prose;
    };
    if let Some((marker, len)) = fence_marker(first) {
        let closed = content
            .last()
            .is_some_and(|line| is_fence_close(line, marker, len));
        if closed {
            let info = first.trim_start()[len..].trim();
            return ChunkKind::Code {
                lang: if info.is_empty() {
                    None
                } else {
                    Some(info.to_owned())
                },
            };
        }
    }
    if first.trim_start().starts_with('|') {
        return ChunkKind::Table;
    }
    ChunkKind::Prose
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    (len >= 3).then_some((marker, len))
}

fn is_fence_close(line: &str, marker: char, open_len: usize) -> bool {
    let trimmed = line.trim_start();
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    len >= open_len && trimmed[len..].trim().is_empty()
}

fn split_ranges(text: &str, max: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut pos = 0_usize;
    let mut fence: Option<(char, usize)> = None;

    for line in text.split_inclusive('\n') {
        if let Some((marker, len)) = fence {
            if is_fence_close(line, marker, len) {
                fence = None;
            }
        } else if let Some(opened) = fence_marker(line) {
            fence = Some(opened);
        }
        pos += line.len();
        let boundary = fence.is_none() && line.trim().is_empty();
        if boundary && pos - start > max {
            ranges.push((start, pos));
            start = pos;
        }
    }
    if start < text.len() {
        ranges.push((start, text.len()));
    }
    if ranges.is_empty() {
        ranges.push((0, text.len()));
    }
    ranges
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0_usize];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_start_at(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(index) => starts[index],
        Err(index) => starts[index.saturating_sub(1)],
    }
}

fn line_of(starts: &[usize], offset: usize) -> u32 {
    let line = match starts.binary_search(&offset) {
        Ok(index) => index + 1,
        Err(index) => index.max(1),
    };
    u32::try_from(line).unwrap_or(u32::MAX)
}
