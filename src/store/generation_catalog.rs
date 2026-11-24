//! Persisted generation catalog for the next registry layout.
use rusqlite::{Connection, Row, types::ValueRef};

use super::registry_inspect::{RegistryProject, UnavailableReason as Why, sqlite_failure};

pub(super) const SCHEMA_VERSION: u32 = 4;
const FORMAT_VERSION: i64 = 1;

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
        .prepare(
            "SELECT g.project_id, g.id, h.active_id, g.layout, g.seal_digest, h.active_phase
             FROM project_generations AS g INDEXED BY one_active_generation
             LEFT JOIN project_generation_head AS h ON h.project_id = g.project_id
             WHERE g.phase = 'active'
             ORDER BY g.project_id LIMIT ?1",
        )
        .map_err(sqlite_failure)?;
    let mut rows = statement.query([limit]).map_err(sqlite_failure)?;
    let mut previous = 0;
    let mut matched = 0;
    while let Some(row) = rows.next().map_err(sqlite_failure)? {
        let project: i64 = integer(row, 0)?;
        let generation: i64 = integer(row, 1)?;
        let head: Option<i64> = row.get(2).map_err(|_| Why::InvalidRegistry)?;
        let phase = row.get_ref(5).map_err(|_| Why::InvalidRegistry)?;
        if project <= previous
            || projects
                .binary_search_by_key(&project, |record| record.id)
                .is_err()
            || generation <= 0
            || head != Some(generation)
            || phase != ValueRef::Text(b"active")
        {
            return Err(Why::InvalidRegistry);
        }
        hash.update(&project.to_le_bytes());
        hash.update(&generation.to_le_bytes());
        hash_layout(&mut hash, row)?;
        previous = project;
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

fn read_epoch(connection: &Connection) -> Result<[u8; 16], Why> {
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

fn hash_layout(hash: &mut blake3::Hasher, row: &Row<'_>) -> Result<(), Why> {
    let layout = row.get_ref(3).map_err(|_| Why::InvalidRegistry)?;
    let digest = row.get_ref(4).map_err(|_| Why::InvalidRegistry)?;
    match (layout, digest) {
        (ValueRef::Text(b"legacy"), ValueRef::Null) => hash.update(&[0]),
        (ValueRef::Text(b"local"), ValueRef::Blob(bytes)) if bytes.len() == 32 => {
            hash.update(&[1]);
            hash.update(bytes)
        }
        _ => return Err(Why::InvalidRegistry),
    };
    Ok(())
}

#[cfg(test)]
pub(super) const SCHEMA_SQL: &str = include_str!("generation_catalog/schema.sql");
