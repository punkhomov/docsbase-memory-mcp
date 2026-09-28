use docsbase_memory::index::tokenizer::{IdentifierTokenizer, NAME};
use tantivy::tokenizer::{TokenStream, Tokenizer, TokenizerManager};

fn tokens(text: &str) -> Vec<String> {
    let mut tokenizer = IdentifierTokenizer;
    let mut stream = tokenizer.token_stream(text);
    let mut output = Vec::new();
    while stream.advance() {
        output.push(stream.token().text.clone());
    }
    output
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
}

#[test]
fn positions_are_contiguous() {
    let mut tokenizer = IdentifierTokenizer;
    let mut stream = tokenizer.token_stream("defineStore assessment_plan_id");
    let mut positions = Vec::new();
    while stream.advance() {
        positions.push(stream.token().position);
    }
    assert_eq!(positions, (0..positions.len()).collect::<Vec<_>>());
}

#[test]
fn registration_works() {
    let mut manager = TokenizerManager::default();
    register_or_panic(&mut manager);
    let mut analyzer = manager.get(NAME).expect("registered analyzer");
    let mut stream = analyzer.token_stream("defineStore");
    let mut count = 0;
    while stream.advance() {
        count += 1;
    }
    assert!(count >= 3);
}

fn register_or_panic(manager: &mut TokenizerManager) {
    docsbase_memory::index::tokenizer::register(manager);
}
