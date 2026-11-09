//! Flat frontmatter parsing (FR-18).
//!
//! A leading `---` block supports flat `key: value` pairs and a block list of
//! string tags. General YAML (nested maps, flow lists, escapes) is out of scope.
//! Malformed content never fails indexing; an unclosed block is left as body.

use std::iter::Peekable;
use std::str::Lines;

/// Metadata extracted from a leading frontmatter block.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Frontmatter {
    /// `title:` value, if present.
    pub title: Option<String>,
    /// `tags:` values from a comma-separated line or a block list.
    pub tags: Vec<String>,
}

/// Splits `text` into frontmatter and body.
///
/// Returns an empty [`Frontmatter`] and the original text when there is no
/// well-formed leading `---` block. CRLF line endings are accepted.
#[must_use]
pub fn parse(text: &str) -> (Frontmatter, &str) {
    let mut meta = Frontmatter::default();
    let Some(after_open) = strip_open(text) else {
        return (meta, text);
    };

    let mut consumed = 0_usize;
    let mut block = String::new();
    let mut closed = false;
    for line in after_open.split_inclusive('\n') {
        consumed += line.len();
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.trim_end() == "---" {
            closed = true;
            break;
        }
        block.push_str(line);
    }
    if !closed {
        return (meta, text);
    }

    parse_block(&mut meta, &block);
    let body_start = text.len() - after_open.len() + consumed;
    (meta, &text[body_start..])
}

fn strip_open(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    rest.strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
}

fn parse_block(meta: &mut Frontmatter, block: &str) {
    let mut lines = block.lines().peekable();
    let mut root_indent = None;
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let indent = indentation(line);
        if *root_indent.get_or_insert(indent) != indent {
            continue;
        }
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => {
                let value = unquote(value);
                if !value.is_empty() {
                    meta.title = Some(value);
                }
            }
            "tags" => {
                meta.tags = if value.is_empty() || value.starts_with('#') {
                    parse_tag_list(&mut lines, indent)
                } else {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(unquote)
                        .collect()
                };
            }
            _ => {}
        }
    }
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn parse_tag_list(lines: &mut Peekable<Lines<'_>>, key_indent: usize) -> Vec<String> {
    let mut tags = Vec::new();
    let mut item_indent = None;
    while let Some(&line) = lines.peek() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            lines.next();
            continue;
        }
        let indent = indentation(line);
        if indent < key_indent || item_indent.is_some_and(|expected| indent != expected) {
            break;
        }
        let Some(value) = trimmed.strip_prefix('-') else {
            break;
        };
        if !value.is_empty() && !value.starts_with(char::is_whitespace) {
            break;
        }
        item_indent = Some(indent);
        let tag = block_tag_value(value.trim());
        if !tag.is_empty() {
            tags.push(tag);
        }
        lines.next();
    }
    tags
}

fn block_tag_value(value: &str) -> String {
    // A second sequence indicator starts a nested list, not a string tag.
    if let Some(rest) = value.strip_prefix('-')
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        return String::new();
    }
    // A quoted item may contain commas, colons and hashes. Only paired quotes
    // are stripped, as for the legacy flat values; YAML escapes are unsupported.
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote)
            && let Some(end) = rest.find(quote)
        {
            return rest[..end].to_owned();
        }
    }
    let mut previous_space = true;
    for (offset, ch) in value.char_indices() {
        if ch == '#' && previous_space {
            return value[..offset].trim_end().to_owned();
        }
        previous_space = ch.is_whitespace();
    }
    value.to_owned()
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    trimmed.to_owned()
}
