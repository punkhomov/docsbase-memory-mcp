//! Owned metadata for dense work, captured in one short SQL snapshot.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{Error, Result};
use crate::store::{Db, repo};

/// Metadata identities and content hashes without a retained SQL handle.
pub struct DenseMetadata {
    cache_root: PathBuf,
    project_id: i64,
    generation: i64,
    chunks: Vec<(u64, String)>,
}

impl DenseMetadata {
    /// Captures generation and chunk hashes from one database snapshot. Ends
    /// its own transaction before returning; a caller-owned transaction remains
    /// the caller's responsibility and is never committed/rolled back here.
    ///
    /// # Errors
    /// Registry/query errors or failure to begin/end the read transaction.
    pub fn capture(db: &Db, project_id: i64) -> Result<Self> {
        Self::capture_with(db, project_id, || {})
    }

    fn capture_with(db: &Db, project_id: i64, after_generation: impl FnOnce()) -> Result<Self> {
        if !db.connection().is_autocommit() {
            return Self::read(
                db.cache_root(),
                db.connection(),
                project_id,
                after_generation,
            );
        }
        // Deferred read scope is an isolation choice, not a busy-timeout default.
        // Busy policy is already owned/applied by the supplied Db connection.
        let transaction =
            Transaction::new_unchecked(db.connection(), TransactionBehavior::Deferred)
                .map_err(|err| Error::internal_with_source("begin dense metadata snapshot", err))?;
        let metadata = Self::read(db.cache_root(), &transaction, project_id, after_generation)?;
        transaction
            .commit()
            .map_err(|err| Error::internal_with_source("end dense metadata snapshot", err))?;
        Ok(metadata)
    }

    fn read(
        root: &Path,
        connection: &Connection,
        project_id: i64,
        after_generation: impl FnOnce(),
    ) -> Result<Self> {
        let generation = repo::index_generation(connection, project_id)?;
        after_generation();
        let chunks = repo::chunk_hashes(connection, project_id)?;
        Ok(Self {
            cache_root: root.to_path_buf(),
            project_id,
            generation,
            chunks,
        })
    }

    /// Explicit database/cache identity of this snapshot.
    #[must_use]
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Registry project identity.
    #[must_use]
    pub const fn project_id(&self) -> i64 {
        self.project_id
    }

    /// Generation observed in the same SQL snapshot as the hashes.
    #[must_use]
    pub const fn generation(&self) -> i64 {
        self.generation
    }

    /// Chunk/content identities, sorted by document and sequence.
    #[must_use]
    pub fn chunks(&self) -> &[(u64, String)] {
        &self.chunks
    }

    pub(super) fn into_chunks(self) -> Vec<(u64, String)> {
        self.chunks
    }
}

#[cfg(test)]
pub(crate) use tests::Fixture as MetadataFixture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::registry;
    use crate::index::tantivy_index::chunk_id;
    use crate::store::{Db, repo, settings};
    use crate::vector_cache::chunk_hash;

    pub(crate) struct Fixture {
        pub(crate) writer: Db,
        pub(crate) cache: tempfile::TempDir,
        _root: tempfile::TempDir,
        pub(crate) project: i64,
        pub(crate) doc: i64,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let cache = tempfile::tempdir().expect("cache");
            let root = tempfile::tempdir().expect("root");
            let policy = settings::standalone().expect("policy");
            let mut writer = Db::open_with_settings(cache.path(), &policy).expect("writer");
            let project = registry::ensure_project(&mut writer, root.path())
                .expect("project")
                .id;
            let path = root.path().join("guide.md");
            let path = path.to_str().expect("fixture path");
            let doc = repo::insert_pending_doc(writer.connection(), project, "guide.md", path)
                .expect("doc");
            repo::upsert_doc(
                writer.connection(),
                project,
                doc,
                &repo::NewDoc {
                    rel_path: "guide.md",
                    abs_path: path,
                    title: None,
                    frontmatter_json: None,
                    size: 10,
                    mtime: 0,
                    content_hash: "indexed",
                    indexed_at: 0,
                },
            )
            .expect("ready doc");
            let chunks: Vec<_> = ["alpha", "beta"]
                .into_iter()
                .enumerate()
                .map(|(index, text)| repo::NewChunk {
                    seq: u32::try_from(index).expect("seq"),
                    heading_path: "Guide",
                    kind: "prose",
                    lang: None,
                    line_start: 1,
                    line_end: 1,
                    text,
                })
                .collect();
            repo::replace_chunks(writer.connection(), doc, &chunks).expect("chunks");
            repo::bump_index_generation(writer.connection(), project).expect("generation");
            Self {
                writer,
                cache,
                _root: root,
                project,
                doc,
            }
        }

        pub(crate) fn reader(&self) -> Db {
            Db::open_readonly_with_settings(
                self.cache.path(),
                &settings::standalone().expect("policy"),
            )
            .expect("reader")
        }

        fn change(&self) {
            let tx = self
                .writer
                .connection()
                .unchecked_transaction()
                .expect("writer transaction");
            tx.execute(
                "UPDATE chunks SET text='changed' WHERE doc_id=?1 AND seq=0",
                [self.doc],
            )
            .expect("update");
            repo::bump_index_generation(&tx, self.project).expect("bump");
            tx.commit().expect("commit");
        }

        pub(crate) fn original_hashes(&self) -> Vec<(u64, String)> {
            vec![
                (chunk_id(self.doc, 0), chunk_hash("alpha")),
                (chunk_id(self.doc, 1), chunk_hash("beta")),
            ]
        }
    }

    #[test]
    fn metadata_owns_root_project_generation_and_hashes() {
        fn owned<T: Send + Sync + 'static>() {}

        let fixture = Fixture::new();
        let reader = fixture.reader();
        let generation =
            repo::index_generation(reader.connection(), fixture.project).expect("generation");
        let metadata = DenseMetadata::capture(&reader, fixture.project).expect("snapshot");
        assert_eq!(metadata.cache_root(), fixture.cache.path());
        assert_eq!(metadata.project_id(), fixture.project);
        assert_eq!(metadata.generation(), generation);
        assert_eq!(metadata.chunks(), fixture.original_hashes());
        assert!(
            reader.connection().is_autocommit(),
            "SQL snapshot ended before CPU work"
        );
        owned::<DenseMetadata>();
    }

    #[test]
    fn generation_and_hashes_come_from_one_sql_snapshot() {
        let fixture = Fixture::new();
        let reader = fixture.reader();
        let generation =
            repo::index_generation(reader.connection(), fixture.project).expect("generation");
        let metadata = DenseMetadata::capture_with(&reader, fixture.project, || fixture.change())
            .expect("snapshot");
        assert_eq!(metadata.generation(), generation);
        assert_eq!(metadata.chunks(), fixture.original_hashes());
        assert!(reader.connection().is_autocommit());
        let fresh = DenseMetadata::capture(&reader, fixture.project).expect("new snapshot");
        assert_eq!(fresh.generation(), generation + 1);
        assert_eq!(
            fresh.chunks()[0],
            (chunk_id(fixture.doc, 0), chunk_hash("changed"))
        );
    }

    #[test]
    fn metadata_error_releases_its_transaction() {
        let fixture = Fixture::new();
        let reader = fixture.reader();
        assert!(DenseMetadata::capture(&reader, fixture.project + 1).is_err());
        assert!(reader.connection().is_autocommit());
        assert!(DenseMetadata::capture(&reader, fixture.project).is_ok());
    }

    #[test]
    fn caller_owned_sql_snapshot_is_preserved() {
        let fixture = Fixture::new();
        let reader = fixture.reader();
        reader
            .connection()
            .execute_batch("BEGIN")
            .expect("caller begins");
        let first = DenseMetadata::capture(&reader, fixture.project).expect("first");
        assert!(!reader.connection().is_autocommit());
        fixture.change();
        let old = DenseMetadata::capture(&reader, fixture.project).expect("same caller snapshot");
        assert_eq!(old.generation(), first.generation());
        assert_eq!(old.chunks(), first.chunks());
        assert!(!reader.connection().is_autocommit());
        reader
            .connection()
            .execute_batch("ROLLBACK")
            .expect("caller ends");
        let fresh = DenseMetadata::capture(&reader, fixture.project).expect("fresh");
        assert_eq!(fresh.generation(), first.generation() + 1);
    }
}
