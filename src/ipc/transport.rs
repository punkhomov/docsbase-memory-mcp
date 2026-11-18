//! IPC-owned native resource catalog; resolved once by each application root.
use crate::daemon::settings::DaemonSettings;
use crate::error::{Error, Result};
use crate::platform::transport::{self, Options, Transport};
use std::time::Duration;

// Initial native implementation policy. These are resource sizes, not wire
// limits or local consumer fallbacks. Only this catalog initializes them.
const CHUNK_BYTES: usize = 64 * 1024;
const NATIVE_STACK_BYTES: usize = 1024 * 1024;
const APPLICATION_STACK_BYTES: usize = 2 * 1024 * 1024;

/// Resolve complete transport policy from the owning application's physical cap.
/// # Errors
/// Invalid or overflowing resource envelope.
pub fn policy(handles: usize, settings: &DaemonSettings) -> Result<Options> {
    let policy = Options {
        handles,
        chunk_bytes: CHUNK_BYTES,
        helper_stack_bytes: NATIVE_STACK_BYTES,
        work_stack_bytes: APPLICATION_STACK_BYTES,
        drain_poll: Duration::from_millis(settings.drain_poll_ms),
        drain_observation: Duration::from_millis(settings.reply_drain_ms),
    };
    policy
        .validate()
        .map_err(|error| Error::internal_with_source("resolve native transport policy", error))?;
    Ok(policy)
}

/// Application-owned scope preserving the original application error category.
/// # Errors
/// Native scope creation, invalid policy or the original application error.
pub fn run<T: Send>(
    policy: &Options,
    work: impl FnOnce(Transport) -> Result<T> + Send,
) -> Result<T> {
    transport::run(policy, |transport| Ok(work(transport)))
        .map_err(|error| Error::internal_with_source("native IPC scope", error))?
}
