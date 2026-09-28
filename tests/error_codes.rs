use docsbase_memory::error::Error;
use std::path::PathBuf;

#[test]
fn maps_categories() {
    let cases: [(Error, i32); 6] = [
        (
            Error::Admission {
                message: "build mismatch".into(),
            },
            -32010,
        ),
        (
            Error::Protocol {
                message: "bad version".into(),
            },
            -32011,
        ),
        (
            Error::Project {
                message: "not registered".into(),
                instruction: Some("run index_project".into()),
            },
            -32012,
        ),
        (
            Error::Index {
                path: Some(PathBuf::from("docs/a.md")),
                message: "parse".into(),
            },
            -32013,
        ),
        (
            Error::Query {
                message: "empty".into(),
            },
            -32014,
        ),
        (
            Error::Internal {
                message: "oops".into(),
            },
            -32603,
        ),
    ];
    for (err, code) in cases {
        assert_eq!(err.mcp_code(), code, "for {err:?}");
    }
}

#[test]
fn display_includes_context() {
    let err = Error::Index {
        path: Some(PathBuf::from("docs/a.md")),
        message: "bad frontmatter".into(),
    };
    let text = err.to_string();
    assert!(text.contains("docs/a.md"), "{text}");
    assert!(text.contains("bad frontmatter"), "{text}");
}

#[test]
fn io_error_maps_to_internal() {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    let err: Error = io.into();
    assert_eq!(err.mcp_code(), -32603);
    assert!(err.to_string().contains("missing"), "{err}");
}
