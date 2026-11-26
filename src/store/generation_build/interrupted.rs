//! Evidence for replacing a selected pair after an incomplete local update.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::config::schema::Resolved;
use crate::daemon::{lifecycle, registry};
use crate::error::Result;
use crate::platform::fs as native_fs;
use crate::store::generation_catalog::{self, Layout};
use crate::store::generation_seal::GenerationSeal;
use crate::store::project_index::project_directory;
use crate::store::registry_inspect::{
    self, InspectionLimits, LocalRevision, ObservedProjectIndex, ProjectIndexLayout,
};
use crate::store::settings::DbSettings;
use crate::store::{Db, repo};

use super::publication::{configure_publication_connection, open_local_metadata};
use super::{admission, entry_exists, filesystem, open_coordination_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InterruptedBase {
    pub(crate) index: ObservedProjectIndex,
    seal: [u8; 32],
    revision: LocalRevision,
}

pub(super) fn inspect_for_replacement(
    registry: &Db,
    project_id: i64,
    limits: InspectionLimits,
) -> Result<Option<InterruptedBase>> {
    let cache = registry.cache_root();
    let lock = lifecycle::lease_path(cache, project_id);
    lifecycle::with_writer_lock(open_coordination_file(&lock, false)?, &lock, || {
        inspect(
            registry.connection(),
            cache,
            project_id,
            registry.settings(),
            limits,
            true,
        )
    })
}

pub(super) fn recheck(
    connection: &Connection,
    cache: &Path,
    expected: InterruptedBase,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<()> {
    if inspect(
        connection,
        cache,
        expected.index.project_id,
        settings,
        limits,
        false,
    )? != Some(expected)
    {
        return Err(admission(
            "interrupted generation changed before replacement",
        ));
    }
    Ok(())
}

fn inspect(
    connection: &Connection,
    cache: &Path,
    project_id: i64,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
    recover_journal: bool,
) -> Result<Option<InterruptedBase>> {
    let epoch = generation_catalog::read_epoch(connection)
        .map_err(|reason| admission(format!("generation epoch unavailable ({reason:?})")))?;
    let Some(head) = generation_catalog::active_head(connection, project_id)
        .map_err(|reason| admission(format!("generation head unavailable ({reason:?})")))?
    else {
        return Ok(None);
    };
    let Layout::Local { seal_digest } = head.layout else {
        return Ok(None);
    };
    let project = registry::project_by_id_in(connection, project_id)?
        .ok_or_else(|| admission("interrupted generation is no longer registered"))?;
    let base = project_directory(cache, project_id);
    let directory = base.join("generations").join(head.id.to_string());
    registry_inspect::check_directory(&directory)
        .map_err(|reason| admission(format!("generation directory unavailable ({reason:?})")))?;
    let lock = directory.join(".read.lock");
    lifecycle::with_read_lock(open_coordination_file(&lock, false)?, &lock, || {
        let bytes = registry_inspect::read_checked_seal(&directory.join("seal.json"), &seal_digest)
            .map_err(|reason| {
                admission(format!(
                    "interrupted generation seal unavailable ({reason:?})"
                ))
            })?;
        let seal: GenerationSeal = serde_json::from_slice(&bytes).map_err(|error| {
            crate::error::Error::internal_with_source("decode generation seal", error)
        })?;
        let journal = directory.join("metadata.db-journal");
        let recovering = recover_journal && entry_exists(&journal)?;
        let mut metadata = if recovering {
            open_journal_for_recovery(&directory, settings, limits)?
        } else {
            open_local_metadata(&directory, settings, limits)?
        };
        let revision =
            registry_inspect::read_local_revision(metadata.connection(), project_id, head.id)
                .map_err(|reason| {
                    admission(format!(
                        "local generation revision unavailable ({reason:?})"
                    ))
                })?;
        if !revision.matches_seal(&seal, epoch, project_id, head.id) {
            return Err(admission(
                "interrupted generation revision does not match its seal",
            ));
        }
        let local = registry::project_by_id(&metadata, project_id)?
            .filter(|local| local.canonical_root == project.canonical_root)
            .ok_or_else(|| admission("interrupted generation project root changed"))?;
        let project_id = local.id;
        if recovering && entry_exists(&journal)? {
            // A read rolls back a hot journal. A cold journal may remain.
            // Let SQLite retire it by committing the validated schema stamp.
            let tx = metadata.begin_write()?;
            tx.pragma_update(None, "user_version", revision.schema)
                .map_err(repo::db_error)?;
            tx.commit().map_err(repo::db_error)?;
        }
        metadata.close()?;
        registry_inspect::sidecars_absent(&directory.join("metadata.db")).map_err(|reason| {
            admission(format!("local generation journal unavailable ({reason:?})"))
        })?;
        Ok(revision.pending.then_some(InterruptedBase {
            index: ObservedProjectIndex {
                project_id,
                epoch: Some(epoch),
                generation_id: Some(head.id),
                content_revision: revision.content_revision,
                layout: ProjectIndexLayout::Local,
            },
            seal: seal_digest,
            revision,
        }))
    })
}

fn open_journal_for_recovery(
    directory: &Path,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Db> {
    if entry_exists(&directory.join("metadata.db-shm"))?
        || entry_exists(&directory.join("metadata.db-wal"))?
    {
        return Err(admission("local generation has unexpected WAL sidecars"));
    }
    let path = directory.join("metadata.db");
    for file in [&path, &directory.join("metadata.db-journal")] {
        filesystem(
            file,
            "inspect local recovery file",
            native_fs::check_single_link(file),
        )?;
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(repo::db_error)?;
    configure_publication_connection(&conn, limits)?;
    conn.pragma_update(None, "synchronous", "EXTRA")
        .map_err(repo::db_error)?;
    let journal: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(repo::db_error)?;
    if journal != "delete" {
        return Err(admission(
            "local generation recovery requires the DELETE journal mode",
        ));
    }
    Ok(Db {
        conn,
        cache_root: directory.to_owned(),
        settings: settings.clone(),
        generation_catalog: false,
    })
}
