//! Connection-local ownership of one negotiated Iroh event task.
//!
//! Control serving and event delivery share a transport lifetime, not session
//! authority. Completion is observed exactly once, nested errors retain their
//! classification, and panic payloads never enter diagnostics. Drop aborts an
//! outstanding task; explicit teardown joins by the connection's deadline.

use crate::error::{MezError, Result};
use tokio::task::JoinHandle;

/// Retains one event worker through live control and bounded teardown.
pub(crate) struct IrohEventTask {
    task: Option<JoinHandle<Result<u64>>>,
}

impl IrohEventTask {
    /// Takes ownership of an optional negotiated event worker.
    pub(crate) fn new(task: Option<JoinHandle<Result<u64>>>) -> Self {
        Self { task }
    }

    /// Races control against event completion without repolling a consumed join.
    /// Clean event completion ends only this attachment; errors remain visible.
    pub(crate) async fn supervise(
        &mut self,
        control: impl std::future::Future<Output = Result<u64>>,
    ) -> (Result<u64>, bool) {
        let Some(task) = self.task.as_mut() else {
            return (control.await, false);
        };
        tokio::select! {
            biased;
            result = control => (result, false),
            joined = task => {
                self.task.take();
                (event_join_result(joined).map(|_| 0), true)
            }
        }
    }

    /// Joins remaining event work by one deadline, aborting only this worker on
    /// expiry. Nested worker errors and unexpected join failures are not clean.
    pub(crate) async fn settle_until(&mut self, deadline: tokio::time::Instant) -> Result<()> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        let result = match tokio::time::timeout_at(deadline, &mut *task).await {
            Ok(joined) => event_join_result(joined).map(|_| ()),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(MezError::invalid_state(
                    "Iroh event task teardown timed out",
                ))
            }
        };
        self.task.take();
        result
    }
}

impl Drop for IrohEventTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Preserves the initiating failure while reporting secondary event teardown
/// degradation without discarding nested errors or exposing panic payloads.
pub(crate) fn merge_event_result(control: Result<u64>, event: Result<()>) -> Result<u64> {
    match (control, event) {
        (Err(error), Err(_)) => Err(MezError::new(
            error.kind(),
            format!("{error}; Iroh event task teardown also failed"),
        )),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Ok(served), Ok(())) => Ok(served),
    }
}

/// Retains typed worker errors while classifying joins without panic payloads.
fn event_join_result(
    joined: std::result::Result<Result<u64>, tokio::task::JoinError>,
) -> Result<u64> {
    joined.map_err(|error| {
        MezError::invalid_state(if error.is_panic() {
            "Iroh event task panicked"
        } else {
            "Iroh event task was cancelled unexpectedly"
        })
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Cancelling an explicit settlement wait must not detach the JoinHandle.
    /// The retained owner still aborts its exact worker when the connection drops.
    #[tokio::test]
    async fn event_task_cancelled_settlement_retains_drop_ownership() {
        let task = tokio::spawn(std::future::pending::<Result<u64>>());
        let abort = task.abort_handle();
        let mut owner = IrohEventTask::new(Some(task));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                owner.settle_until(tokio::time::Instant::now() + Duration::from_secs(60))
            )
            .await
            .is_err()
        );
        drop(owner);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    /// A required event worker must interrupt live control on typed failure or
    /// panic. Joining twice is inert, and private panic payloads stay excluded.
    #[tokio::test]
    async fn event_task_failure_interrupts_live_control_and_preserves_classification() {
        for panic in [false, true] {
            let task = tokio::spawn(async move {
                if panic {
                    panic!("private event panic payload");
                }
                Err(MezError::invalid_args("render transfer exceeds limit"))
            });
            let mut owner = IrohEventTask::new(Some(task));
            let (result, completed) = tokio::time::timeout(
                Duration::from_secs(5),
                owner.supervise(std::future::pending()),
            )
            .await
            .unwrap();
            assert!(completed);
            let error = result.unwrap_err();
            if panic {
                assert!(error.message().contains("panicked"));
                assert!(!error.message().contains("private"));
            } else {
                assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
            }
            owner
                .settle_until(tokio::time::Instant::now())
                .await
                .unwrap();
        }
    }

    /// Control completion cannot disguise a concurrently failed worker, and a
    /// shutdown deadline aborts only that connection's outstanding event task.
    #[tokio::test(start_paused = true)]
    async fn event_task_teardown_retains_nested_failure_and_bounds_pending_worker() {
        let task = tokio::spawn(async { Err(MezError::invalid_args("event flush failed")) });
        let mut owner = IrohEventTask::new(Some(task));
        let (control, completed) = owner.supervise(async { Ok(3) }).await;
        assert!(!completed);
        let event = owner
            .settle_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert_eq!(
            merge_event_result(control, event).unwrap_err().kind(),
            crate::error::MezErrorKind::InvalidArgs
        );
        let task = tokio::spawn(std::future::pending::<Result<u64>>());
        let abort = task.abort_handle();
        let mut owner = IrohEventTask::new(Some(task));
        let error = owner
            .settle_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.message().contains("teardown timed out"));
        assert!(abort.is_finished());
    }
}
