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
/// (≈10% of the char cap); consumed by the recursive splitter (SQ4) and by
/// the long-chunk penalty threshold (`tantivy_index`).
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

#[cfg(test)]
thread_local! {
    static CAPTURE_PARSER_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn capture_parser_entries() -> usize {
    CAPTURE_PARSER_ENTRIES.get()
}

/// Allocation-free allowance for eager parser trees/ranges before construction.
pub(crate) fn capture_parser_charge(body: &str) -> crate::error::Result<u64> {
    let denied = || crate::error::Error::Admission {
        message: "parser capture preflight overflow".into(),
    };
    let bytes = body.len() as u64;
    if let Some(lines) = simple_ascii_lines(body) {
        // pulldown-cmark 0.13.5 firstpass preallocates max(128, len/32)
        // nodes even for one borrowed plain-text run. Account that capacity
        // separately from line-proportional growth, not 96 bytes per source byte.
        // Only unindented ASCII words/spaces, blank lines and ATX headings
        // qualify: no inline delimiters, attributes, HTML, links or code buffers.
        // 96 bytes/node covers Node<Item>; 512/line covers growth/spine/ranges.
        (bytes / 32)
            .max(128)
            .checked_mul(96)
            .and_then(|n| {
                lines
                    .checked_mul(512)
                    .and_then(|lines| n.checked_add(lines))
            })
            .and_then(|n| n.checked_add(8192))
            .ok_or_else(denied)
    } else {
        bytes.checked_mul(96).ok_or_else(denied)
    }
}

fn simple_ascii_lines(body: &str) -> Option<u64> {
    let mut count = 0_u64;
    for line in body.split('\n') {
        count = count.checked_add(1)?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        let text = if line.starts_with('#') {
            let text = line.trim_start_matches('#');
            if line.len() - text.len() > 6 || !text.starts_with(' ') {
                return None;
            }
            text
        } else {
            // Indented code can allocate an owned whole-block buffer.
            if line.starts_with(' ') && !line.bytes().all(|byte| byte == b' ') {
                return None;
            }
            line
        };
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b' ')
        {
            return None;
        }
    }
    Some(count)
}

/// Conservative logical peak before frontmatter/chunk/JSON/apply allocations.
/// Parser internals and range arrays are bounded by source bytes; retained heading
/// breadcrumbs are additionally bounded per section (including all chunk clones).
pub(crate) fn capture_charge(body: &str, cap: usize) -> crate::error::Result<u64> {
    use crate::error::Error;
    let denied = || Error::Admission {
        message: "chunk capture preflight overflow".into(),
    };
    let mut total = capture_parser_charge(body)?;
    let mut headings = [0_u64; 6];
    let mut start = 0;
    let mut previous_heading = 0_u64;
    let mut section = |start: usize, end: usize, heading: u64| -> crate::error::Result<()> {
        let text = &body[start..end];
        let pieces = text
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            .saturating_add(text.len() / cap.saturating_sub(CHUNK_OVERLAP).max(1))
            .saturating_add(2) as u64;
        let records = heading
            .checked_mul(8)
            .and_then(|n| n.checked_add(2048))
            .and_then(|n| n.checked_mul(pieces))
            .ok_or_else(denied)?;
        total = total.checked_add(records).ok_or_else(denied)?;
        Ok(())
    };
    #[cfg(test)]
    CAPTURE_PARSER_ENTRIES.set(CAPTURE_PARSER_ENTRIES.get() + 1);
    for (event, range) in Parser::new_ext(body, Options::all()).into_offset_iter() {
        if let Event::Start(Tag::Heading { level, .. }) = event {
            section(start, range.start, previous_heading)?;
            let level = level as usize;
            headings[level - 1] = (range.end - range.start) as u64;
            headings[level..].fill(0);
            previous_heading = headings.iter().sum();
            start = range.start;
        }
    }
    section(start, body.len(), previous_heading)?;
    Ok(total)
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
    /// Set when the piece absorbed a preceding heading caption: `true` for
    /// every atomic fence/table block (they always absorb an adjacent
    /// caption) and for prose pieces that merged a heading-only piece.
    /// Overlap is not added afterwards (for prose this keeps the cap +
    /// overlap bound; atomic blocks are exempt by the SC-5 delta).
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
/// cap + overlap and the merged piece skips overlap, keeping SC-5. A push/pop
/// pass keeps the backward re-check without O(n) removal inside the scan
/// (SQ16).
fn merge_heading_only(pieces: Vec<Piece>, text: &str, max: usize) -> Vec<Piece> {
    let mut merged: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        merged.push(piece);
        while merged.len() >= 2 {
            let current = merged[merged.len() - 2];
            let next = merged[merged.len() - 1];
            let current_chars = text[current.start..current.end].chars().count();
            let next_chars = text[next.start..next.end].chars().count();
            let heading_only = !current.atomic
                && !next.atomic
                && current.end == next.start
                && current_chars + next_chars <= max + CHUNK_OVERLAP
                && text[current.start..current.end]
                    .lines()
                    .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'));
            if !heading_only {
                break;
            }
            // Merge `current` into `next`: keep the newer piece, drop the
            // older one (remove at len-2 shifts only the last element).
            let last = merged.len() - 1;
            merged[last].start = current.start;
            merged[last].heading_merged = true;
            merged.remove(last - 1);
        }
    }
    merged
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
            debug_assert!(
                !pieces_of_line.is_empty(),
                "a non-empty oversized line must yield at least one piece"
            );
            let Some(last) = pieces_of_line.pop() else {
                // Invariant broken: keep the line instead of silently dropping
                // the rest of the run (SQ13).
                push_piece(pieces, text, line_start, line_start + line.len());
                piece_start = line_start + line.len();
                chars = 0;
                continue;
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
#[must_use]
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

#[cfg(test)]
mod parser_allowance_tests {
    use super::*;

    #[test]
    fn simple_ascii_allowance_accounts_initial_tree_and_keeps_complex_markdown_fallback() {
        let text = "# Heading\r\n\nwords 123\n";
        let lines = simple_ascii_lines(text).unwrap();
        assert_eq!(lines, 4);
        let allowance = capture_parser_charge(text).unwrap();
        assert!(allowance >= 128 * 96 + lines * 512);
        let events = Parser::new_ext(text, Options::all()).count();
        assert!(
            events as u64 <= lines * 8,
            "simple blocks have bounded line structure"
        );
        for text in [
            "    indented code",
            "\tcode",
            "words # later",
            "#unspaced",
            "####### heading",
            "[link](url)",
            "# Heading {id}",
            "a|b\n---|---",
            "```code\nbody\n```",
            "<html>",
            "Unicode é",
            "words\rwords",
        ] {
            assert_eq!(
                simple_ascii_lines(text),
                None,
                "complex input keeps byte-based bound: {text:?}"
            );
            assert_eq!(capture_parser_charge(text).unwrap(), text.len() as u64 * 96);
        }
    }
}
