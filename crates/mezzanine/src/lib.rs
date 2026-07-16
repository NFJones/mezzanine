//! Product composition library for Mezzanine.
//!
//! The application crate owns CLI bootstrap, product protocols, security,
//! persistence, concrete integrations, host I/O, user-interface adapters, and
//! serialized runtime composition. Reusable domain contracts live in the four
//! lower workspace crates and are not re-exported from this library.

mod cli;
mod config;
mod control;
mod error;
mod host;
mod integrations;
mod protocol;
mod runtime;
mod security;
mod storage;
#[cfg(test)]
mod test_support;
mod ui;

/// Intentionally supported control-client wire helpers.
///
/// External clients can frame and decode JSON-RPC control messages without
/// gaining access to the server dispatcher, runtime state, or internal control
/// records.
pub mod control_client {
    pub use crate::control::{decode_control_frame, encode_control_body};
}

pub use error::{MezError, MezErrorKind, Result};

/// Runs the product command-line workflow and returns the process exit code.
pub async fn run_cli() -> u8 {
    cli::run().await
}
