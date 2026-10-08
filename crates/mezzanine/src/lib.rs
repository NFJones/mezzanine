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
mod session_title;
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

/// Runs one exact code-owned internal child mode before configuration or the
/// asynchronous runtime is initialized.
///
/// Internal modes accept only exact code-owned bounded arguments. The hidden
/// read-only harness peer checker also uses this path so it needs no HOME/config
/// or asynchronous runtime. The exact observational event helper builds only
/// its own bounded current-thread I/O runtime, without configured CPU discovery.
/// `None` means ordinary CLI startup should continue.
pub fn internal_process_exit_code() -> Option<u8> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    security::sandbox::seatbelt_probe::run_internal_process(&arguments)
        .or_else(|| security::sandbox::seatbelt_child::run_internal_process(&arguments))
        .or_else(|| runtime::run_internal_editor_process(&arguments))
        .or_else(|| cli::run_internal_harness_peer_process(&arguments))
        .or_else(|| cli::run_internal_harness_source_process(&arguments))
        .or_else(|| cli::run_internal_harness_event_process(&arguments))
}

/// Reads the configured Tokio worker count before constructing the runtime.
pub fn configured_runtime_cpu_count() -> Result<usize> {
    let paths = config::ConfigPaths::from_process_env()?;
    config::runtime_cpu_count_from_primary_config(&paths)
}

/// Runs the product command-line workflow and returns the process exit code.
pub async fn run_cli() -> u8 {
    cli::run().await
}
