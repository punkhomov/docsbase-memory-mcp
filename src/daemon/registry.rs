//! Project registry: ensure/list/status/resolve (FR-10, FR-13; I6, I7; C8).

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::store::Db;
use crate::store::migrations;
use crate::store::models::{Project, ProjectStatus};
use crate::store::repo::db_error;

/// Registers `path` and returns the project, reusing an existing row when the
/// canonical root (git root when present) was registered before (FR-13).
///
/// # Errors
/// Returns [`Error::Project`] when the path is missing, not a directory, or
/// resolves to `/`, `$HOME` or the cache root (I7).
pub fn ensure_project(db: &mut Db, path: &Path) -> Result<Project> {
    let home = crate::platform::paths::home_dir();
    let canonical_root = normalize_root(path, home.as_deref(), db.cache_root())?;
    if let Some(project) = find_by_root(db.connection(), &canonical_root)? {
        return Ok(project);
    }

    let name = canonical_root.file_name().map_or_else(
        || canonical_root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(|err| Error::internal_with_source(format!("begin sqlite: {err}"), err))?;
    tx.execute(
        "INSERT INTO projects (canonical_root, name, status, schema_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(canonical_root) DO NOTHING",
        params![
            canonical_root.to_string_lossy(),
            name,
            ProjectStatus::NotIndexed.as_str(),
            migrations::SCHEMA_VERSION,
            unix_now()
        ],
    )
    .map_err(db_error)?;
    let project = find_by_root(&tx, &canonical_root)?.ok_or_else(|| {
        Error::internal(format!(
            "project row missing after insert: {}",
            canonical_root.display()
        ))
    })?;
    tx.commit()
        .map_err(|err| Error::internal_with_source(format!("commit sqlite: {err}"), err))?;
    Ok(project)
}

/// Every registered project, ordered by name.
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures or a corrupt status value.
pub fn list_projects(db: &Db) -> Result<Vec<Project>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT id, canonical_root, name, status, schema_version, created_at, last_indexed_at
             FROM projects ORDER BY name, id",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(db_error)?;
    let mut projects = Vec::new();
    for row in rows {
        projects.push(to_project(row.map_err(db_error)?)?);
    }
    Ok(projects)
}

/// Records a successful full index: `Indexed`, `last_indexed_at` and the
/// schema version the index was built with (FR-26, T24).
///
/// # Errors
/// Returns [`Error::Project`] for an unknown id and [`Error::Internal`] on
/// `SQLite` failures.
pub fn mark_indexed(db: &Db, id: i64) -> Result<()> {
    let conn = db.connection();
    let updated = conn
        .execute(
            "UPDATE projects SET status = ?1, last_indexed_at = ?2, schema_version = ?3 WHERE id = ?4",
            params![
                ProjectStatus::Indexed.as_str(),
                unix_now(),
                migrations::SCHEMA_VERSION,
                id
            ],
        )
        .map_err(db_error)?;
    if updated == 0 {
        return Err(Error::Project {
            message: format!("unknown project id {id}"),
            instruction: None,
        });
    }
    Ok(())
}

/// Updates the lifecycle status of one project (FR-26). Incremental runs must
/// not touch `schema_version`, so a schema bump keeps refusing reads until a
/// full rebuild (T24).
///
/// # Errors
/// Returns [`Error::Project`] for an unknown id and [`Error::Internal`] on
/// `SQLite` failures.
pub fn set_status(db: &Db, id: i64, status: ProjectStatus) -> Result<()> {
    let conn = db.connection();
    let updated = if status == ProjectStatus::Indexed {
        conn.execute(
            "UPDATE projects SET status = ?1, last_indexed_at = ?2 WHERE id = ?3",
            params![status.as_str(), unix_now(), id],
        )
    } else {
        conn.execute(
            "UPDATE projects SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
        )
    }
    .map_err(db_error)?;
    if updated == 0 {
        return Err(Error::Project {
            message: format!("unknown project id {id}"),
            instruction: None,
        });
    }
    Ok(())
}

/// Finds the registered project whose root is the closest ancestor of `cwd`
/// (I6); sessions bind to it instead of indexing silently (FR-10, C8).
///
/// # Errors
/// Returns [`Error::Project`] when `cwd` cannot be resolved or no project
/// contains it.
pub fn resolve_by_cwd(db: &Db, cwd: &Path) -> Result<Project> {
    let canonical = cwd.canonicalize().map_err(|err| Error::Project {
        message: format!("cannot resolve working directory {}: {err}", cwd.display()),
        instruction: None,
    })?;
    list_projects(db)?
        .into_iter()
        .filter(|project| crate::platform::paths::is_under(&canonical, &project.canonical_root))
        .max_by_key(|project| project.canonical_root.components().count())
        .ok_or_else(|| Error::Project {
            message: format!("no project registered for {}", canonical.display()),
            instruction: Some("call index_project or `docsbase index` first".to_owned()),
        })
}

/// Loads one project by id.
///
/// # Errors
/// Returns [`Error::Internal`] on `SQLite` failures or a corrupt status value.
pub fn project_by_id(db: &Db, id: i64) -> Result<Option<Project>> {
    let row = db
        .connection()
        .query_row(
            "SELECT id, canonical_root, name, status, schema_version, created_at, last_indexed_at
             FROM projects WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?;
    row.map(to_project).transpose()
}

fn find_by_root(conn: &Connection, root: &Path) -> Result<Option<Project>> {
    let root = root.to_string_lossy();
    let row = conn
        .query_row(
            "SELECT id, canonical_root, name, status, schema_version, created_at, last_indexed_at
             FROM projects WHERE canonical_root = ?1",
            [root.as_ref()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?;
    row.map(to_project).transpose()
}

type Row = (i64, String, String, String, u32, i64, Option<i64>);

fn to_project(row: Row) -> Result<Project> {
    let (id, root, name, status, schema_version, created_at, last_indexed_at) = row;
    let status = ProjectStatus::from_str(&status)
        .map_err(|err| Error::internal_with_source(format!("project {id}: {err}"), err))?;
    Ok(Project {
        id,
        canonical_root: PathBuf::from(root),
        name,
        status,
        schema_version,
        created_at,
        last_indexed_at,
    })
}

/// Canonical project root (realpath + git root) with I7 validation, without
/// touching the registry; used for config lookup before registration.
///
/// # Errors
/// Same as [`ensure_project`].
pub fn project_root_for(path: &Path, cache: &Path) -> Result<PathBuf> {
    normalize_root(path, crate::platform::paths::home_dir().as_deref(), cache)
}

fn normalize_root(path: &Path, home: Option<&Path>, cache: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(Error::Project {
            message: format!("{} does not exist", path.display()),
            instruction: None,
        });
    }
    let canonical = path.canonicalize().map_err(|err| Error::Project {
        message: format!("cannot resolve {}: {err}", path.display()),
        instruction: None,
    })?;
    if !canonical.is_dir() {
        return Err(Error::Project {
            message: format!("{} is not a directory", canonical.display()),
            instruction: None,
        });
    }
    let root = git_root(&canonical);
    let cache = cache.canonicalize().unwrap_or_else(|_| cache.to_path_buf());
    // Canonicalize and fold so a symlinked/case-variant HOME is still refused
    // (I7; ADR-10 for Windows verbatim paths).
    let home = home.map(|home| home.canonicalize().unwrap_or_else(|_| home.to_path_buf()));
    let home_is_root = home.as_deref().is_some_and(|home| {
        crate::platform::paths::normalize_for_compare(&root)
            == crate::platform::paths::normalize_for_compare(home)
    });
    let rejected = crate::platform::paths::is_filesystem_root(&root)
        || home_is_root
        || crate::platform::paths::is_system_dir(&root)
        || crate::platform::paths::is_under(&root, &cache)
        || crate::platform::paths::is_under(&canonical, &cache);
    if rejected {
        return Err(Error::Project {
            message: format!(
                "{} is not a valid project root (/, $HOME, system directories and the cache root are refused)",
                root.display()
            ),
            instruction: Some("pick a project directory below your home".to_owned()),
        });
    }
    Ok(root)
}

fn git_root(canonical: &Path) -> PathBuf {
    let mut current = Some(canonical);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    canonical.to_path_buf()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_root_refuses_home_and_cache() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        let project = tmp.path().join("home/code/app");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::create_dir_all(&cache).expect("mkdir cache");

        assert!(normalize_root(&home, Some(&home), &cache).is_err());
        assert!(normalize_root(&cache, Some(&home), &cache).is_err());
        let inside_cache = cache.join("nested");
        std::fs::create_dir_all(&inside_cache).expect("mkdir nested");
        assert!(normalize_root(&inside_cache, Some(&home), &cache).is_err());
        assert!(normalize_root(&project, Some(&home), &cache).is_ok());
    }
}
