use super::*;

#[test]
fn shared_readers_coexist_but_exclude_a_writer() {
    let cache = tempfile::tempdir().expect("cache");
    let result = with_project_read_lease(cache.path(), 1, || {
        let result = with_project_read_lease(cache.path(), 1, || Ok(42))?;
        let conflict =
            with_writer_lease(cache.path(), 1, || Ok(())).expect_err("reader excludes writer");
        assert!(is_lease_conflict(&conflict));
        Ok(result)
    })
    .expect("shared readers");
    assert_eq!(result, 42);
    with_writer_lease(cache.path(), 1, || Ok(())).expect("readers released");
}

#[test]
fn writer_excludes_a_reader_until_released() {
    let cache = tempfile::tempdir().expect("cache");
    with_writer_lease(cache.path(), 1, || {
        let error = with_project_read_lease(cache.path(), 1, || -> Result<()> {
            panic!("reader work must not run under a writer");
        })
        .expect_err("writer excludes reader");
        assert!(matches!(
            error,
            Error::Project {
                instruction: Some(_),
                ..
            }
        ));
        Ok(())
    })
    .expect("writer");
    with_project_read_lease(cache.path(), 1, || Ok(())).expect("writer released");
}

#[test]
fn read_error_releases_the_lease_and_preserves_the_error() {
    let cache = tempfile::tempdir().expect("cache");
    let result = with_project_read_lease(cache.path(), 1, || -> Result<()> {
        Err(Error::internal("controlled preparation error"))
    });
    assert_eq!(
        result.expect_err("work failed").inner_message(),
        "controlled preparation error"
    );
    with_writer_lease(cache.path(), 1, || Ok(())).expect("reader released on error");
}

#[test]
fn read_unwind_releases_the_lease() {
    let cache = tempfile::tempdir().expect("cache");
    let result = std::panic::catch_unwind(|| {
        let _ = with_project_read_lease(cache.path(), 1, || -> Result<()> {
            panic!("preparation unwind");
        });
    });
    assert!(result.is_err());
    with_writer_lease(cache.path(), 1, || Ok(())).expect("reader released on unwind");
}

#[test]
fn read_lease_does_not_block_a_different_project() {
    let cache = tempfile::tempdir().expect("cache");
    with_project_read_lease(cache.path(), 1, || {
        with_writer_lease(cache.path(), 2, || Ok(()))
    })
    .expect("independent project writer");
}

#[test]
fn read_lease_preserves_existing_lock_file_contents() {
    let cache = tempfile::tempdir().expect("cache");
    let path = lease_path(cache.path(), 1);
    fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    fs::write(&path, b"existing lock identity").expect("lock file");
    with_project_read_lease(cache.path(), 1, || Ok(())).expect("reader");
    assert_eq!(fs::read(path).expect("contents"), b"existing lock identity");
}
