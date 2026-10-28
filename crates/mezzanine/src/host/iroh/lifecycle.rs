//! Typed failure scope after observed optional-host transport cleanup.
//!
//! An unexpected endpoint close is transport-only only when endpoint close and
//! every connection task drain completed without degradation. This decision is
//! made from owned cleanup outcomes, never an error message or a peer-supplied
//! failure. Reporting grants no lease, replay, restart or retirement authority.

use tokio::task::JoinSet;

use crate::error::{MezError, Result};

/// Explicit host-owner disposition after the remote serving future has ended.
#[derive(Debug)]
pub(crate) enum HostIrohServeError {
    /// Transport is gone but all of its task ownership has been settled.
    TransportUnavailable(MezError),
    /// Cleanup or shared-owner safety cannot be established; stop the host.
    Critical(MezError),
}

impl HostIrohServeError {
    /// Preserves the full original typed error for final host diagnostics.
    pub(crate) fn into_error(self) -> MezError {
        match self {
            Self::TransportUnavailable(error) | Self::Critical(error) => error,
        }
    }
}

/// Privacy-safe aggregate of persistent-host Iroh cleanup degradation.
#[derive(Debug, Default)]
pub(super) struct HostIrohShutdownReport {
    /// Closing the owned endpoint exceeded its existing transport budget.
    pub(super) endpoint_close_failed: bool,
    /// Join failures other than cancellation, without panic payload retention.
    pub(super) task_join_failures: usize,
    /// Task cancellations observed before the authorized abort phase.
    pub(super) unexpected_task_cancellations: usize,
    /// Tasks that required bounded-drain escalation before being joined.
    pub(super) forced_aborts: usize,
}

impl HostIrohShutdownReport {
    /// Isolates only unexpected endpoint loss with every cleanup stage proven.
    /// Any close timeout, failed join, unexpected cancellation or forced abort
    /// retains whole-host failure scope even when endpoint loss happened first.
    pub(super) fn finish_scoped(
        self,
        accepted: u64,
        endpoint_closed_unexpectedly: bool,
    ) -> std::result::Result<u64, HostIrohServeError> {
        let isolated = endpoint_closed_unexpectedly
            && !self.endpoint_close_failed
            && self.task_join_failures == 0
            && self.unexpected_task_cancellations == 0
            && self.forced_aborts == 0;
        self.finish(accepted, endpoint_closed_unexpectedly)
            .map_err(|error| {
                if isolated {
                    HostIrohServeError::TransportUnavailable(error)
                } else {
                    HostIrohServeError::Critical(error)
                }
            })
    }

    /// Records one connection-task join without retaining panic payloads or
    /// peer-identifying context.
    pub(super) fn record_task_completion(
        &mut self,
        joined: std::result::Result<(), tokio::task::JoinError>,
        cancellation_expected: bool,
    ) {
        let Err(error) = joined else {
            return;
        };
        if error.is_cancelled() {
            if !cancellation_expected {
                self.unexpected_task_cancellations =
                    self.unexpected_task_cancellations.saturating_add(1);
            }
        } else {
            self.task_join_failures = self.task_join_failures.saturating_add(1);
        }
    }

    /// Returns the accepted count only for a fully clean listener and cleanup
    /// outcome; otherwise returns one deterministic aggregate service error.
    pub(super) fn finish(self, accepted: u64, endpoint_closed_unexpectedly: bool) -> Result<u64> {
        let mut failures = Vec::new();
        if endpoint_closed_unexpectedly {
            failures.push("persistent host Iroh listener closed unexpectedly".to_string());
        }
        if self.endpoint_close_failed {
            failures.push("endpoint close timed out".to_string());
        }
        if self.task_join_failures != 0 {
            failures.push(format!(
                "{} connection task joins failed",
                self.task_join_failures
            ));
        }
        if self.unexpected_task_cancellations != 0 {
            failures.push(format!(
                "{} connection tasks were cancelled unexpectedly",
                self.unexpected_task_cancellations
            ));
        }
        if self.forced_aborts != 0 {
            failures.push(format!(
                "{} connection tasks required forced abort",
                self.forced_aborts
            ));
        }
        if failures.is_empty() {
            return Ok(accepted);
        }
        Err(MezError::invalid_state(format!(
            "{} after accepting {accepted} connections",
            failures.join("; ")
        )))
    }
}

/// Drains every persistent-host connection task within policy, then aborts and
/// joins all remaining work while retaining privacy-safe degradation counts.
pub(super) async fn drain_host_iroh_connection_tasks(
    tasks: &mut JoinSet<()>,
    timeout: std::time::Duration,
    report: &mut HostIrohShutdownReport,
) {
    let drain = async {
        while let Some(joined) = tasks.join_next().await {
            report.record_task_completion(joined, false);
        }
    };
    if tokio::time::timeout(timeout, drain).await.is_ok() {
        return;
    }
    report.forced_aborts = report.forced_aborts.saturating_add(tasks.len());
    tasks.abort_all();
    while let Some(joined) = tasks.join_next().await {
        report.record_task_completion(joined, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only isolated endpoint loss with clean ownership settlement permits the
    /// host to continue. Each independent cleanup failure must restore critical
    /// scope regardless of the same unexpected-close diagnostic text.
    #[test]
    fn remote_failure_scope_requires_every_cleanup_stage_to_succeed() {
        assert!(matches!(
            HostIrohShutdownReport::default().finish_scoped(0, true),
            Err(HostIrohServeError::TransportUnavailable(_))
        ));
        for report in [
            HostIrohShutdownReport {
                endpoint_close_failed: true,
                ..Default::default()
            },
            HostIrohShutdownReport {
                task_join_failures: 1,
                ..Default::default()
            },
            HostIrohShutdownReport {
                unexpected_task_cancellations: 1,
                ..Default::default()
            },
            HostIrohShutdownReport {
                forced_aborts: 1,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                report.finish_scoped(0, true),
                Err(HostIrohServeError::Critical(_))
            ));
        }
        assert_eq!(
            HostIrohShutdownReport::default()
                .finish_scoped(7, false)
                .unwrap(),
            7
        );
    }
}
