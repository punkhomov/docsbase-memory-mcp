//! Daemon-side modules: registry, lifecycle, admission, sessions, server.

pub mod admission;
pub mod launch;
pub mod lifecycle;
pub mod registry;
mod request;
pub mod server;
pub mod session;
pub mod settings;
pub mod tools;
