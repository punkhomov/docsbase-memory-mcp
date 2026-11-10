//! Shared byte admission for owned search payloads; no payload allocation or IO.

use std::sync::{Arc, Mutex, PoisonError};

use crate::config::schema::{Capabilities, SettingsSpec};
use crate::error::{Error, Result};

use super::settings::{SearchSettings, SearchSpec};

struct Retained {
    policy: SearchSettings,
    bytes: Mutex<u64>,
}

/// One daemon's payload allowance. Clones share the same retained charge.
#[derive(Clone)]
pub struct PayloadBudget {
    retained: Arc<Retained>,
}

impl PayloadBudget {
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
            bytes,
        })
    }
}

/// Retain this non-cloneable guard for the lifetime of the charged payload.
#[must_use = "retain the permit while the charged search payload is live"]
pub struct PayloadPermit {
    retained: Arc<Retained>,
    bytes: u64,
}

impl Drop for PayloadPermit {
    fn drop(&mut self) {
        let mut retained = self
            .retained
            .bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *retained = retained.saturating_sub(self.bytes);
    }
}
