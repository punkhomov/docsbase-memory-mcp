//! Versioned IPC over the daemon local socket (FR-7, FR-33; ADR-9).

pub mod async_client;
pub mod client;
pub(crate) mod framing;
pub mod protocol;
pub mod settings;

#[cfg(test)]
mod async_client_tests;

pub mod panel;

pub(crate) mod encoding;

pub mod transport;
