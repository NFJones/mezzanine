//! Once-only endpoint close sequencing around the existing service supervisor.
//!
//! Cancellation closes the session endpoint before stopping services. Natural
//! completion and service failure still attempt an unstarted close before actor
//! shutdown. A close interrupted by service failure is reported as unproven and
//! never repeated. The endpoint implementation and timeout remain owned by the
//! existing handle; this adapter grants no process or effect-retry authority.

use super::{
    AsyncRuntimeService, AsyncRuntimeSupervisionReport, Future, MezError, Result,
    supervise_async_runtime_services,
};

/// Returns independent supervision and endpoint-close outcomes so the runtime
/// can preserve its initiating cause while completing actor/native teardown.
/// The optional close closure is consumed at most once. False completion or a
/// dropped in-progress close is an error, while an absent endpoint is success.
pub(super) async fn supervise_and_close_endpoint<C, Close, Closing>(
    services: Vec<AsyncRuntimeService>,
    cancellation: C,
    mut close: Option<Close>,
) -> (Result<AsyncRuntimeSupervisionReport>, Result<()>)
where
    C: Future<Output = ()>,
    Close: FnOnce() -> Closing,
    Closing: Future<Output = bool>,
{
    let mut started = false;
    let mut completed = None;
    let stop = async {
        cancellation.await;
        if let Some(close) = close.take() {
            started = true;
            completed = Some(close().await);
        }
    };
    let supervision = supervise_async_runtime_services(services, stop).await;
    if let Some(close) = close.take() {
        started = true;
        completed = Some(close().await);
    }
    let endpoint = match (started, completed) {
        (false, None) | (true, Some(true)) => Ok(()),
        (_, Some(false)) => Err(MezError::invalid_state(
            "session Iroh endpoint close did not complete within its deadline",
        )),
        _ => Err(MezError::invalid_state(
            "session Iroh endpoint close was interrupted; completion is unproven",
        )),
    };
    (supervision, endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::async_runtime::AsyncRuntimeServiceExit;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// Natural primary completion and explicit cancellation must each consume
    /// the same single endpoint close. An absent endpoint is also valid and
    /// cannot invent shutdown work or keep primary supervision alive.
    #[tokio::test(flavor = "current_thread")]
    async fn session_teardown_endpoint_close_runs_once_on_every_exit() {
        for cancelled in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let services = vec![AsyncRuntimeService::new("control", async move {
                if cancelled {
                    std::future::pending::<()>().await;
                }
                Ok(AsyncRuntimeServiceExit::completed(0))
            })];
            let cancellation = async move {
                if !cancelled {
                    std::future::pending::<()>().await;
                }
            };
            let (supervision, endpoint) = supervise_and_close_endpoint(
                services,
                cancellation,
                Some(move || async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    true
                }),
            )
            .await;
            assert_eq!(supervision.unwrap().shutdown_requested, cancelled);
            endpoint.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        let (supervision, endpoint) = supervise_and_close_endpoint(
            vec![AsyncRuntimeService::new("control", async {
                Ok(AsyncRuntimeServiceExit::completed(0))
            })],
            std::future::pending(),
            None::<fn() -> std::future::Ready<bool>>,
        )
        .await;
        supervision.unwrap();
        endpoint.unwrap();
    }

    /// Endpoint timeout evidence must remain separate from a service failure so
    /// callers retain both. A failed close is attempted once and does not cause
    /// service restart, repeated endpoint effects or a healthy close result.
    #[tokio::test(flavor = "current_thread")]
    async fn session_teardown_endpoint_close_failure_retains_both_outcomes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (supervision, endpoint) = supervise_and_close_endpoint(
            vec![AsyncRuntimeService::new("control", async {
                Err(MezError::forbidden("initiating service failure"))
            })],
            std::future::pending(),
            Some(move || async move {
                observed.fetch_add(1, Ordering::SeqCst);
                false
            }),
        )
        .await;
        assert_eq!(
            supervision.unwrap_err().kind(),
            crate::error::MezErrorKind::Forbidden
        );
        assert!(endpoint.unwrap_err().message().contains("deadline"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A real service failure may win while cancellation awaits endpoint close.
    /// The consumed close future then has unknown completion; it must be reported
    /// rather than silently accepted or invoked again. Channels establish this
    /// interleaving without a native endpoint, sleeps or elapsed-time inference.
    #[tokio::test(flavor = "current_thread")]
    async fn session_teardown_endpoint_interrupted_close_is_unproven_without_retry() {
        let (close_started, observed) = tokio::sync::oneshot::channel();
        let (fail, service_failure) = tokio::sync::oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let services = vec![AsyncRuntimeService::new("control", async move {
            service_failure.await.unwrap();
            Err(MezError::forbidden("service failed during endpoint close"))
        })];
        let closing = supervise_and_close_endpoint(
            services,
            std::future::ready(()),
            Some(move || async move {
                count.fetch_add(1, Ordering::SeqCst);
                close_started.send(()).unwrap();
                std::future::pending::<bool>().await
            }),
        );
        let controller = async {
            observed.await.unwrap();
            fail.send(()).unwrap();
        };
        let ((supervision, endpoint), ()) = tokio::join!(closing, controller);
        assert_eq!(
            supervision.unwrap_err().kind(),
            crate::error::MezErrorKind::Forbidden
        );
        assert!(
            endpoint
                .unwrap_err()
                .message()
                .contains("completion is unproven")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
