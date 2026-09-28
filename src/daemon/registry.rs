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
    let home = home_dir();
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
/// Returns [`Error::Internal`] on SQLite failures or a corrupt status value.
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

/// Updates the lifecycle status of one project.
///
/// # Errors
/// Returns [`Error::Project`] for an unknown id and [`Error::Internal`] on
/// SQLite failures.
pub fn set_status(db: &Db, id: i64, status: ProjectStatus) -> Result<()> {
    let updated = db
        .connection()
        .execute(
            "UPDATE projects SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
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
        .filter(|project| canonical.starts_with(&project.canonical_root))
        .max_by_key(|project| project.canonical_root.components().count())
        .ok_or_else(|| Error::Project {
            message: format!("no project registered for {}", canonical.display()),
            instruction: Some("call index_project or `docsbase index` first".to_owned()),
        })
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
    let rejected = root == Path::new("/")
        || home.is_some_and(|home| root == home)
        || root == cache
        || canonical.starts_with(&cache);
    if rejected {
        return Err(Error::Project {
            message: format!(
                "{} is not a valid project root (/, $HOME and the cache root are refused)",
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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
