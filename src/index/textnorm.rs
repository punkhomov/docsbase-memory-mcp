//! Minimal, dependency-free Unicode folding and script detection (FR-14).
//!
//! Folds what the multilingual pipeline needs, preserving case so the FR-10
//! ALL-CAPS stem guard still sees it: `ё`/`Ё` → `е`/`Е`, fullwidth ASCII →
//! ASCII, Turkish dotted `İ` → `I` (the tokenizer lowercases token text).
//! Arabic normalization lands with SQ10.
//!
//! All folds are one-to-one character replacements, so token offsets stay
//! aligned with the source text except for byte lengths of width-folded
//! characters (fullwidth → ASCII shrinks three bytes to one).

use std::borrow::Cow;

/// Writing system of a segment, chosen by predominant letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Cyrillic,
    Cjk,
    Arabic,
    Other,
}

/// Folds a raw segment; returns the input untouched when nothing changes.
#[must_use]
pub fn normalize(raw: &str) -> Cow<'_, str> {
    if !raw.chars().any(needs_fold) {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        out.push(fold(ch));
    }
    Cow::Owned(out)
}

fn needs_fold(ch: char) -> bool {
    matches!(ch, 'ё' | 'Ё' | 'İ' | '\u{FF01}'..='\u{FF5E}')
}

fn fold(ch: char) -> char {
    match ch {
        'ё' | 'Ё' => {
            if ch == 'ё' {
                'е'
            } else {
                'Е'
            }
        }
        'İ' => 'I',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch),
        _ => ch,
    }
}

/// Script of a letter; `None` for digits, punctuation and unknown letters.
fn script_char(ch: char) -> Option<Script> {
    if !ch.is_alphabetic() {
        return None;
    }
    match ch as u32 {
        0x0041..=0x024F | 0x1E00..=0x1EFF => Some(Script::Latin),
        0x0400..=0x052F => Some(Script::Cyrillic),
        0x1100..=0x11FF
        | 0x3040..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xFF66..=0xFF9F => Some(Script::Cjk),
        0x0600..=0x06FF | 0x0750..=0x077F => Some(Script::Arabic),
        _ => None,
    }
}

/// Script of `s` by predominant letter count; ties keep enum order and a
/// letter-less string is [`Script::Other`].
#[must_use]
pub fn script_of(s: &str) -> Script {
    let mut counts = [
        (Script::Latin, 0_usize),
        (Script::Cyrillic, 0),
        (Script::Cjk, 0),
        (Script::Arabic, 0),
    ];
    for ch in s.chars() {
        if let Some(script) = script_char(ch)
            && let Some(entry) = counts.iter_mut().find(|(known, _)| *known == script)
        {
            entry.1 += 1;
        }
    }
    let mut best = Script::Other;
    let mut best_count = 0_usize;
    for (script, count) in counts {
        if count > best_count {
            best_count = count;
            best = script;
        }
    }
    best
}

/// Splits `s` into single-script runs at letter boundaries; digits and
/// punctuation stay attached to the current run, so `OpenSearchを検索` yields
/// `["OpenSearch", "を検索"]` (byte offsets preserved).
#[must_use]
pub fn split_script_runs(s: &str) -> Vec<(usize, &str)> {
    let mut runs: Vec<(usize, &str)> = Vec::new();
    let mut start = 0_usize;
    let mut current: Option<Script> = None;
    for (index, ch) in s.char_indices() {
        let Some(script) = script_char(ch) else {
            continue;
        };
        match current {
            None => current = Some(script),
            Some(active) if active != script => {
                if index > start {
                    runs.push((start, &s[start..index]));
                }
                start = index;
                current = Some(script);
            }
            Some(_) => {}
        }
    }
    if start < s.len() {
        runs.push((start, &s[start..]));
    }
    runs
}
