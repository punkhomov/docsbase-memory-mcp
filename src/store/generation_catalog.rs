//! Persisted generation catalog for the next registry layout.
use rusqlite::{Connection, Row, Transaction, types::ValueRef};

use super::registry_inspect::{RegistryProject, UnavailableReason as Why, sqlite_failure};

pub(super) const SCHEMA_VERSION: u32 = 5;
const FORMAT_VERSION: i64 = 1;
const ACTIVE_ROWS: &str =
    "SELECT g.project_id, g.id, h.active_id, g.layout, g.seal_digest, h.active_phase
    FROM project_generations AS g INDEXED BY one_active_generation
    LEFT JOIN project_generation_head AS h ON h.project_id = g.project_id
    WHERE g.phase = 'active'";

#[derive(Clone, Copy)]
pub(super) enum Layout {
    Legacy,
    Local { seal_digest: [u8; 32] },
}

pub(super) struct Head {
    pub(super) project_id: i64,
    pub(super) id: i64,
    pub(super) layout: Layout,
}

pub(super) fn active_head(connection: &Connection, project_id: i64) -> Result<Option<Head>, Why> {
    let mut statement = connection
        .prepare(&format!("{ACTIVE_ROWS} AND g.project_id = ?1 LIMIT 2"))
        .map_err(sqlite_failure)?;
    let mut rows = statement.query([project_id]).map_err(sqlite_failure)?;
    if let Some(row) = rows.next().map_err(sqlite_failure)? {
        let head = read_head(row)?;
        if head.project_id != project_id || rows.next().map_err(sqlite_failure)?.is_some() {
            return Err(Why::InvalidRegistry);
        }
        return Ok(Some(head));
    }
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM project_generation_head WHERE project_id = ?1)",
            [project_id],
            |row| row.get(0),
        )
        .map_err(sqlite_failure)?;
    if exists {
        return Err(Why::InvalidRegistry);
    }
    Ok(None)
}

/// Observe catalog identity on a leased, bounded registry snapshot. Local
/// artifact health and the content revision belong to the generation reader.
pub(super) fn identity(
    connection: &Connection,
    projects: &[RegistryProject],
) -> Result<blake3::Hash, Why> {
    let mut hash = blake3::Hasher::new_derive_key("docsbase.registry.generations.v1");
    hash.update(&read_epoch(connection)?);
    let limit = i64::try_from(projects.len())
        .map_err(|_| Why::LimitExceeded)?
        .checked_add(1)
        .ok_or(Why::LimitExceeded)?;
    let mut statement = connection
        .prepare(&format!("{ACTIVE_ROWS} ORDER BY g.project_id LIMIT ?1"))
        .map_err(sqlite_failure)?;
    let mut rows = statement.query([limit]).map_err(sqlite_failure)?;
    let mut previous = 0;
    let mut matched = 0;
    while let Some(row) = rows.next().map_err(sqlite_failure)? {
        let head = read_head(row)?;
        if head.project_id <= previous
            || projects
                .binary_search_by_key(&head.project_id, |record| record.id)
                .is_err()
        {
            return Err(Why::InvalidRegistry);
        }
        hash.update(&head.project_id.to_le_bytes());
        hash.update(&head.id.to_le_bytes());
        match head.layout {
            Layout::Legacy => hash.update(&[0]),
            Layout::Local { seal_digest } => {
                hash.update(&[1]);
                hash.update(&seal_digest)
            }
        };
        previous = head.project_id;
        matched += 1;
    }
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM (SELECT project_id FROM project_generation_head LIMIT ?1)",
            [limit],
            |row| row.get(0),
        )
        .map_err(sqlite_failure)?;
    if count != matched {
        return Err(Why::InvalidRegistry);
    }
    Ok(hash.finalize())
}

fn read_head(row: &Row<'_>) -> Result<Head, Why> {
    let project_id = integer(row, 0)?;
    let id = integer(row, 1)?;
    let active_id: Option<i64> = row.get(2).map_err(|_| Why::InvalidRegistry)?;
    let phase = row.get_ref(5).map_err(|_| Why::InvalidRegistry)?;
    if project_id <= 0 || id <= 0 || active_id != Some(id) || phase != ValueRef::Text(b"active") {
        return Err(Why::InvalidRegistry);
    }
    let name = row.get_ref(3).map_err(|_| Why::InvalidRegistry)?;
    let digest = row.get_ref(4).map_err(|_| Why::InvalidRegistry)?;
    let layout = match (name, digest) {
        (ValueRef::Text(b"legacy"), ValueRef::Null) => Layout::Legacy,
        (ValueRef::Text(b"local"), ValueRef::Blob(bytes)) => Layout::Local {
            seal_digest: bytes.try_into().map_err(|_| Why::InvalidRegistry)?,
        },
        _ => return Err(Why::InvalidRegistry),
    };
    Ok(Head {
        project_id,
        id,
        layout,
    })
}

pub(super) fn read_epoch(connection: &Connection) -> Result<[u8; 16], Why> {
    let mut statement = connection
        .prepare("SELECT singleton, format_version, epoch FROM generation_store LIMIT 2")
        .map_err(sqlite_failure)?;
    let mut rows = statement.query([]).map_err(sqlite_failure)?;
    let row = rows
        .next()
        .map_err(sqlite_failure)?
        .ok_or(Why::InvalidRegistry)?;
    if integer(row, 0)? != 1 {
        return Err(Why::InvalidRegistry);
    }
    if integer(row, 1)? != FORMAT_VERSION {
        return Err(Why::UnsupportedSchema);
    }
    let epoch = row
        .get_ref(2)
        .map_err(|_| Why::InvalidRegistry)?
        .as_blob()
        .map_err(|_| Why::InvalidRegistry)?
        .try_into()
        .map_err(|_| Why::InvalidRegistry)?;
    if rows.next().map_err(sqlite_failure)?.is_some() {
        return Err(Why::InvalidRegistry);
    }
    Ok(epoch)
}

fn integer(row: &Row<'_>, column: usize) -> Result<i64, Why> {
    row.get(column).map_err(|_| Why::InvalidRegistry)
}

pub(super) const SCHEMA_SQL: &str = include_str!("generation_catalog/schema.sql");

/// Called only in the transaction creating a fresh registry.
pub(super) fn initialize(tx: &Transaction<'_>) -> crate::error::Result<()> {
    let mut epoch = [0; 16];
    getrandom::fill(&mut epoch).map_err(|_| {
        crate::error::Error::internal("OS entropy unavailable for the installation epoch")
    })?;
    tx.execute_batch(SCHEMA_SQL)
        .map_err(super::repo::db_error)?;
    tx.execute(
        "INSERT INTO generation_store (singleton, format_version, epoch) VALUES (1, ?1, ?2)",
        rusqlite::params![FORMAT_VERSION, epoch],
    )
    .map_err(super::repo::db_error)?;
    Ok(())
}
