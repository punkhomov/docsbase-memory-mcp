//! Daemon-side modules: registry, lifecycle, admission, sessions, server.

pub mod admission;
pub mod launch;
pub mod lifecycle;
pub mod readiness;
pub mod registry;
pub mod remote;
pub(crate) mod request;
pub mod server;
pub mod session;
pub mod settings;
pub mod tools;
pub mod worker_admission;
