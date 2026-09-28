use docsbase_memory::index::tokenizer::{IdentifierTokenizer, NAME};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer, TokenizerManager};

fn token_list(text: &str) -> Vec<Token> {
    let mut tokenizer = IdentifierTokenizer;
    let mut stream = tokenizer.token_stream(text);
    let mut output = Vec::new();
    while stream.advance() {
        output.push(stream.token().clone());
    }
    output
}

fn tokens(text: &str) -> Vec<String> {
    token_list(text)
        .into_iter()
        .map(|token| token.text)
        .collect()
}

fn expected(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[test]
fn camel() {
    assert_eq!(
        tokens("defineStore"),
        expected(&["definestore", "define", "store"])
    );
}

#[test]
fn snake() {
    assert_eq!(
        tokens("assessment_plan_id"),
        expected(&["assessment_plan_id", "assessment", "plan", "id"])
    );
}

#[test]
fn xml_header() {
    assert_eq!(
        tokens("X-Request-ID"),
        expected(&["x-request-id", "x", "request", "id"])
    );
}

#[test]
fn dunder() {
    assert_eq!(
        tokens("__bt_tt_getProp"),
        expected(&["__bt_tt_getprop", "bt", "tt", "getprop", "get", "prop"])
    );
}

#[test]
fn lowercased() {
    assert_eq!(tokens("NODE_ENV"), expected(&["node_env", "node", "env"]));
    assert_eq!(tokens("PA.data"), expected(&["pa.data", "pa", "data"]));
}

#[test]
fn punctuation_only_skipped() {
    assert_eq!(tokens("— ... !!!"), Vec::<String>::new());
    assert_eq!(tokens(""), Vec::<String>::new());
    assert_eq!(tokens("   "), Vec::<String>::new());
}

#[test]
fn prose_words_match_across_punctuation() {
    let indexed = tokens("Hello, world!");
    assert!(indexed.contains(&"hello".to_owned()), "{indexed:?}");
    assert!(indexed.contains(&"world".to_owned()), "{indexed:?}");

    let indexed = tokens("«привет», foo(bar)");
    assert!(indexed.contains(&"привет".to_owned()), "{indexed:?}");
    assert!(indexed.contains(&"bar".to_owned()), "{indexed:?}");
}

#[test]
fn digits_and_acronyms() {
    assert_eq!(
        tokens("get2FASecret"),
        expected(&["get2fasecret", "get2", "fa", "secret"])
    );
    assert_eq!(
        tokens("XMLHttpRequest"),
        expected(&["xmlhttprequest", "xml", "http", "request"])
    );
}

#[test]
fn unicode_camel() {
    let indexed = tokens("ЗаголовокТекста");
    assert!(indexed.contains(&"заголовок".to_owned()), "{indexed:?}");
    assert!(indexed.contains(&"текста".to_owned()), "{indexed:?}");
}

#[test]
fn dedupe_within_segment() {
    assert_eq!(tokens("x-x"), expected(&["x-x", "x"]));
    assert_eq!(tokens("aA"), expected(&["aa", "a"]));
}

#[test]
fn offsets_match_text() {
    let text = "defineStore assessment_plan_id, X-Request-ID";
    for token in token_list(text) {
        assert_eq!(
            text[token.offset_from..token.offset_to].to_lowercase(),
            token.text,
            "token offsets must cover the source span"
        );
    }
}

#[test]
fn positions_are_contiguous() {
    let text = "defineStore assessment_plan_id";
    let positions: Vec<usize> = token_list(text).into_iter().map(|t| t.position).collect();
    assert_eq!(positions, (0..positions.len()).collect::<Vec<_>>());
}

#[test]
fn registration_works() {
    let manager = TokenizerManager::default();
    docsbase_memory::index::tokenizer::register(&manager);
    let mut analyzer = manager.get(NAME).expect("registered analyzer");
    let mut stream = analyzer.token_stream("defineStore");
    let mut count = 0;
    while stream.advance() {
        count += 1;
    }
    assert!(count >= 3);
}
