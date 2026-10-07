//! Heading-based chunking (FR-22), recursive for oversized sections (FR-4).
//!
//! Splits markdown into sections at headings, keeps a breadcrumb, never cuts
//! inside code fences or tables, and bounds oversized sections by paragraphs,
//! lines and token boundaries (emergency char cut only for unbroken blobs);
//! consecutive pieces share a small tail overlap for citation context.

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

/// Tail of the previous chunk carried into the next one at paragraph splits
/// (≈10% of the char cap); consumed by the recursive splitter (SQ4).
pub const CHUNK_OVERLAP: usize = 150;

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

/// One bounded piece produced by the splitter.
#[derive(Debug, Clone, Copy)]
struct Piece {
    start: usize,
    end: usize,
    /// Fence or table block: never subdivided and never overlapped.
    atomic: bool,
    /// A heading caption was merged into this piece: no overlap is added, so
    /// the cap + overlap bound still holds.
    heading_merged: bool,
}

fn split_ranges(text: &str, max: usize) -> Vec<(usize, usize)> {
    if text.chars().count() <= max {
        // Sections within the cap keep their v1 shape (no segmentation, no
        // synthetic boundaries): recursive treatment is for oversized content.
        return vec![(0, text.len())];
    }
    let pieces = bounded_pieces(text, max);
    let pieces = merge_heading_only(pieces, text, max);
    apply_overlap(text, &pieces, max)
}

/// A heading-only piece directly before the next piece is a caption: merge it
/// back so chunks never consist of a bare heading. The merge budget is
/// cap + overlap and the merged piece skips overlap, keeping SC-5.
fn merge_heading_only(mut pieces: Vec<Piece>, text: &str, max: usize) -> Vec<Piece> {
    let mut index = 0_usize;
    while index + 1 < pieces.len() {
        let current = pieces[index];
        let next = pieces[index + 1];
        let current_chars = text[current.start..current.end].chars().count();
        let next_chars = text[next.start..next.end].chars().count();
        let heading_only = !current.atomic
            && !next.atomic
            && current.end == next.start
            && current_chars + next_chars <= max + CHUNK_OVERLAP
            && text[current.start..current.end]
                .lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'));
        if heading_only {
            pieces[index + 1].start = current.start;
            pieces[index + 1].heading_merged = true;
            pieces.remove(index);
            index = index.saturating_sub(1);
        } else {
            index += 1;
        }
    }
    pieces
}

/// Splits `text` into pieces ≤ `max` chars: prose runs are cut at line
/// boundaries, oversized lines at token boundaries (emergency char cut only
/// for unbroken blobs); fenced code and tables stay atomic (FR-4).
fn bounded_pieces(text: &str, max: usize) -> Vec<Piece> {
    let lines: Vec<(usize, usize)> = line_ranges(text);
    let mut pieces = Vec::new();
    let mut index = 0_usize;
    while index < lines.len() {
        let (line_start, line_end) = lines[index];
        let line = &text[line_start..line_end];
        if let Some((marker, len)) = fence_marker(line) {
            let mut end = index + 1;
            while end < lines.len() {
                let (s, e) = lines[end];
                end += 1;
                if is_fence_close(&text[s..e], marker, len) {
                    break;
                }
            }
            let start = merge_preceding_heading(&mut pieces, text, line_start);
            pieces.push(Piece {
                start,
                end: lines[end - 1].1,
                atomic: true,
                heading_merged: true,
            });
            index = end;
            continue;
        }
        if is_table_line(line) {
            let mut end = index + 1;
            while end < lines.len() && is_table_line(&text[lines[end].0..lines[end].1]) {
                end += 1;
            }
            let start = merge_preceding_heading(&mut pieces, text, line_start);
            pieces.push(Piece {
                start,
                end: lines[end - 1].1,
                atomic: true,
                heading_merged: true,
            });
            index = end;
            continue;
        }
        let mut end = index;
        while end < lines.len() {
            let (s, e) = lines[end];
            let candidate = &text[s..e];
            if fence_marker(candidate).is_some() || is_table_line(candidate) {
                break;
            }
            end += 1;
        }
        bound_prose(&mut pieces, text, line_start, lines[end - 1].1, max);
        index = end;
    }
    pieces
}

/// A heading-only prose tail immediately before a fence/table is its caption
/// and joins the atomic block, so the code chunk keeps its heading line.
fn merge_preceding_heading(pieces: &mut Vec<Piece>, text: &str, block_start: usize) -> usize {
    let Some(last) = pieces.last() else {
        return block_start;
    };
    if last.end != block_start || last.atomic {
        return block_start;
    }
    let heading_only = text[last.start..last.end]
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'));
    if !heading_only {
        return block_start;
    }
    let start = last.start;
    pieces.pop();
    start
}

/// Bounds one prose run: strict line accumulation up to `max`, oversized
/// single lines split at token boundaries (emergency char cut for blobs).
fn bound_prose(pieces: &mut Vec<Piece>, text: &str, start: usize, end: usize, max: usize) {
    let run = &text[start..end];
    let mut piece_start = start;
    let mut chars = 0_usize;
    let mut offset = 0_usize;
    for line in run.split_inclusive('\n') {
        let line_start = start + offset;
        offset += line.len();
        let line_chars = line.chars().count();
        if chars > 0 && chars + line_chars > max {
            push_piece(pieces, text, piece_start, line_start);
            piece_start = line_start;
            chars = 0;
        }
        if chars == 0 && line_chars > max {
            let mut pieces_of_line = split_oversized_line(line, max);
            let Some(last) = pieces_of_line.pop() else {
                return;
            };
            for (from, to) in pieces_of_line {
                push_piece(pieces, text, line_start + from, line_start + to);
            }
            piece_start = line_start + last.0;
            chars = line[last.0..last.1].chars().count();
        } else {
            chars += line_chars;
        }
    }
    push_piece(pieces, text, piece_start, end);
}

/// Emits a piece unless it is whitespace-only (dropped like before).
fn push_piece(pieces: &mut Vec<Piece>, text: &str, start: usize, end: usize) {
    if start < end && !text[start..end].trim().is_empty() {
        pieces.push(Piece {
            start,
            end,
            atomic: false,
            heading_merged: false,
        });
    }
}

/// Splits one overlong line: cut after the last token boundary inside each
/// `max`-char window; char-boundary emergency cut when no boundary exists.
fn split_oversized_line(line: &str, max: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut chars = 0_usize;
    for (index, _) in line.char_indices() {
        chars += 1;
        if chars <= max {
            continue;
        }
        let boundary = line[start..index]
            .char_indices()
            .rev()
            .find(|(_, ch)| is_token_boundary(*ch))
            .map(|(offset, ch)| start + offset + ch.len_utf8())
            .filter(|cut| *cut > start);
        let cut = boundary.unwrap_or(index);
        ranges.push((start, cut));
        chars = line[cut..index].chars().count() + 1;
        start = cut;
    }
    if start < line.len() {
        ranges.push((start, line.len()));
    }
    ranges
}

/// A token boundary for the splitter: whitespace or punctuation other than
/// `_` (identifiers keep their underscore joins intact).
fn is_token_boundary(ch: char) -> bool {
    ch.is_whitespace() || (ch.is_ascii_punctuation() && ch != '_')
}

/// Prepends a bounded tail of the previous piece to each following piece so
/// the citation still carries context (FR-4); atomic blocks are not overlapped.
/// The tail is at most [`CHUNK_OVERLAP`] and at most a tenth of the cap.
fn apply_overlap(text: &str, pieces: &[Piece], max: usize) -> Vec<(usize, usize)> {
    let take = CHUNK_OVERLAP.min(max / 10);
    let mut ranges = Vec::with_capacity(pieces.len());
    for (index, piece) in pieces.iter().enumerate() {
        let start = if index == 0
            || take == 0
            || piece.atomic
            || pieces[index - 1].atomic
            || piece.heading_merged
            || pieces[index - 1].heading_merged
        {
            piece.start
        } else {
            overlap_start(text, pieces[index - 1].start, pieces[index - 1].end, take)
                .unwrap_or(piece.start)
        };
        ranges.push((start, piece.end));
    }
    ranges
}

/// Longest suffix of `[prev_start..prev_end)` (≤ `take` chars) that starts at
/// a token boundary; `None` when the tail has no boundary (blob).
fn overlap_start(text: &str, prev_start: usize, prev_end: usize, take: usize) -> Option<usize> {
    let prev = &text[prev_start..prev_end];
    let total = prev.chars().count();
    let take = total.min(take);
    if take == 0 {
        return None;
    }
    let skip = total - take;
    let tail_start = prev
        .char_indices()
        .nth(skip)
        .map_or(prev_start, |(offset, _)| prev_start + offset);
    let mut position = tail_start;
    while position < prev_end {
        let is_boundary = position == prev_start
            || text[..position]
                .chars()
                .next_back()
                .is_some_and(is_token_boundary);
        if is_boundary && !text[position..prev_end].trim().is_empty() {
            return Some(position);
        }
        let ch = text[position..].chars().next()?;
        position += ch.len_utf8();
    }
    None
}

fn line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    for line in text.split_inclusive('\n') {
        ranges.push((start, start + line.len()));
        start += line.len();
    }
    ranges
}

fn is_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && !trimmed.is_empty()
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
