//! Invocation-local evidence of an expense writer that never started.
//!
//! This is not a general SQLite retry policy, source authority or durable receipt.
//! Only the final idempotent WAL setup refusal and the expense ledger's failed
//! BEGIN IMMEDIATE may record a base BUSY/LOCKED code. Schema acquisition,
//! statements, COMMIT, extended recovery/snapshot codes and arbitrary I/O never
//! qualify. Each original-work execution creates a fresh witness; an intermediate
//! WAL busy that later succeeds cannot poison a subsequent terminal result.

use super::{ExternalUsageCommit, MezError, Result};

#[cfg(test)]
mod post_writer_tests;
#[cfg(test)]
mod tests;

/// Exact no-expense boundary at which one invocation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalPrecommitPhase {
    /// Idempotent WAL setup failed before an expense transaction was opened.
    WalSetup,
    /// The expense transaction's BEGIN IMMEDIATE failed before it existed.
    LedgerBeginImmediate,
}

/// Raw, allowlisted SQLite refusal metadata; never a credential or ACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExternalPrecommitRefusal {
    /// One of the two explicitly instrumented no-expense boundaries.
    pub(crate) phase: ExternalPrecommitPhase,
    /// Original extended code; only exact base BUSY/LOCKED are eligible.
    pub(crate) extended_code: i32,
}

impl ExternalPrecommitRefusal {
    /// Defends the private retry contract without widening extended-code policy.
    pub(crate) fn is_explicit_precommit(self) -> bool {
        matches!(
            self.phase,
            ExternalPrecommitPhase::WalSetup | ExternalPrecommitPhase::LedgerBeginImmediate
        ) && matches!(
            self.extended_code,
            rusqlite::ffi::SQLITE_BUSY | rusqlite::ffi::SQLITE_LOCKED
        )
    }
}

/// Actual outcome of one original-work invocation, not a reporting receipt.
#[derive(Debug)]
pub(crate) enum ExternalUsageAttempt {
    /// Success or a terminal/uncertain error; it must not automatically replay.
    Completed(Result<ExternalUsageCommit>),
    /// An instrumented acquisition refused before any expense transaction began.
    PrecommitRefused {
        /// Existing diagnostic preserved without a new global error flag.
        error: MezError,
        /// Exact phase and raw SQLite code of this invocation only.
        refusal: ExternalPrecommitRefusal,
    },
}

impl ExternalUsageAttempt {
    /// Flattens private scheduling evidence to the unchanged public store result.
    pub(crate) fn into_result(self) -> Result<ExternalUsageCommit> {
        match self {
            Self::Completed(result) => result,
            Self::PrecommitRefused { error, .. } => Err(error),
        }
    }
}

/// Fresh stack-local observer shared by this attempt's freeze/open/ledger calls.
/// Only storage may record a refusal, immediately before returning that error.
#[derive(Debug, Default)]
pub(crate) struct ExternalUsageAdmissionWitness {
    /// Only this operation's final refused pretransaction boundary.
    refusal: Option<ExternalPrecommitRefusal>,
    /// Fixture-only real SQLite COMMIT-hook refusal, never scheduling evidence.
    #[cfg(test)]
    abort_commit: bool,
}

impl ExternalUsageAdmissionWitness {
    /// Installs a static rejecting SQLite hook only on this test invocation's
    /// already-open ledger connection. The hook creates an actual failed COMMIT;
    /// it never marks precommit evidence or supplies a fabricated worker result.
    #[cfg(test)]
    pub(super) fn install_commit_abort_for_tests(&self, connection: &rusqlite::Connection) {
        if self.abort_commit {
            // SAFETY: the connection owns a live SQLite handle throughout its
            // transaction. The static callback accesses no pointer/context and
            // outlives the connection; SQLite removes it when the handle closes.
            unsafe {
                rusqlite::ffi::sqlite3_commit_hook(
                    connection.handle(),
                    Some(reject_fixture_commit),
                    std::ptr::null_mut(),
                );
            }
        }
    }

    /// Starts a new storage operation without inheriting a prior refusal. The
    /// runtime also creates a fresh witness per attempt; this fence keeps later
    /// validation/schema/statement errors terminal even if a caller reuses one.
    pub(super) fn clear(&mut self) {
        self.refusal = None;
    }

    /// Records only base BUSY/LOCKED at a proven pretransaction return site.
    /// Unexpected extended codes and other errors explicitly leave no witness.
    pub(super) fn refused(&mut self, phase: ExternalPrecommitPhase, error: &rusqlite::Error) {
        self.refusal = match error {
            rusqlite::Error::SqliteFailure(code, _)
                if matches!(
                    code.extended_code,
                    rusqlite::ffi::SQLITE_BUSY | rusqlite::ffi::SQLITE_LOCKED
                ) =>
            {
                Some(ExternalPrecommitRefusal {
                    phase,
                    extended_code: code.extended_code,
                })
            }
            _ => None,
        };
    }

    /// Consumes the invocation observer. Successful outcomes can never request
    /// retry, even if an incorrectly retained witness were present internally.
    pub(crate) fn finish(self, result: Result<ExternalUsageCommit>) -> ExternalUsageAttempt {
        match (result, self.refusal) {
            (Err(error), Some(refusal)) => {
                ExternalUsageAttempt::PrecommitRefused { error, refusal }
            }
            (result, _) => ExternalUsageAttempt::Completed(result),
        }
    }
}

/// Returning nonzero asks SQLite to roll back this generated fixture's COMMIT.
/// No context is dereferenced and no thread-local/global scheduling state exists.
#[cfg(test)]
unsafe extern "C" fn reject_fixture_commit(_: *mut std::ffi::c_void) -> i32 {
    1
}
