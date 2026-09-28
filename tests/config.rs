use std::fs;
use std::path::Path;

use docsbase_memory::config::{Config, ConfigOverrides};
use tempfile::TempDir;

fn write(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("write fixture");
}

#[test]
fn defaults_when_missing() {
    let config_dir = TempDir::new().expect("tempdir");
    let config = Config::load_from(config_dir.path(), None).expect("load");

    assert_eq!(config, Config::default());
    assert_eq!(config.max_file_size, 1_048_576);
    assert_eq!(config.max_docs_per_project, 20_000);
    assert!(!config.hybrid);
}

#[test]
fn auto_index_default_false() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "hybrid = true\n");
    write(project.path(), ".docsbase.toml", "max_file_size = 42\n");

    let config = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    assert!(!config.auto_index, "auto_index must stay opt-in (ADR-6)");
    assert!(config.hybrid, "global flag inherited");
    assert_eq!(config.max_file_size, 42);
}

#[test]
fn project_overrides_global() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(
        config_dir.path(),
        "config.toml",
        "max_file_size = 100\nauto_index = true\nignores = [\"a/**\"]\n",
    );
    write(
        project.path(),
        ".docsbase.toml",
        "max_file_size = 200\nignores = [\"b/**\"]\n",
    );

    let config = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    assert_eq!(config.max_file_size, 200, "project wins over global");
    assert!(
        config.auto_index,
        "global value inherited when project is silent"
    );
    assert_eq!(config.ignores, vec!["b/**".to_owned()]);
}

#[test]
fn cli_overrides_project() {
    let config_dir = TempDir::new().expect("tempdir");
    let project = TempDir::new().expect("tempdir");
    write(project.path(), ".docsbase.toml", "max_file_size = 200\n");

    let base = Config::load_from(config_dir.path(), Some(project.path())).expect("load");
    let config = base.with_overrides(&ConfigOverrides {
        max_file_size: Some(300),
        auto_index: Some(true),
        ..ConfigOverrides::default()
    });

    assert_eq!(config.max_file_size, 300, "CLI wins over project");
    assert!(config.auto_index, "CLI can opt in explicitly");
}

#[test]
fn unknown_key_rejected() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "unknown_key = 1\n");

    let err = Config::load_from(config_dir.path(), None).expect_err("must reject");
    let text = err.to_string();
    assert!(text.contains("unknown_key"), "{text}");
}

#[test]
fn zero_limits_rejected() {
    let config_dir = TempDir::new().expect("tempdir");
    write(config_dir.path(), "config.toml", "max_file_size = 0\n");

    let err = Config::load_from(config_dir.path(), None).expect_err("must reject");
    assert!(
        matches!(err, docsbase_memory::error::Error::Admission { .. }),
        "{err:?}"
    );

    let overridden = Config::default().with_overrides(&ConfigOverrides {
        max_docs_per_project: Some(0),
        ..ConfigOverrides::default()
    });
    assert!(
        overridden.validate().is_err(),
        "overrides must be revalidated"
    );
}
