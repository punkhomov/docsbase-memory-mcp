use docsbase_memory::index::textnorm::{Script, normalize, script_of, split_script_runs};
use docsbase_memory::index::tokenizer::{IdentifierTokenizer, NAME};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer, TokenizerManager};

fn token_list(text: &str) -> Vec<Token> {
    let mut tokenizer = IdentifierTokenizer::default();
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
fn punctuation_wrapped_identifier_keeps_clean_token() {
    // Backticks/commas around an identifier must not hide its exact form:
    // the query `assessment_plan_id` has to match `assessment_plan_id`.
    let wrapped = tokens("`assessment_plan_id`,");
    assert!(
        wrapped.contains(&"assessment_plan_id".to_owned()),
        "clean identifier token missing: {wrapped:?}"
    );
    let json = tokens("\"assessment_plan_id\":");
    assert!(
        json.contains(&"assessment_plan_id".to_owned()),
        "clean identifier token missing: {json:?}"
    );
}

#[test]
fn positions_are_one_per_segment() {
    // FR-7: every variant of one whitespace segment shares a position;
    // the next segment advances the position by one.
    let list = token_list("defineStore assessment_plan_id");
    let pairs: Vec<(&str, usize)> = list
        .iter()
        .map(|token| (token.text.as_str(), token.position))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("definestore", 0),
            ("define", 0),
            ("store", 0),
            ("assessment_plan_id", 1),
            ("assessment", 1),
            ("plan", 1),
            ("id", 1),
        ]
    );
}

#[test]
fn tokens_over_max_length_are_dropped() {
    // FR-16: tokens longer than 40 chars are not emitted; 40 is kept.
    let ok = "b".repeat(40);
    assert_eq!(tokens(&ok), expected(&[ok.as_str()]));
    let too_long = "c".repeat(41);
    assert_eq!(tokens(&too_long), Vec::<String>::new());
    let blob = "a".repeat(64);
    assert_eq!(tokens(&blob), Vec::<String>::new());
}

#[test]
fn russian_forms_share_stem_classes() {
    // FR-9 via the Snowball Russian algorithm. Its RV region splits the
    // illustrative trio from the brief: `замена`/`замены` -> `зам`,
    // `заменой` -> `замен` (same classes as the official vocabulary:
    // `алена` -> `ал`, `времена` -> `врем`). Cross-form retrieval holds
    // because documents carry several forms; the golden RU class (SQ7)
    // proves the queries find the document.
    assert_eq!(tokens("замена"), tokens("замены"));
    assert_eq!(tokens("заменой"), tokens("замену"));
    assert_ne!(tokens("замена"), tokens("заменой"));
    assert_ne!(tokens("замена"), expected(&["замена"]));
    assert_ne!(tokens("заменой"), expected(&["заменой"]));
}

#[test]
fn english_forms_share_a_stem_variant() {
    let forms = ["replace", "replaces", "replaced"];
    let token_sets: Vec<Vec<String>> = forms.iter().map(|form| tokens(form)).collect();
    let common: Vec<&String> = token_sets[0]
        .iter()
        .filter(|token| {
            !forms.contains(&token.as_str())
                && token_sets[1].contains(token)
                && token_sets[2].contains(token)
        })
        .collect();
    assert_eq!(
        common.len(),
        1,
        "shared non-surface stem expected: {token_sets:?}"
    );
}

#[test]
fn identifiers_are_not_stemmed() {
    // FR-10: exact identifiers keep their variants, no invented stems.
    assert_eq!(
        tokens("MAX_FRAME_BYTES"),
        expected(&["max_frame_bytes", "max", "frame", "bytes"])
    );
    assert_eq!(
        tokens("assessment_plan_id"),
        expected(&["assessment_plan_id", "assessment", "plan", "id"])
    );
    // Camel parts are protected too: `define` must not become `defin`.
    assert_eq!(
        tokens("defineStore"),
        expected(&["definestore", "define", "store"])
    );
}

#[test]
fn stemming_guards_short_caps_and_digits() {
    assert_eq!(tokens("МОСКВА"), expected(&["москва"]));
    assert_eq!(tokens("ветка2"), expected(&["ветка2"]));
}

#[test]
fn yo_folds_to_ye() {
    // FR-14/SC-8: `ёлка` must be found by the query `елка`.
    assert_eq!(tokens("ёлка"), tokens("елка"));
    // The fold preserves case, so ALL-CAPS words keep the FR-10 guard and are
    // indexed as the (lowercased) surface only, without a stem.
    assert_eq!(tokens("ЁЛКА"), expected(&["елка"]));
}

#[test]
fn fullwidth_folds_to_ascii() {
    assert_eq!(tokens("ＡＢＣ"), expected(&["abc"]));
}

#[test]
fn turkish_dotted_i_folds() {
    assert_eq!(tokens("İstanbul"), expected(&["istanbul"]));
}

#[test]
fn normalization_is_idempotent() {
    for raw in [
        "ёлка",
        "ЁЛКА",
        "ＡＢＣ",
        "İstanbul",
        "plain ascii",
        "привет",
        "مَطَار",
        "مطـار",
        "أحمد",
        "مدرسة",
        "مصطفى",
    ] {
        let once = normalize(raw);
        let twice = normalize(&once);
        assert_eq!(once, twice, "not idempotent for {raw:?}");
    }
}

#[test]
fn script_detection() {
    assert_eq!(script_of("hello"), Script::Latin);
    assert_eq!(script_of("привет"), Script::Cyrillic);
    assert_eq!(script_of("東京"), Script::Cjk);
    assert_eq!(script_of("مطار"), Script::Arabic);
    assert_eq!(script_of("123 !?"), Script::Other);
}

#[test]
fn script_runs_split_mixed_segment() {
    // `OpenSearchを検索` — the SQ11 case: Latin run + CJK run.
    let runs = split_script_runs("OpenSearchを検索");
    let texts: Vec<&str> = runs.iter().map(|(_, text)| *text).collect();
    assert_eq!(texts, vec!["OpenSearch", "を検索"]);
    assert_eq!(runs[0].0, 0);
    assert_eq!(runs[1].0, "OpenSearch".len());
}

#[test]
fn arabic_harakat_and_tatweel_fold() {
    // FR-13/SC-8: the vocalized document must match a plain query.
    assert_eq!(tokens("مَطَار"), tokens("مطار"));
    assert_eq!(tokens("مطـار"), tokens("مطار"));
    // Hamza forms, ta marbuta and alef maksura unify (FR-13).
    assert_eq!(tokens("أحمد"), tokens("احمد"));
    assert_eq!(tokens("إبراهيم"), tokens("ابراهيم"));
    assert_eq!(tokens("آلة"), tokens("الة"));
    assert_eq!(tokens("مدرسة"), tokens("مدرسه"));
    assert_eq!(tokens("مصطفى"), tokens("مصطفي"));
}

#[test]
fn arabic_words_are_stemmed() {
    // Snowball Arabic: the definite article and feminine endings fold away.
    assert_eq!(tokens("المطار"), expected(&["مطار"]));
    assert_eq!(tokens("طائرة"), expected(&["طاير"]));
}

#[test]
fn arabic_stems_share_segment_positions() {
    let positions: Vec<usize> = token_list("مطار طائرة")
        .into_iter()
        .map(|token| token.position)
        .collect();
    assert_eq!(positions, vec![0, 1], "one stem per segment");
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
