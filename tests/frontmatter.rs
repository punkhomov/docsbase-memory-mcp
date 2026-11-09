use docsbase_memory::index::frontmatter::{Frontmatter, parse};

#[test]
fn absent() {
    let text = "# Just a doc\n\nbody\n";
    let (meta, body) = parse(text);
    assert_eq!(meta, Frontmatter::default());
    assert_eq!(body, text);
}

#[test]
fn flat_keys() {
    let text = "---\ntitle: My Doc\ntags: alpha, beta\nother: ignored\n---\n# Body\n";
    let (meta, body) = parse(text);
    assert_eq!(meta.title.as_deref(), Some("My Doc"));
    assert_eq!(meta.tags, vec!["alpha".to_owned(), "beta".to_owned()]);
    assert_eq!(body, "# Body\n");
}

#[test]
fn quoted_values_and_empty_tags() {
    let text = "---\ntitle: \"Quoted\"\ntags:\n---\nbody";
    let (meta, body) = parse(text);
    assert_eq!(
        meta,
        Frontmatter {
            title: Some("Quoted".to_owned()),
            tags: Vec::new(),
        }
    );
    assert_eq!(body, "body");
}

#[test]
fn malformed_is_ignored() {
    let text = "---\nnot a pair\ntags: a\ntitle: T\n---\nbody";
    let (meta, body) = parse(text);
    assert_eq!(meta.title.as_deref(), Some("T"));
    assert_eq!(meta.tags, vec!["a".to_owned()]);
    assert_eq!(body, "body");

    let unterminated = "---\ntitle: T\nbody without closing";
    let (meta, body) = parse(unterminated);
    assert_eq!(meta.title, None);
    assert_eq!(body, unterminated);
}

#[test]
fn crlf() {
    let text = "---\r\ntitle: T\r\ntags: a\r\n---\r\nbody\r\n";
    let (meta, body) = parse(text);
    assert_eq!(meta.title.as_deref(), Some("T"));
    assert_eq!(meta.tags, vec!["a".to_owned()]);
    assert_eq!(body, "body\r\n");
}

#[test]
fn unpaired_quote_is_preserved() {
    let text = "---\ntitle: 'Twas the night\n---\n\nbody\n";
    let (meta, body) = parse(text);
    assert_eq!(meta.title.as_deref(), Some("'Twas the night"));
    assert_eq!(body, "\nbody\n");
}

#[test]
fn closing_fence_accepts_trailing_whitespace() {
    let text = "---\ntitle: Guide\n--- \nbody\n";
    let (meta, body) = parse(text);
    assert_eq!(meta.title.as_deref(), Some("Guide"));
    assert_eq!(body, "body\n");
}

#[test]
fn block_tags_match_inline_tags() {
    let expected = parse("---\ntitle: Guide\ntags: api, docs\n---\n# Body\n");
    for list in [
        "  - api\n  - docs",
        "    - api\n    - docs",
        "- api\n- docs",
    ] {
        for newline in ["\n", "\r\n"] {
            let text =
                format!("---\ntitle: Guide\ntags:\n{list}\n---\n# Body\n").replace('\n', newline);
            let (meta, body) = parse(&text);
            assert_eq!(meta, expected.0, "{text:?}");
            assert_eq!(body, format!("# Body{newline}"));
        }
    }
}

#[test]
fn block_tags_keep_scalar_values_and_skip_comments() {
    let text = "---\ntags: # categories\n  - api # public API\n\n  # keep reading\n  - 'docs, guides'\n  - \"kind: reference\"\n  - c#\n  - '#literal' # comment\n  - ''\n  -\ntitle: Guide\nother:\n  - unrelated\n---\nbody";
    let (meta, body) = parse(text);
    assert_eq!(
        meta.tags,
        ["api", "docs, guides", "kind: reference", "c#", "#literal"]
    );
    assert_eq!(meta.title.as_deref(), Some("Guide"));
    assert_eq!(body, "body");
}

#[test]
fn block_tags_do_not_collect_other_lists_or_nested_items() {
    for block in [
        "other:\n  - unrelated\ntags:\n  - api\ntitle: Guide\n  - unrelated",
        "tags:\n  - api\nother:\n  - unrelated",
        "tags:\n  - api\n    - nested",
        "tags:\n  - api\n    title: nested",
        "tags:\n  - api\nother:\n  tags:\n    - unrelated",
    ] {
        let text = format!("---\n{block}\n---\nbody");
        assert_eq!(parse(&text).0.tags, ["api"], "{block}");
    }
    assert!(
        parse("---\ntags:\nother:\n  - unrelated\n---\nbody")
            .0
            .tags
            .is_empty()
    );
    assert!(
        parse("---\nother:\n  tags:\n    - unrelated\n---\nbody")
            .0
            .tags
            .is_empty()
    );
}

#[test]
fn uniformly_indented_flat_keys_are_supported() {
    let (meta, body) = parse("---\n  title: Guide\n  tags:\n    - api\n    - docs\n---\nbody");
    assert_eq!(meta.title.as_deref(), Some("Guide"));
    assert_eq!(meta.tags, ["api", "docs"]);
    assert_eq!(body, "body");
}

#[test]
fn block_tags_skip_compact_nested_sequences_but_keep_quoted_dashes() {
    let (meta, _) = parse(
        "---\ntags:\n  - - nested\n  - -\n  - \"- quoted\"\n  - '- other'\n  - -plain\n---\nbody",
    );
    assert_eq!(meta.tags, ["- quoted", "- other", "-plain"]);
}
