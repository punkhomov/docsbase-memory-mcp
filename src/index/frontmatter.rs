//! Flat frontmatter parsing (FR-18).
//!
//! Only a leading `---` block of `key: value` pairs is understood; nested YAML
//! is intentionally out of scope. Malformed content never fails indexing: the
//! parser returns an empty metadata set instead.

/// Metadata extracted from a leading frontmatter block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// `title:` value, if present.
    pub title: Option<String>,
    /// Comma-separated `tags:` values.
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
        if trimmed == "---" {
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
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => {
                let value = unquote(value);
                if !value.is_empty() {
                    meta.title = Some(value);
                }
            }
            "tags" => {
                meta.tags = value
                    .split(',')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(unquote)
                    .collect();
            }
            _ => {}
        }
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_owned()
}
