//! Explicit per-workload launch accounting at native process owners.
//!
//! This is instrumentation/admission, not an OS execution-denial sandbox.
//! It observes direct launches at participating owners (including absolute
//! executables and self-reexecution), not descendants spawned by shell code.
//! Pass the ledger explicitly across workers; never use thread-local/global
//! counters that confuse concurrent operations. No command/environment bytes
//! are retained. A process-free workload rejects every reason before spawn.

use std::sync::{Arc, Mutex};

use crate::error::{MezError, Result};

/// Why a participating native owner attempts a child launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeLaunchReason {
    /// Intentional model-authored shell command (possibly sandbox wrapped).
    ShellCommand,
    /// Legacy semantic patch shell; forbidden in the process-free target.
    LegacyPatch,
    /// OS backend capability probe, never a native filesystem prerequisite.
    SandboxProbe,
    /// Explicit command-backed status provider, outside basic-action scope.
    StatusProvider,
}

/// Bounded counters; absence of a ledger snapshot is not evidence of zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NativeLaunchCount {
    /// Declared launch purpose.
    pub(crate) reason: NativeLaunchReason,
    /// Attempts, including denied or failed spawn calls.
    pub(crate) attempted: u64,
    /// Successful direct child creations, not descendant counts.
    pub(crate) launched: u64,
}

/// One explicitly shared workload's bounded launch evidence/admission policy.
#[derive(Debug, Clone)]
pub(crate) struct NativeLaunchLedger {
    /// True only for an adapter that forbids all child launches.
    process_free: bool,
    /// At most one counter entry per finite reason variant.
    counts: Arc<Mutex<Vec<NativeLaunchCount>>>,
}

impl NativeLaunchLedger {
    /// Creates an observing ledger, or a rejecting process-free ledger.
    pub(crate) fn new(process_free: bool) -> Self {
        Self {
            process_free,
            counts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Records admission before launch and success only after the OS confirms
    /// creation. Poisoned evidence fails closed; the callback is not invoked.
    pub(crate) fn launch<T>(
        &self,
        reason: NativeLaunchReason,
        spawn: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        // Hold this workload-local lock through spawn so no fallible evidence
        // acquisition occurs after child creation. Returning an accounting
        // error then would discard a live Child without lifecycle ownership.
        let mut counts = self
            .counts
            .lock()
            .map_err(|_| MezError::invalid_state("native launch accounting lock poisoned"))?;
        let index = match counts.iter().position(|count| count.reason == reason) {
            Some(index) => index,
            None => {
                counts.push(NativeLaunchCount {
                    reason,
                    attempted: 0,
                    launched: 0,
                });
                counts.len() - 1
            }
        };
        counts[index].attempted = counts[index].attempted.saturating_add(1);
        if self.process_free {
            return Err(MezError::invalid_state(format!(
                "process-free native operation rejected child launch: {reason:?}"
            )));
        }
        let result = spawn()?;
        counts[index].launched = counts[index].launched.saturating_add(1);
        Ok(result)
    }

    /// Copies bounded evidence, or reports unavailable evidence after poisoning.
    pub(crate) fn snapshot(&self) -> Option<Vec<NativeLaunchCount>> {
        self.counts.lock().ok().map(|counts| counts.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rejects every participating launch purpose without executing callbacks;
    /// this does not rely on PATH or on a specific executable spelling.
    #[test]
    fn process_free_guard_rejects_all_launch_reasons() {
        let ledger = NativeLaunchLedger::new(true);
        for reason in [
            NativeLaunchReason::ShellCommand,
            NativeLaunchReason::LegacyPatch,
            NativeLaunchReason::SandboxProbe,
            NativeLaunchReason::StatusProvider,
        ] {
            assert!(
                ledger
                    .launch::<()>(reason, || panic!("forbidden spawn callback ran"))
                    .is_err()
            );
        }
        let evidence = ledger.snapshot().unwrap();
        assert_eq!(evidence.len(), 4);
        assert!(
            evidence
                .iter()
                .all(|count| count.attempted == 1 && count.launched == 0)
        );
    }

    /// Shared operation counters distinguish failed attempts from creations,
    /// without contaminating another concurrent operation's ledger.
    #[test]
    fn ledgers_are_workload_scoped_and_count_failures_truthfully() {
        let ledger = NativeLaunchLedger::new(false);
        let other = NativeLaunchLedger::new(false);
        ledger
            .clone()
            .launch(NativeLaunchReason::ShellCommand, || Ok(()))
            .unwrap();
        assert!(
            ledger
                .launch::<()>(NativeLaunchReason::ShellCommand, || Err(
                    MezError::invalid_state("spawn failed")
                ))
                .is_err()
        );
        assert_eq!(
            ledger.snapshot().unwrap(),
            vec![NativeLaunchCount {
                reason: NativeLaunchReason::ShellCommand,
                attempted: 2,
                launched: 1
            }]
        );
        assert!(other.snapshot().unwrap().is_empty());
    }
}
