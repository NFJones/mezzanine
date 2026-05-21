//! Main implementation.
//!
//! This module owns the main boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::process::ExitCode;

/// Runs the main operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    ExitCode::from(mezzanine::cli::run().await)
}
