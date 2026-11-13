//! Read-only lease observations shared by the lease integration tests.

use std::path::Path;

use docsbase_memory::error::{Error, Result};
use docsbase_memory::vector_cache::VECTOR_DB_FILE;
use rusqlite::{Connection, OpenFlags, params};

/// Count all stored leases for a model, including expired rows, without opening
/// a cache owner or creating a missing database.
pub fn lease_count(cache_root: &Path, model_id: &str) -> Result<u64> {
    let path = cache_root.join(VECTOR_DB_FILE);
    let conn =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|err| {
            Error::internal_with_source(
                format!("open vector lease database {}", path.display()),
                err,
            )
        })?;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vector_cache_leases WHERE model_id = ?1",
            params![model_id],
            |row| row.get(0),
        )
        .map_err(|err| {
            Error::internal_with_source(
                format!("count vector leases for {model_id:?} in {}", path.display()),
                err,
            )
        })?;
    u64::try_from(count).map_err(|err| {
        Error::internal_with_source(
            format!(
                "convert vector lease count for {model_id:?} in {}",
                path.display()
            ),
            err,
        )
    })
}
