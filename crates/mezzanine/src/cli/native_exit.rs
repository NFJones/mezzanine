//! Native cleanup ownership at the CLI/library/process boundary.
//!
//! A command's numeric status is not ownership. The foreground native owner or
//! existing host supervisor is captured before startup and outlives command
//! errors and Tokio destruction. Programmatic callers receive an opaque
//! outcome that can inspect and retry exact cleanup without extracting PIDs.
//! Reporting/dropping that outcome deliberately abandons this retry capability;
//! it never proves OS process exit or native retirement. No global registry,
//! runtime-in-Drop, detached task, or generic error payload is introduced here.

use std::fmt;
use std::io::{self, Write};
use std::process::{ExitCode, Termination};
use std::time::Duration;

use mez_mux::process::PaneProcessRetirementOwner;

use crate::error::{MezError, Result};
use crate::host::async_runtime::drain_native_pane_retirement;
use crate::host::session::SessionSupervisor;

/// Opaque command status retaining any unproven native cleanup ownership.
///
/// Keep this value while cleanup is pending and use `retry_native_cleanup`
/// under a caller-owned Tokio runtime. `Termination::report` is also callable
/// by library users: it means explicit abandonment, NOT proof of process exit
/// or child reaping. No implicit consuming numeric conversion is provided.
/// Debug reports only effective code and a coarse pending count, never retained
/// diagnostics, child handles or private host/session state.
#[must_use = "retain this outcome to inspect/retry native cleanup; dropping/reporting it abandons unproven ownership"]
pub struct CliExit {
    code: u8,
    native: CliNativeOwnership,
}

impl fmt::Debug for CliExit {
    /// Projects only already-approved status; query errors become coarse None,
    /// and their diagnostic payload is never formatted or traversed.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = self.pending_native_children().ok();
        formatter
            .debug_struct("CliExit")
            .field("code", &self.code())
            .field("pending_native_children", &pending)
            .finish_non_exhaustive()
    }
}

impl CliExit {
    /// Builds a dependency-free status for internal children and display paths.
    pub(crate) fn plain(code: u8) -> Self {
        Self {
            code,
            native: CliNativeOwnership::default(),
        }
    }

    /// Transfers command-scoped native ownership into the public outcome.
    pub(super) fn owned(code: u8, native: CliNativeOwnership) -> Self {
        Self { code, native }
    }

    /// Borrows the exit status, preserving any original nonzero command code.
    /// Pending/unknown cleanup or an earlier cleanup failure forces nonzero.
    pub fn code(&self) -> u8 {
        if self.code != 0 {
            return self.code;
        }
        if self.native.cleanup_failed
            || !self.native.is_sealed()
            || !matches!(self.pending_native_children(), Ok(0))
            || !matches!(self.native.has_retained_failure(), Ok(false))
        {
            1
        } else {
            0
        }
    }

    /// Counts exact Reserved/Leased/Returned/Draining child obligations.
    /// This is point-in-time status, not a substitute for sealed bounded drain.
    /// A poisoned host status lock is an error, never a fabricated zero.
    pub fn pending_native_children(&self) -> Result<usize> {
        self.native.pending_count()
    }

    /// Retries exact native cleanup under the caller's current Tokio runtime.
    ///
    /// One absolute `budget` covers sealing, host startup cancellation, every
    /// session and foreground child. Timeout, error or cancellation leaves the
    /// same owners in this outcome. Original command errors and cleanup failure
    /// status are sticky even if a later retry succeeds. Empty outcomes require
    /// no runtime; nonempty outcomes without a current runtime return an error.
    pub async fn retry_native_cleanup(&mut self, budget: Duration) -> Result<()> {
        if self.native.is_empty() {
            return Ok(());
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(MezError::invalid_state(
                "native cleanup retry requires a caller-owned Tokio runtime",
            ));
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(budget)
            .ok_or_else(|| {
                MezError::invalid_args("native cleanup retry budget is not representable")
            })?;
        self.native.cleanup(deadline).await
    }
}

impl Termination for CliExit {
    /// Converts status while explicitly abandoning remaining retry ownership.
    fn report(self) -> ExitCode {
        ExitCode::from(self.code())
    }
}

impl Drop for CliExit {
    /// Publishes an unproven-abandonment diagnostic without waiting or panicking.
    fn drop(&mut self) {
        if !matches!(self.pending_native_children(), Ok(0)) {
            let _ = writeln!(
                io::stderr().lock(),
                "mez: abandoning unproven native cleanup ownership; process exit is not a reap receipt"
            );
        }
    }
}

/// Lazy one-command ownership, allocated only by actual foreground/host serve.
#[derive(Debug, Default)]
pub(super) struct CliNativeOwnership {
    foreground: Option<PaneProcessRetirementOwner>,
    host: Option<SessionSupervisor>,
    cleanup_failed: bool,
    pub(super) cleanup_diagnostic: Option<String>,
}

impl CliNativeOwnership {
    /// Captures foreground ownership before the factory or first native spawn.
    pub(super) fn foreground(&mut self) -> PaneProcessRetirementOwner {
        self.foreground
            .get_or_insert_with(PaneProcessRetirementOwner::default)
            .clone()
    }

    /// Retains the existing host supervisor before startup/stdout can fail.
    pub(super) fn retain_host(&mut self, host: SessionSupervisor) -> Result<()> {
        if self.host.is_some() {
            return Err(MezError::invalid_state(
                "CLI native host owner is already installed",
            ));
        }
        self.host = Some(host);
        Ok(())
    }

    /// Distinguishes dependency-free status from actual native serve ownership.
    fn is_empty(&self) -> bool {
        self.foreground.is_none() && self.host.is_none()
    }

    /// Zero is terminal evidence only after every source of admission is sealed.
    fn is_sealed(&self) -> bool {
        self.foreground
            .as_ref()
            .is_none_or(PaneProcessRetirementOwner::is_sealed)
            && self
                .host
                .as_ref()
                .is_none_or(SessionSupervisor::native_admission_is_sealed)
    }

    /// Failure evidence is independent of cardinality and cannot become healthy
    /// merely because a cancelled future's last child was eventually reaped.
    fn has_retained_failure(&self) -> Result<bool> {
        if self
            .foreground
            .as_ref()
            .is_some_and(PaneProcessRetirementOwner::has_cleanup_failure)
        {
            return Ok(true);
        }
        self.host.as_ref().map_or(Ok(false), |host| {
            host.native_cleanup_failure()
                .map(|failure| failure.is_some())
        })
    }

    /// Counts retained native obligations without consulting an actor channel.
    pub(super) fn pending_count(&self) -> Result<usize> {
        let foreground = self
            .foreground
            .as_ref()
            .map_or(0, PaneProcessRetirementOwner::outstanding);
        let host = self
            .host
            .as_ref()
            .map_or(Ok(0), SessionSupervisor::native_pending_count)?;
        foreground
            .checked_add(host)
            .ok_or_else(|| MezError::invalid_state("native cleanup count overflow"))
    }

    /// Drains while Tokio is alive; ownership remains here through runtime Drop.
    pub(super) async fn cleanup(&mut self, deadline: tokio::time::Instant) -> Result<()> {
        let mut failure: Option<MezError> = None;
        if let Some(owner) = &self.foreground {
            owner.seal();
        }
        if let Some(host) = &self.host
            && let Err(error) = host.seal_native_admission()
        {
            failure = Some(error);
        }
        if let Some(owner) = &self.foreground
            && let Err(error) = drain_native_pane_retirement(owner, deadline).await
        {
            failure.get_or_insert(error);
        }
        if let Some(host) = &self.host
            && let Err(error) = host.drain_native_retirement(deadline).await
        {
            failure.get_or_insert(error);
        }
        if let Some(error) = failure {
            self.cleanup_failed = true;
            self.cleanup_diagnostic = Some(error.to_string());
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates only a test-owned PTY child with its mandatory pre-spawn slot.
    fn live_native_outcome(
        code: u8,
        gate: bool,
    ) -> (
        CliExit,
        rustix::process::Pid,
        Option<mez_mux::process::PaneProcessChildReservation>,
    ) {
        let mut native = CliNativeOwnership::default();
        let owner = native.foreground();
        let gate = gate.then(|| owner.reserve().unwrap());
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            mez_mux::layout::Size::new(80, 24).unwrap(),
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        drop(process);
        (CliExit::owned(code, native), pid, gate)
    }

    /// Internal-child/nonzero command status is preserved without allocating
    /// native ownership or constructing a runtime, including at report().
    #[test]
    fn plain_exit_preserves_exact_codes_and_dependency_free_empty_ownership() {
        for code in [0, 1, 17, 101, 255] {
            let exit = CliExit::plain(code);
            assert!(exit.native.is_empty());
            assert_eq!(exit.pending_native_children().unwrap(), 0);
            assert_eq!(exit.code(), code);
            assert_eq!(exit.report(), ExitCode::from(code));
        }
    }

    /// Public ordinary/pretty Debug must not traverse synthetic private
    /// foreground diagnostics, native owner internals or host entry/receipt
    /// state. This is formatting privacy coverage, not native resource proof.
    #[test]
    fn native_exit_debug_exposes_only_bounded_coarse_status() {
        let host = SessionSupervisor::private_diagnostic_fixture_for_test();
        for original in [0, 17] {
            let mut native = CliNativeOwnership::default();
            native.foreground().seal();
            native.cleanup_failed = true;
            native.cleanup_diagnostic = Some(
                "PRIVATE_NATIVE_FOREGROUND_CANARY Bearer SYNTHETIC /private/foreground-debug"
                    .to_string(),
            );
            native.retain_host(host.clone()).unwrap();
            let exit = CliExit::owned(original, native);
            let expected = if original == 0 { 1 } else { original };
            assert_eq!(exit.code(), expected);
            for shown in [format!("{exit:?}"), format!("{exit:#?}")] {
                assert!(shown.contains("CliExit") && shown.contains(&format!("code: {expected}")));
                assert!(shown.contains("pending_native_children"));
                assert!(shown.len() < 256);
                for private in [
                    "PRIVATE_NATIVE_",
                    "/private/",
                    "Bearer",
                    "SYNTHETIC",
                    "cleanup_diagnostic",
                    "CliNativeOwnership",
                    "SessionSupervisor",
                    "PaneProcessRetirementOwner",
                    "RetirementState",
                    "RawPaneChild",
                    "Mutex",
                ] {
                    assert!(
                        !shown.contains(private),
                        "public Debug exposed a private synthetic marker: {private}"
                    );
                }
            }
        }
    }

    /// A late private failure receipt can arrive with zero remaining children.
    /// Borrowed public status must remain nonzero after entry/runtime teardown;
    /// this is a receipt-classification test, not fabricated native reap proof.
    #[test]
    fn native_exit_zero_pending_does_not_erase_late_failure_receipt() {
        let host = SessionSupervisor::new(0);
        host.seal_native_admission().unwrap();
        let mut native = CliNativeOwnership::default();
        native.retain_host(host.clone()).unwrap();
        let exit = CliExit::owned(0, native);
        assert_eq!(exit.code(), 0);
        host.retain_native_failure_for_test();
        assert_eq!(exit.pending_native_children().unwrap(), 0);
        assert_eq!(exit.code(), 1);
    }

    /// Public status must fail closed on an unknown owner count, rather than
    /// converting a private status failure into successful empty cleanup.
    #[test]
    fn native_exit_unknown_status_forces_nonzero_without_erasing_original_code() {
        let host = SessionSupervisor::new(0);
        host.poison_native_status_for_test();
        assert!(host.seal_native_admission().is_err());
        for original in [0, 17] {
            let mut native = CliNativeOwnership::default();
            native.retain_host(host.clone()).unwrap();
            let exit = CliExit::owned(original, native);
            assert!(exit.pending_native_children().is_err());
            assert_eq!(exit.code(), if original == 0 { 1 } else { original });
            for shown in [format!("{exit:?}"), format!("{exit:#?}")] {
                assert!(shown.contains("None"));
                assert!(!shown.contains("poison") && !shown.contains("unavailable"));
                assert!(!shown.contains("SessionSupervisor") && shown.len() < 256);
            }
        }
    }

    /// Expired/overflow budgets cannot signal, reap or discard an exact child.
    /// A later explicit retry proves immediate native absence, while an earlier
    /// observed cleanup failure remains a nonzero exit verdict.
    #[tokio::test]
    async fn pending_exit_rejects_expired_or_overflow_budget_and_retains_for_retry() {
        let (mut exit, pid, _) = live_native_outcome(0, false);
        assert!(exit.retry_native_cleanup(Duration::MAX).await.is_err());
        assert!(exit.retry_native_cleanup(Duration::ZERO).await.is_err());
        assert_eq!(exit.pending_native_children().unwrap(), 1);
        assert_eq!(exit.code(), 1);
        exit.retry_native_cleanup(Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert_eq!(exit.pending_native_children().unwrap(), 0);
        assert_eq!(
            exit.code(),
            1,
            "cleanup failure verdict is intentionally sticky"
        );
    }

    /// A cancelled retry still owns every reservation. Another explicit retry
    /// can settle after the gate is released; original command codes never clear.
    #[tokio::test]
    async fn cancelled_exit_retry_retains_owner_and_original_nonzero_code() {
        let (mut exit, pid, gate) = live_native_outcome(17, true);
        let cancelled = {
            let retry = exit.retry_native_cleanup(Duration::from_secs(1));
            tokio::pin!(retry);
            tokio::time::timeout(Duration::from_millis(30), &mut retry)
                .await
                .is_err()
        };
        assert!(cancelled);
        assert_eq!(exit.code(), 17);
        assert!(exit.pending_native_children().unwrap() >= 1);
        drop(gate);
        exit.retry_native_cleanup(Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert_eq!(exit.code(), 17);
    }

    /// Tokio destruction can return a child after the command's bounded drain
    /// has failed. The public outcome, not a local helper variable, must retain
    /// that exact owner across destruction and permit caller-runtime retry.
    #[test]
    fn exit_owner_survives_late_child_return_during_runtime_destruction() {
        let mut native = CliNativeOwnership::default();
        let owner = native.foreground();
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            mez_mux::layout::Size::new(80, 24).unwrap(),
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let worker = runtime.spawn(async move {
            let _process = process;
            std::future::pending::<()>().await;
        });
        let mut exit = CliExit::owned(0, native);
        assert!(
            runtime
                .block_on(exit.retry_native_cleanup(Duration::from_millis(30)))
                .is_err()
        );
        drop(runtime);
        assert!(worker.is_finished());
        assert_eq!(exit.pending_native_children().unwrap(), 1);
        assert_eq!(exit.code(), 1);
        let retry_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        retry_runtime
            .block_on(exit.retry_native_cleanup(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert_eq!(exit.pending_native_children().unwrap(), 0);
    }
}
