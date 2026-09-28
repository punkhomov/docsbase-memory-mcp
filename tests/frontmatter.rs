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
