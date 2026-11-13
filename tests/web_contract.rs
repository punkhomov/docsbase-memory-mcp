#![cfg(feature = "web")]
use docsbase_memory::web::contracts::typescript;
#[test]
fn generated_types_are_current() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/shared/api/generated.ts");
    let expected = typescript();
    if std::env::var_os("DOCSBASE_GENERATE_TYPES").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        expected,
        "run DOCSBASE_GENERATE_TYPES=1 cargo test --features web --test web_contract"
    );
}
