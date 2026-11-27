//! Registry access for normal index jobs and explicitly stopped maintenance.

use std::path::Path;

use rusqlite::Connection;

use crate::config::schema::Resolved;
use crate::daemon::lifecycle;
use crate::error::Result;
use crate::store::project_index::observe_catalog_index;
use crate::store::registry_inspect::{InspectionLimits, ObservedProjectIndex};
use crate::store::settings::DbSettings;
use crate::store::{Db, registry_inspect, repo};

use super::replacement::{self, ReplacementBase};
use super::{admission, open_registry, recovery::open_recovery_registry, validate_epoch};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogAccess {
    Stopped,
    Operational,
    Repair(ReplacementBase),
}

impl CatalogAccess {
    pub(super) fn read<T>(
        self,
        cache: &Path,
        settings: &Resolved<DbSettings>,
        limits: InspectionLimits,
        read: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        match self {
            Self::Stopped => {
                registry_inspect::with_registry(cache, limits, || {}, |conn, _| Ok(read(conn)))
                    .map_err(|reason| {
                        admission(format!("generation registry is unavailable ({reason:?})"))
                    })?
            }
            Self::Operational | Self::Repair(_) => {
                let db = open_operational_reader(cache, settings, limits)?;
                let tx = db
                    .connection()
                    .unchecked_transaction()
                    .map_err(repo::db_error)?;
                let value = read(&tx)?;
                tx.commit().map_err(repo::db_error)?;
                db.close()?;
                Ok(value)
            }
        }
    }

    pub(super) fn writer(self, cache: &Path, settings: &Resolved<DbSettings>) -> Result<Db> {
        match self {
            Self::Stopped => open_registry(cache, settings),
            Self::Operational | Self::Repair(_) => open_recovery_registry(cache, settings),
        }
    }
}

pub(crate) fn open_index_registry(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    epoch: [u8; 16],
) -> Result<Db> {
    let db = CatalogAccess::Operational.writer(cache, settings)?;
    validate_epoch(db.connection(), epoch)?;
    Ok(db)
}

fn open_operational_reader(
    cache: &Path,
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Db> {
    let db = open_recovery_registry(cache, settings)?;
    db.connection()
        .busy_timeout(settings.settings().read_busy_timeout)
        .map_err(repo::db_error)?;
    registry_inspect::configure_connection(db.connection(), limits).map_err(repo::db_error)?;
    Ok(db)
}

pub(crate) fn observe_index_base(
    cache: &Path,
    project_id: i64,
    epoch: [u8; 16],
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<Option<ObservedProjectIndex>> {
    let initial_bytes = registry_inspect::validate(cache, limits)?;
    registry_inspect::check_identity_budget(limits, initial_bytes, true)
        .map_err(|reason| admission(format!("generation identity unavailable ({reason:?})")))?;
    let db = open_operational_reader(cache, settings, limits)?;
    validate_epoch(db.connection(), epoch)?;
    let project = crate::daemon::registry::project_by_id(&db, project_id)?
        .ok_or_else(|| admission("generation build project is not registered"))?;
    let observed = observe_catalog_index(&db, &project, limits)?;
    db.close()?;
    Ok(observed)
}

/// Capture a usable base, or prove that an explicit full rebuild can replace
/// the selected generation without using its incomplete contents as input.
pub(crate) fn observe_replacement_base(
    cache: &Path,
    project_id: i64,
    epoch: [u8; 16],
    settings: &Resolved<DbSettings>,
    limits: InspectionLimits,
) -> Result<(Option<ObservedProjectIndex>, Option<ReplacementBase>)> {
    match observe_index_base(cache, project_id, epoch, settings, limits) {
        Ok(base) => return Ok((base, None)),
        Err(error) if lifecycle::is_lease_conflict(&error) => return Err(error),
        Err(_) => {}
    }
    let db = open_operational_reader(cache, settings, limits)?;
    validate_epoch(db.connection(), epoch)?;
    let replacement = replacement::inspect_for_replacement(&db, project_id, limits)?;
    db.close()?;
    if let Some(base) = replacement {
        return Ok((Some(base.index), Some(base)));
    }
    // SQLite may have rolled back a journal left before the intent committed.
    Ok((
        observe_index_base(cache, project_id, epoch, settings, limits)?,
        None,
    ))
}
