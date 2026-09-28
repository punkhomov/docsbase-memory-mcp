//! Identifier-aware tokenizer (FR-21).
//!
//! Emits the lowercased raw token plus `camelCase` / `snake_case` sub-tokens so
//! technical identifiers (`defineStore`, `assessment_plan_id`,
//! `__bt_tt_getProp`, `X-Request-ID`) rank high on exact matches while still
//! matching their parts.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Tokenizer name registered in the [`TokenizerManager`].
pub const NAME: &str = "identifier";

const SEPARATORS: &[char] = &['_', '-', '.', ':', '/', '\\'];

/// Tokenizer producing identifier sub-tokens.
#[derive(Clone, Default)]
pub struct IdentifierTokenizer;

impl Tokenizer for IdentifierTokenizer {
    type TokenStream<'a> = IdentifierTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        IdentifierTokenStream {
            tokens: tokenize(text),
            index: 0,
        }
    }
}

/// Registers this tokenizer in a tantivy manager under [`NAME`].
pub fn register(manager: &mut tantivy::tokenizer::TokenizerManager) {
    manager.register(NAME, IdentifierTokenizer);
}

/// Vec-backed token stream; positions are contiguous.
pub struct IdentifierTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl TokenStream for IdentifierTokenStream {
    fn advance(&mut self) -> bool {
        if self.index >= self.tokens.len() {
            return false;
        }
        self.index += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        let index = self.index - 1;
        &mut self.tokens[index]
    }
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0_usize;

    for (offset, raw) in raw_segments(text) {
        if !raw.chars().any(char::is_alphanumeric) {
            continue;
        }
        let lower = raw.to_lowercase();
        push(
            &mut tokens,
            &lower,
            offset,
            offset + raw.len(),
            &mut position,
        );

        for part in raw.split(SEPARATORS).filter(|part| !part.is_empty()) {
            let part_lower = part.to_lowercase();
            if part_lower != lower {
                push(
                    &mut tokens,
                    &part_lower,
                    offset,
                    offset + raw.len(),
                    &mut position,
                );
            }
            for sub in camel_split(part) {
                if sub != part_lower && sub != lower {
                    push(&mut tokens, &sub, offset, offset + raw.len(), &mut position);
                }
            }
        }
    }
    tokens
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

fn camel_split(part: &str) -> Vec<String> {
    let chars: Vec<char> = part.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();

    for (index, ch) in chars.iter().enumerate() {
        let prev = index
            .checked_sub(1)
            .and_then(|position| chars.get(position));
        let next = chars.get(index + 1);
        let boundary = !current.is_empty()
            && ch.is_uppercase()
            && prev.is_some_and(|prev| {
                prev.is_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_uppercase() && next.is_some_and(|next| next.is_lowercase()))
            });
        if boundary {
            words.push(std::mem::take(&mut current).to_lowercase());
        }
        current.push(*ch);
    }
    if !current.is_empty() {
        words.push(current.to_lowercase());
    }
    words
}

fn push(tokens: &mut Vec<Token>, text: &str, from: usize, to: usize, position: &mut usize) {
    tokens.push(Token {
        offset_from: from,
        offset_to: to,
        position: *position,
        text: text.to_owned(),
        position_length: 1,
    });
    *position += 1;
}
