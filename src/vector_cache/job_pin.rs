//! Private move-only negative lease authority. Persistent allocation never reuses an id.
use super::{VectorCacheSource, now_secs};
use crate::error::{Error, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const COORD_LEASE: i64 = i64::MIN;
pub(super) const COORD_MODEL: &str = "\0docsbase:job-pin-sequence:v1";
pub(super) const APPLICATION_ID: i32 = 0x4442_4A31;

/// Never exposed as a raw signed-id constructor. Exists only in admitted native state.
pub(crate) struct JobPinAuthority {
    id: i64,
    root: PathBuf,
    source: VectorCacheSource,
    model: String,
    ttl: Duration,
    hashes: HashSet<String>,
    max_hashes: usize,
    retained: Option<std::sync::Arc<crate::search::budget::PayloadPermit>>,
}
impl JobPinAuthority {
    pub(crate) fn allocate(
        root: &Path,
        source: VectorCacheSource,
        model: String,
        ttl: Duration,
        max_hashes: usize,
    ) -> Result<Self> {
        source.validate_root(root)?;
        let mut cache = source.connect(root)?;
        let tx = cache
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let marker: i32 = tx
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(db_error)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM vector_cache_leases WHERE lease_id=?1",
                [COORD_LEASE],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if marker == 0 && count == 0 {
            let negative: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM vector_cache_leases WHERE lease_id<0",
                    [],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if negative != 0 {
                return Err(invalid(
                    "uninitialized vector pin format has negative leases",
                ));
            }
            tx.pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(db_error)?;
            tx.execute("INSERT INTO vector_cache_leases(lease_id,model_id,chunk_hash,expires_at) VALUES(?1,?2,'0',?3)", params![COORD_LEASE, COORD_MODEL, i64::MAX]).map_err(db_error)?;
        } else if marker != APPLICATION_ID || count != 1 {
            return Err(invalid(
                "missing or incompatible vector pin coordination state",
            ));
        }
        let (encoded, expiry): (String, i64) = tx.query_row("SELECT chunk_hash,expires_at FROM vector_cache_leases WHERE lease_id=?1 AND model_id=?2", params![COORD_LEASE, COORD_MODEL], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?.ok_or_else(|| invalid("missing vector pin sequence"))?;
        let last = encoded
            .parse::<i64>()
            .map_err(|_| invalid("corrupt vector pin sequence"))?;
        if last > 0 || last == i64::MIN || last.to_string() != encoded || expiry != i64::MAX {
            return Err(invalid("corrupt vector pin sequence"));
        }
        let older: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM vector_cache_leases WHERE lease_id<0 AND lease_id<>?1 AND lease_id<?2)", params![COORD_LEASE,last], |r| r.get(0)).map_err(db_error)?;
        if older {
            return Err(invalid("vector pin sequence is behind issued authority"));
        }
        let id = last
            .checked_sub(1)
            .filter(|id| *id != i64::MIN)
            .ok_or_else(|| invalid("vector pin sequence exhausted"))?;
        tx.execute(
            "UPDATE vector_cache_leases SET chunk_hash=?1 WHERE lease_id=?2 AND model_id=?3",
            params![id.to_string(), COORD_LEASE, COORD_MODEL],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        let mut hashes = HashSet::new();
        hashes
            .try_reserve(max_hashes)
            .map_err(|e| Error::internal_with_source("allocate accounted pin tracking", e))?;
        Ok(Self {
            id,
            root: root.to_path_buf(),
            source,
            model,
            ttl,
            hashes,
            max_hashes,
            retained: None,
        })
    }
    pub(crate) fn retain_account(
        &mut self,
        account: std::sync::Arc<crate::search::budget::PayloadPermit>,
    ) {
        self.retained = Some(account);
    }
    pub(crate) fn track(&mut self, hash: &str) -> Result<()> {
        if !self.hashes.contains(hash) {
            if self.hashes.len() >= self.max_hashes {
                return Err(invalid("accounted job pin hash bound exceeded"));
            }
            self.hashes.insert(hash.to_owned());
        }
        Ok(())
    }
    pub(crate) fn renew(&self) -> Result<()> {
        let expiry = now_secs()
            .checked_add(
                i64::try_from(self.ttl.as_secs()).map_err(|_| invalid("job pin TTL overflow"))?,
            )
            .ok_or_else(|| invalid("job pin expiry overflow"))?;
        let mut cache = self.source.connect(&self.root)?;
        let tx = cache
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        for hash in &self.hashes {
            tx.execute("INSERT INTO vector_cache_leases(lease_id,model_id,chunk_hash,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(lease_id,model_id,chunk_hash) DO UPDATE SET expires_at=excluded.expires_at", params![self.id,self.model,hash,expiry]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }
    fn release(&self) -> Result<()> {
        let mut cache = self.source.connect(&self.root)?;
        let tx = cache
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute(
            "DELETE FROM vector_cache_leases WHERE lease_id=?1 AND model_id=?2",
            params![self.id, self.model],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
}
impl Drop for JobPinAuthority {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            eprintln!("warning: cannot release job vector pins: {error}");
        }
    }
}
fn db_error(error: rusqlite::Error) -> Error {
    Error::internal_with_source("job pin coordination", error)
}
fn invalid(message: &str) -> Error {
    Error::Admission {
        message: message.into(),
    }
}
