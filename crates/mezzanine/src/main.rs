//! Thin product entry point; CLI ownership selects startup dependencies.

/// Preserves CLI and exact internal-child exit codes at the process boundary.
fn main() -> mezzanine::CliExit {
    mezzanine::run_cli()
}
