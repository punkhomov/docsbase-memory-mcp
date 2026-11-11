//! Explicit metadata sources. Prepared leases never open a query connection.

use std::ops::Deref;
use std::path::Path;
use std::time::Instant;

use crate::error::{Error, Result};
use crate::search::operation::SearchOperation;
use crate::store::Db;
use crate::store::read_pool::{ReadLease, ReadPool};

/// Composition boundary for daemon readers and caller-owned direct searches.
#[derive(Clone, Copy)]
pub enum MetadataSource<'a> {
    /// The caller retains this handle; search requires an autocommit connection.
    Borrowed(&'a Db),
    /// Bounded operational readers prepared once at daemon startup.
    Prepared(&'a ReadPool),
}

impl<'a> MetadataSource<'a> {
    /// Cache identity, available without acquiring a reader or filesystem IO.
    #[must_use]
    pub fn cache_root(self) -> &'a Path {
        match self {
            Self::Borrowed(db) => db.cache_root(),
            Self::Prepared(pool) => pool.cache_root(),
        }
    }

    pub(crate) fn acquire(self, operation: &SearchOperation) -> Result<MetadataReader<'a>> {
        match self {
            Self::Borrowed(db) => borrowed(db),
            Self::Prepared(pool) => {
                let started = Instant::now();
                let reader = pool.acquire(operation.deadline());
                operation.record_queue(started.elapsed());
                reader.map(MetadataReader::Prepared)
            }
        }
    }

    pub(crate) fn try_acquire(self) -> Result<MetadataReader<'a>> {
        match self {
            Self::Borrowed(db) => borrowed(db),
            Self::Prepared(pool) => pool.try_acquire().map(MetadataReader::Prepared),
        }
    }
}

fn borrowed(db: &Db) -> Result<MetadataReader<'_>> {
    if !db.connection().is_autocommit() {
        return Err(Error::Admission {
            message: "search requires an autocommit metadata reader; finish the caller transaction"
                .to_owned(),
        });
    }
    Ok(MetadataReader::Borrowed(db))
}

pub(crate) enum MetadataReader<'a> {
    Borrowed(&'a Db),
    Prepared(ReadLease<'a>),
}

impl Deref for MetadataReader<'_> {
    type Target = Db;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(db) => db,
            Self::Prepared(reader) => reader,
        }
    }
}
