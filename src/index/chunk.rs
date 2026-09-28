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
    let sections = sections(body);
    let mut chunks = Vec::new();
    let mut seq = 0_u32;

    for section in sections {
        if section.start >= section.end {
            continue;
        }
        let text = &body[section.start..section.end];
        let kind = classify(text);
        for (start, end) in split_ranges(text, max_chunk_chars) {
            if start >= end {
                continue;
            }
            let abs_start = section.start + start;
            let abs_end = section.start + end;
            chunks.push(Chunk {
                doc_id: 0,
                seq,
                heading_path: section.headings.clone(),
                kind: kind.clone(),
                line_start: line_of(&starts, abs_start),
                line_end: line_of(&starts, abs_end.saturating_sub(1)),
                text: text[start..end].to_owned(),
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

fn sections(body: &str) -> Vec<Section> {
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
                if range.start > section_start {
                    sections.push(Section {
                        start: section_start,
                        end: range.start,
                        headings: headings.clone(),
                    });
                }
                section_start = range.start;
                heading = Some((level as usize, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, text)) = heading.take() {
                    headings.truncate(level.saturating_sub(1));
                    headings.push(text.trim().to_owned());
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, buffer)) = heading.as_mut() {
                    if !buffer.is_empty() {
                        buffer.push(' ');
                    }
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
    let first = first.trim_start();
    if first.starts_with("```") || first.starts_with("~~~") {
        let fence = &first[..3];
        let closed = content.len() >= 2
            && content
                .last()
                .is_some_and(|last| last.trim_start().starts_with(fence));
        if closed {
            let lang = first[3..].trim();
            return ChunkKind::Code {
                lang: if lang.is_empty() {
                    None
                } else {
                    Some(lang.to_owned())
                },
            };
        }
    }
    if first.starts_with('|') {
        return ChunkKind::Table;
    }
    ChunkKind::Prose
}

fn split_ranges(text: &str, max: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut pos = 0_usize;
    let mut in_fence = false;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        pos += line.len();
        let boundary = !in_fence && line.trim().is_empty();
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

fn line_of(starts: &[usize], offset: usize) -> u32 {
    let line = match starts.binary_search(&offset) {
        Ok(index) => index + 1,
        Err(index) => index.max(1),
    };
    u32::try_from(line).unwrap_or(u32::MAX)
}
