//! Shared byte admission for search preparation and owned payloads; no payload allocation or IO.

use std::sync::{Arc, Mutex, PoisonError};

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::error::{Error, Result};

use super::settings::{SearchSettings, SearchSpec};

struct Retained {
    policy: SearchSettings,
    bytes: Mutex<u64>,
}

/// One daemon's search byte allowance. Clones share the same retained charge.
#[derive(Clone)]
pub struct PayloadBudget {
    retained: Arc<Retained>,
}

impl PayloadBudget {
    /// Complete policy supplied by the composition root; no values are filled.
    #[must_use]
    pub fn settings(&self) -> &SearchSettings {
        &self.retained.policy
    }

    /// Constructs admission from complete settings; never resolves defaults.
    ///
    /// # Errors
    /// Rejects malformed imported settings before creating runtime state.
    pub fn new(policy: SearchSettings) -> Result<Self> {
        SearchSpec::validate(&policy, &Capabilities::current())?;
        Ok(Self {
            retained: Arc::new(Retained {
                policy,
                bytes: Mutex::new(0),
            }),
        })
    }

    /// Additional retained/encoded input is part of the same request allowance.
    #[cfg(feature = "hybrid-remote")]
    pub(crate) fn reserve_additional(&self, existing: u64, bytes: u64) -> Result<PayloadPermit> {
        if existing
            .checked_add(bytes)
            .is_none_or(|total| total > self.retained.policy.max_request_payload_bytes)
        {
            return Err(Error::Admission {
                message: "remote rerank input exceeds max_request_payload_bytes".to_owned(),
            });
        }
        self.reserve(bytes)
    }

    /// Reserves the preflight charge before copying candidate payload. Does
    /// not wait for another request to release its allowance or allocate data.
    ///
    /// # Errors
    /// Admission if a request/aggregate limit would be exceeded, including
    /// arithmetic overflow. A refusal leaves the retained charge unchanged.
    pub fn reserve(&self, bytes: u64) -> Result<PayloadPermit> {
        if bytes > self.retained.policy.max_request_payload_bytes {
            return Err(Error::Admission {
                message: format!(
                    "search payload requires {bytes} bytes, exceeding max_request_payload_bytes={}",
                    self.retained.policy.max_request_payload_bytes
                ),
            });
        }
        let mut retained = self
            .retained
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let next = retained.checked_add(bytes).filter(|next| {
            *next <= self.retained.policy.max_retained_payload_bytes
        }).ok_or_else(|| Error::Admission {
            message: format!(
                "search payload needs {bytes} bytes, exceeding available max_retained_payload_bytes={}",
                self.retained.policy.max_retained_payload_bytes
            ),
        })?;
        *retained = next;
        Ok(PayloadPermit {
            retained: Arc::clone(&self.retained),
            bytes: std::sync::atomic::AtomicU64::new(bytes),
        })
    }
}

/// Retain this non-cloneable guard for the lifetime of the charged payload.
#[must_use = "retain the permit while the charged search payload is live"]
pub struct PayloadPermit {
    retained: Arc<Retained>,
    bytes: std::sync::atomic::AtomicU64,
}

impl PayloadPermit {
    /// Current logical charge retained by this account.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Reserve the checked delta BEFORE allocating or copying retained data.
    /// A refusal changes neither account nor shared aggregate.
    ///
    /// # Errors
    /// Per-account/aggregate limit exceeded or arithmetic overflow.
    pub fn grow_to(&mut self, total: u64) -> Result<()> {
        self.grow_shared(total)
    }

    /// One serialized job/coalescer owns mutations; clones retain the same
    /// existing allowance through CPU, transport and terminal retirement.
    pub(crate) fn grow_shared(&self, total: u64) -> Result<()> {
        let mut retained = self
            .retained
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let current = self.bytes();
        if total <= current {
            return Ok(());
        }
        if total > self.retained.policy.max_request_payload_bytes {
            return Err(Error::Admission {
                message: "job capture exceeds max_request_payload_bytes".into(),
            });
        }
        let next = retained
            .checked_add(total - current)
            .filter(|next| *next <= self.retained.policy.max_retained_payload_bytes)
            .ok_or_else(|| Error::Admission {
                message: "job capture exceeds max_retained_payload_bytes".into(),
            })?;
        *retained = next;
        self.bytes.store(total, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Release only storage already destroyed by this serialized owner.
    pub(crate) fn shrink_shared(&self, total: u64) {
        let mut retained = self
            .retained
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let current = self.bytes();
        if total >= current {
            return;
        }
        *retained -= current - total;
        self.bytes.store(total, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for PayloadPermit {
    fn drop(&mut self) {
        let mut retained = self
            .retained
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *retained = retained.saturating_sub(self.bytes());
    }
}
