use std::fs;
use std::path::Path;

use docsbase_memory::config::{Config, ConfigOverrides};
use docsbase_memory::error::Error;
use docsbase_memory::index::walk::{IGNORE_FILE, resolve_in_root, walk};
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write fixture");
}

fn collect(root: &Path, config: &Config) -> Vec<String> {
    let mut paths: Vec<String> = walk(root, config)
        .expect("walk")
        .map(|item| {
            let path = item.expect("entry");
            path.strip_prefix(root)
                .expect("relative")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn finds_md() {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), "a.md", "# a");
    write(root.path(), "sub/b.md", "# b");
    write(root.path(), "c.txt", "nope");
    write(root.path(), "sub/d.MD", "# d");

    let found = collect(root.path(), &Config::default());
    assert_eq!(found, vec!["a.md", "sub/b.md", "sub/d.MD"]);
}

#[test]
fn skips_default_ignores() {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), "keep.md", "# keep");
    write(root.path(), "node_modules/pkg/readme.md", "# pkg");
    write(root.path(), "target/doc.md", "# doc");
    write(root.path(), "vendor/x.md", "# x");

    let found = collect(root.path(), &Config::default());
    assert_eq!(found, vec!["keep.md"]);
}

#[test]
fn honors_docsbaseignore() {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), IGNORE_FILE, "skip/**\n# comment\n");
    write(root.path(), "keep.md", "# keep");
    write(root.path(), "skip/secret.md", "# secret");

    let found = collect(root.path(), &Config::default());
    assert_eq!(found, vec!["keep.md"]);
}

#[test]
fn honors_config_ignores() {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), "keep.md", "# keep");
    write(root.path(), "generated/x.md", "# x");

    let config = Config::default().with_overrides(&ConfigOverrides {
        ignores: Some(vec!["generated/**".to_owned()]),
        ..ConfigOverrides::default()
    });
    let found = collect(root.path(), &config);
    assert_eq!(found, vec!["keep.md"]);
}

#[test]
fn rejects_symlink_escape() {
    let root = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside");
    write(outside.path(), "evil.md", "# evil");
    write(root.path(), "keep.md", "# keep");

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("evil.md"), root.path().join("link.md"))
        .expect("symlink file");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.path().join("out")).expect("symlink dir");

    let found = collect(root.path(), &Config::default());
    assert_eq!(found, vec!["keep.md"]);

    let err = resolve_in_root(root.path(), Path::new("link.md")).expect_err("must reject");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
}

#[test]
fn ignore_dotdot_patterns() {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), IGNORE_FILE, "../secret.md\n");
    write(root.path(), "keep.md", "# keep");

    let err = walk(root.path(), &Config::default())
        .err()
        .expect("must reject");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
    assert!(err.to_string().contains(".."), "{err}");

    let config = Config::default().with_overrides(&ConfigOverrides {
        ignores: Some(vec!["../outside/**".to_owned()]),
        ..ConfigOverrides::default()
    });
    let err = walk(root.path(), &config)
        .err()
        .expect("must reject config pattern");
    assert!(matches!(err, Error::Project { .. }), "{err:?}");
}
