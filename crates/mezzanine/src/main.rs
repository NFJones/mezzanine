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
fn main() -> ExitCode {
    if let Some(exit_code) = mezzanine::internal_process_exit_code() {
        return ExitCode::from(exit_code);
    }
    let worker_threads = match mezzanine::configured_runtime_cpu_count() {
        Ok(worker_threads) => worker_threads,
        Err(error) => {
            eprintln!("mez: {error}");
            return ExitCode::from(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("mez: failed to construct Tokio runtime: {error}");
            return ExitCode::from(1);
        }
    };
    ExitCode::from(runtime.block_on(mezzanine::run_cli()))
}
