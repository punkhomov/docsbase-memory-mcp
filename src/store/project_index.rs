//! Validated project index binding, captured under the project read lease.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(super) const LOCAL_SCHEMA_VERSION: u32 = 1;
pub(super) const LOCAL_SCHEMA_SQL: &str = include_str!("registry_inspect/project_schema.sql");

/// Identifies one revision of a project's existing SQLite/Tantivy pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectIndex {
    project_id: i64,
    content_revision: i64,
}

impl ProjectIndex {
    pub(crate) fn legacy(project_id: i64, content_revision: i64) -> Result<Self> {
        if project_id <= 0 || content_revision < 0 {
            return Err(Error::Project {
                message: format!("project {project_id} has an invalid index generation"),
                instruction: Some("check the project index before retrying".to_owned()),
            });
        }
        Ok(Self {
            project_id,
            content_revision,
        })
    }

    pub(crate) const fn content_revision(self) -> i64 {
        self.content_revision
    }

    /// Resolve within the same cache as the metadata reader that supplied this binding.
    pub(crate) fn tantivy_dir(self, cache: &Path) -> PathBuf {
        legacy_tantivy_dir(cache, self.project_id)
    }
}

pub(crate) fn legacy_tantivy_dir(cache: &Path, project_id: i64) -> PathBuf {
    project_directory(cache, project_id).join("tantivy")
}

pub(super) fn project_directory(cache: &Path, project_id: i64) -> PathBuf {
    cache.join("projects").join(project_id.to_string())
}
