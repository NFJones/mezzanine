//! Connection-local transport ownership and complete task draining.
//!
//! Isolated joins remain payload-free failures, not listener failures. Intentional
//! aborts retain their separate shutdown accounting; shared lifecycle loss is
//! still handled by the listener and actor supervisors.

use super::*;

/// Closes only the exact transport when its serving future unwinds or is aborted.
/// Explicit graceful serving still orders FIN acknowledgement before this drop.
pub(super) struct IrohConnectionTransportOwner(pub(super) iroh::endpoint::Connection);

impl Drop for IrohConnectionTransportOwner {
    fn drop(&mut self) {
        self.0.close(VarInt::from_u32(0), b"connection owner ended");
    }
}

/// Reaps every connection task, recording local failures without early return.
pub(super) async fn drain_iroh_control_tasks(
    tasks: &mut JoinSet<Result<u64>>,
    diagnostics: &RuntimeIrohDiagnostics,
) {
    while let Some(joined) = tasks.join_next().await {
        record_iroh_connection_join(diagnostics, joined, false);
    }
}

/// Records unexpected connection joins without exposing arbitrary panic payloads.
/// Ordinary worker results are already recorded by their owning serving task.
pub(super) fn record_iroh_connection_join(
    diagnostics: &RuntimeIrohDiagnostics,
    joined: std::result::Result<Result<u64>, tokio::task::JoinError>,
    forced_abort: bool,
) {
    if let Err(error) = joined {
        if forced_abort && error.is_cancelled() {
            return;
        }
        diagnostics
            .inner
            .connections_failed
            .fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "mez: Iroh connection task {}",
            if error.is_panic() {
                "panicked"
            } else {
                "was cancelled unexpectedly"
            }
        );
    }
}

/// Waits until the runtime enters a terminal lifecycle state or its publisher
/// disappears, allowing peer-controlled setup to cancel promptly.
pub(super) async fn wait_for_terminal_iroh_lifecycle(
    lifecycle: &mut tokio::sync::watch::Receiver<crate::runtime::RuntimeLifecycleState>,
) {
    loop {
        if terminal_daemon_state(*lifecycle.borrow()) || lifecycle.changed().await.is_err() {
            return;
        }
    }
}

/// Classifies the existing terminal states without changing supervisor policy.
pub(super) fn terminal_daemon_state(state: crate::runtime::RuntimeLifecycleState) -> bool {
    matches!(
        state,
        crate::runtime::RuntimeLifecycleState::Stopping
            | crate::runtime::RuntimeLifecycleState::Killed
            | crate::runtime::RuntimeLifecycleState::Failed
    )
}
