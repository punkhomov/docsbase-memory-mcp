//! Identifier-aware tokenizer (FR-21).
//!
//! Emits the lowercased raw token plus `camelCase` / `snake_case` sub-tokens so
//! technical identifiers (`defineStore`, `assessment_plan_id`,
//! `__bt_tt_getProp`, `X-Request-ID`) rank high on exact matches while prose
//! words match regardless of adjacent punctuation. Standalone prose words are
//! reduced to a `ru`/`en`/`ar` stem (FR-9/FR-13) instead of the surface form:
//! tantivy's phrase query requires every same-position term, so emitting the
//! stem *and* the surface would stop word forms from finding each other
//! (SQ7 ruling).

use std::collections::HashSet;

use tantivy::tokenizer::{
    Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer, Token, TokenStream, Tokenizer,
};

use super::textnorm::{self, Script};

/// Tokenizer name registered in the [`TokenizerManager`].
pub const NAME: &str = "identifier";

/// Maximum emitted token length in Unicode chars (FR-16): longer tokens (hash
/// blobs, giant base64 pieces) are dropped instead of bloating the dictionary.
pub const MAX_TOKEN_CHARS: usize = 40;

/// Version of the tokenizer pipeline (FR-8). Bump on any change of
/// normalization, emitted variants or stemming: indexes record this value and
/// are rebuilt from `SQLite` when it differs. 1 = legacy (indexes without a
/// version file are considered stale); 2 = first versioned pipeline;
/// 3 = one position per segment + token length limit; 4 = ru/en stemming;
/// 5 = `ё`/width/Turkish-`İ` folding; 6 = Arabic folding + stemming;
/// 7 = CJK bigrams.
pub const TOKENIZER_VERSION: u32 = 7;

/// Tokenizer producing identifier sub-tokens and prose stems.
#[derive(Clone)]
pub struct IdentifierTokenizer {
    ru: TextAnalyzer,
    en: TextAnalyzer,
    ar: TextAnalyzer,
}

impl Default for IdentifierTokenizer {
    fn default() -> Self {
        Self {
            ru: stemmer(Language::Russian),
            en: stemmer(Language::English),
            ar: stemmer(Language::Arabic),
        }
    }
}

fn stemmer(language: Language) -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(Stemmer::new(language))
        .build()
}

impl Tokenizer for IdentifierTokenizer {
    type TokenStream<'a> = IdentifierTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let normalized = textnorm::normalize(text);
        IdentifierTokenStream::new(&normalized, &mut self.ru, &mut self.en, &mut self.ar)
    }
}

/// Registers this tokenizer in a tantivy manager under [`NAME`].
pub fn register(manager: &tantivy::tokenizer::TokenizerManager) {
    manager.register(NAME, IdentifierTokenizer::default());
}

/// Vec-backed token stream; positions advance once per whitespace segment.
pub struct IdentifierTokenStream {
    tokens: Vec<Token>,
    index: usize,
    fallback: Token,
}

impl IdentifierTokenStream {
    fn new(
        text: &str,
        ru: &mut TextAnalyzer,
        en: &mut TextAnalyzer,
        ar: &mut TextAnalyzer,
    ) -> Self {
        Self {
            tokens: tokenize(text, ru, en, ar),
            index: 0,
            fallback: Token::default(),
        }
    }
}

impl TokenStream for IdentifierTokenStream {
    fn advance(&mut self) -> bool {
        if self.index >= self.tokens.len() {
            return false;
        }
        self.index = self.index.saturating_add(1);
        true
    }

    fn token(&self) -> &Token {
        self.index
            .checked_sub(1)
            .and_then(|position| self.tokens.get(position))
            .unwrap_or(&self.fallback)
    }

    fn token_mut(&mut self) -> &mut Token {
        if let Some(index) = self.index.checked_sub(1) {
            return &mut self.tokens[index];
        }
        &mut self.fallback
    }
}

fn tokenize(
    text: &str,
    ru: &mut TextAnalyzer,
    en: &mut TextAnalyzer,
    ar: &mut TextAnalyzer,
) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0_usize;

    for (offset, raw) in raw_segments(text) {
        if !raw.chars().any(char::is_alphanumeric) {
            continue;
        }
        let mut emitted: HashSet<String> = HashSet::new();
        let runs = alnum_runs(raw);
        let protected = raw.contains('_');
        // A single-word segment whose only run stems is emitted as the stem
        // alone (SQ7 ruling): surface variants would make the query a phrase
        // that only a document with the same surface form can satisfy.
        let stem_only =
            runs.len() == 1 && !protected && stem_language(&raw[runs[0].0..runs[0].1]).is_some();
        // A CJK-dominant segment is represented by its bigrams only (FR-12);
        // the whole-segment token would be dictionary noise.
        let cjk_segment = textnorm::script_of(raw) == Script::Cjk;
        if !stem_only && !cjk_segment {
            let lower = raw.to_lowercase();
            emit(
                &mut tokens,
                &mut emitted,
                &lower,
                offset,
                offset + raw.len(),
                position,
            );

            // Emit the identifier with surrounding punctuation stripped so that
            // `` `assessment_plan_id`, `` still carries the exact identifier token
            // (FR-21); only spans made purely of alphanumerics/underscores count.
            if let Some((from, to)) = identifier_span(raw)
                && (from > 0 || to < raw.len())
            {
                let trimmed = &raw[from..to];
                let trimmed_lower = trimmed.to_lowercase();
                emit(
                    &mut tokens,
                    &mut emitted,
                    &trimmed_lower,
                    offset + from,
                    offset + to,
                    position,
                );
            }
        }

        // Mixed segments are processed per single-script run (FR-12): CJK runs
        // become sequential bigrams, everything else keeps the word path.
        // Non-CJK variants all share the segment position (FR-7/SQ6): runs of
        // `assessment_plan_id` must not become phrase-adjacent.
        let base = position;
        for (part_from, part_to) in runs {
            let part = &raw[part_from..part_to];
            for (run_from, run) in textnorm::split_script_runs(part) {
                let start = part_from + run_from;
                if textnorm::script_of(run) == Script::Cjk {
                    emit_cjk(&mut tokens, run, offset + start, &mut position);
                    continue;
                }
                if !protected
                    && let Some(language) = stem_language(run)
                    && let Some(stemmed) =
                        stem(analyzer_for(language, ru, en, ar), &run.to_lowercase())
                {
                    emit(
                        &mut tokens,
                        &mut emitted,
                        &stemmed,
                        offset + start,
                        offset + start + run.len(),
                        base,
                    );
                    continue;
                }
                let run_lower = run.to_lowercase();
                emit(
                    &mut tokens,
                    &mut emitted,
                    &run_lower,
                    offset + start,
                    offset + start + run.len(),
                    base,
                );
                for (sub, sub_from, sub_to) in camel_split(run) {
                    emit(
                        &mut tokens,
                        &mut emitted,
                        &sub,
                        offset + start + sub_from,
                        offset + start + sub_to,
                        base,
                    );
                }
            }
        }
        // FR-7: one position per segment (CJK bigrams add their own on top).
        position = position.max(base + 1);
    }
    tokens
}

/// Emits overlapping bigrams over every character of a CJK run with
/// sequential positions (FR-12); a single-character run emits that unigram.
/// Bigrams bypass the variant dedupe: repeated bigrams must keep their
/// positions for phrase matching.
fn emit_cjk(tokens: &mut Vec<Token>, run: &str, offset: usize, position: &mut usize) {
    let chars: Vec<(usize, char)> = run.char_indices().collect();
    if chars.len() == 1 {
        let (from, ch) = chars[0];
        emit_token(
            tokens,
            &ch.to_string(),
            offset + from,
            offset + from + ch.len_utf8(),
            *position,
        );
        *position += 1;
        return;
    }
    for pair in chars.windows(2) {
        let (from, first) = pair[0];
        let (_, second) = pair[1];
        let bigram = format!("{first}{second}");
        let to = from + first.len_utf8() + second.len_utf8();
        emit_token(tokens, &bigram, offset + from, offset + to, *position);
        *position += 1;
    }
}

/// Stemmer for a standalone word: pure alphabet, longer than 3 chars, not
/// ALL-CAPS, not a camel part (FR-10 guards). `None` = keep the surface form.
fn stem_language(part: &str) -> Option<Language> {
    if part.chars().count() <= 3
        || !part.chars().all(char::is_alphabetic)
        || part.chars().all(char::is_uppercase)
        || camel_split(part).len() > 1
    {
        return None;
    }
    match textnorm::script_of(part) {
        Script::Latin => Some(Language::English),
        Script::Cyrillic => Some(Language::Russian),
        Script::Arabic => Some(Language::Arabic),
        _ => None,
    }
}

fn analyzer_for<'a>(
    language: Language,
    ru: &'a mut TextAnalyzer,
    en: &'a mut TextAnalyzer,
    ar: &'a mut TextAnalyzer,
) -> &'a mut TextAnalyzer {
    match language {
        Language::Russian => ru,
        Language::Arabic => ar,
        _ => en,
    }
}

/// Runs the word through its stemmer and returns the single output token.
fn stem(analyzer: &mut TextAnalyzer, word: &str) -> Option<String> {
    let mut stream = analyzer.token_stream(word);
    let mut result = None;
    while stream.advance() {
        result = Some(stream.token().text.clone());
    }
    result
}

fn emit(
    tokens: &mut Vec<Token>,
    emitted: &mut HashSet<String>,
    text: &str,
    from: usize,
    to: usize,
    position: usize,
) {
    if text.is_empty() || !emitted.insert(text.to_owned()) {
        return;
    }
    emit_token(tokens, text, from, to, position);
}

/// Pushes one token unconditionally (length-guarded); callers own dedupe and
/// position decisions.
fn emit_token(tokens: &mut Vec<Token>, text: &str, from: usize, to: usize, position: usize) {
    if text.is_empty() || text.chars().count() > MAX_TOKEN_CHARS {
        return;
    }
    tokens.push(Token {
        offset_from: from,
        offset_to: to,
        position,
        text: text.to_owned(),
        position_length: 1,
    });
}

fn raw_segments(text: &str) -> Vec<(usize, &str)> {
    let mut segments = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                segments.push((from, &text[from..index]));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        segments.push((from, &text[from..]));
    }
    segments
}

/// Byte span of `raw` once leading/trailing punctuation is stripped, when
/// every character inside the span is alphanumeric or `_`; `None` when the
/// segment is empty or contains punctuation in the middle (`PA.data`).
fn identifier_span(raw: &str) -> Option<(usize, usize)> {
    let is_ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut first: Option<usize> = None;
    let mut last = 0;
    for (index, ch) in raw.char_indices() {
        if is_ident(ch) {
            if first.is_none() {
                first = Some(index);
            }
            last = index + ch.len_utf8();
        }
    }
    let (from, to) = (first?, last);
    raw[from..to].chars().all(is_ident).then_some((from, to))
}

fn alnum_runs(raw: &str) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in raw.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(from) = start.take() {
            runs.push((from, index));
        }
    }
    if let Some(from) = start {
        runs.push((from, raw.len()));
    }
    runs
}

fn camel_split(part: &str) -> Vec<(String, usize, usize)> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut current_start = 0_usize;

    for (byte_index, ch) in part.char_indices() {
        let prev = part[..byte_index].chars().next_back();
        let next = part[byte_index + ch.len_utf8()..].chars().next();
        let boundary = !current.is_empty()
            && ch.is_uppercase()
            && prev.is_some_and(|prev| {
                prev.is_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary {
            words.push((current.to_lowercase(), current_start, byte_index));
            current = String::new();
        }
        if current.is_empty() {
            current_start = byte_index;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        words.push((current.to_lowercase(), current_start, part.len()));
    }
    words
}
