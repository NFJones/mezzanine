//! Thin product entry point; CLI ownership selects startup dependencies.

use std::process::ExitCode;

/// Preserves CLI and exact internal-child exit codes at the process boundary.
fn main() -> ExitCode {
    ExitCode::from(mezzanine::run_cli())
}
