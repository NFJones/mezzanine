//! Exact worker handles and bounded failure evidence without invented shell I/O.
//!
//! Never-started evidence is produced only while dispatch remains owned before
//! spawn. A join failure cannot prove absent effects and therefore stays unknown.
//! Neither outcome permits replay or certifies canonical/durable settlement.

use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;
use mez_agent::native_action::{
    NativeCommitIdentity, NativeEffectCertainty, NativePatchOutcome, NativePatchPhase,
};
use std::sync::Arc;

/// Request-local actual producer custody, independent of observer lifetimes.
pub(super) enum Execution {
    /// No blocking task was ever admitted by this producer owner.
    Unstarted,
    /// Actual handle remains here while monitors borrow it across await.
    Running {
        /// Actual handle remains owned while an observer borrows it.
        worker: tokio::task::JoinHandle<Arc<NativePatchWorkerOutcome>>,
        /// Original immutable identity survives even an unexpected join failure.
        work: Arc<crate::runtime::agent::native_filesystem::NativeFilesystemWork>,
    },
    /// Original immutable observed outcome remains until checked handoff.
    Joined(Arc<NativePatchWorkerOutcome>),
}

/// Builds positive never-started evidence from the still-owned dispatch slot.
pub(super) fn unstarted(identity: &NativeCommitIdentity) -> NativePatchWorkerOutcome {
    failure(
        identity,
        NativeEffectCertainty::Unexecuted,
        "native filesystem producer retired before start",
    )
}

/// Retains join failure as unknown; arbitrary panic payloads are never exposed.
pub(super) fn lost(identity: &NativeCommitIdentity, cancelled: bool) -> NativePatchWorkerOutcome {
    failure(
        identity,
        NativeEffectCertainty::Unknown,
        if cancelled {
            "native filesystem producer join cancelled; effects require reconciliation"
        } else {
            "native filesystem producer join failed; effects require reconciliation"
        },
    )
}

/// Constructs typed bounded ownership evidence without fabricated file receipts.
fn failure(
    identity: &NativeCommitIdentity,
    certainty: NativeEffectCertainty,
    diagnostic: &str,
) -> NativePatchWorkerOutcome {
    NativePatchWorkerOutcome {
        result: NativePatchOutcome {
            identity: identity.clone(),
            phase: NativePatchPhase::Settle,
            certainty,
            confirmed_effects: Vec::new(),
            diagnostic: Some(diagnostic.to_owned()),
        },
        receipts: Vec::new(),
        completed_operations: Vec::new(),
    }
}
