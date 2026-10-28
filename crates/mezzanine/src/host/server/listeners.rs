//! Joint ownership of local management and optional remote host listeners.
//!
//! This owner composes both listeners over the existing session router. Isolated
//! remote loss retains the original local future after remote cleanup; unsafe
//! cleanup, local failure and explicit shutdown still settle both listeners.
//! Degradation is sticky for final diagnostics and never triggers a restart,
//! replacement router, key owner or input replay. The CLI supplies its signal.

use std::future::Future;

use crate::error::{MezError, Result, TeardownErrors};
use crate::host::iroh::{HostIrohRuntime, HostIrohServeError};

use super::HostServer;

impl HostServer {
    /// Runs both listeners, retaining local service after proven transport-only
    /// loss. Returns retained transport or critical failure after safe cleanup;
    /// no remote failure is converted into a healthy whole-host result.
    pub(crate) async fn serve_with_iroh<C>(
        &self,
        iroh: &HostIrohRuntime,
        cancellation: C,
    ) -> Result<()>
    where
        C: Future<Output = ()>,
    {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let mut local_shutdown = shutdown.subscribe();
        let mut remote_shutdown = shutdown.subscribe();
        let local = self.serve(async move {
            if *local_shutdown.borrow() {
                return;
            }
            let _ = local_shutdown.changed().await;
        });
        let remote = iroh.serve_routed(self.router(), async move {
            if *remote_shutdown.borrow() {
                return;
            }
            let _ = remote_shutdown.changed().await;
        });
        tokio::pin!(cancellation);
        tokio::pin!(local);
        tokio::pin!(remote);
        tokio::select! {
            result = &mut local => {
                let _ = shutdown.send(true);
                finish_host_listener_pair(
                    result,
                    async { remote.await.map(|_| ()).map_err(HostIrohServeError::into_error) },
                    "remote",
                )
                .await
            }
            result = &mut remote => {
                match result {
                    Err(HostIrohServeError::TransportUnavailable(error)) => {
                        eprintln!("mez host: Iroh transport unavailable; local service remains active");
                        // The remote future is complete and must never be polled
                        // again. Its owner and identity remain alive in the caller.
                        let result = tokio::select! {
                            result = &mut local => result,
                            () = &mut cancellation => {
                                let _ = shutdown.send(true);
                                local.await
                            }
                        };
                        finish_degraded_host(result, error)
                    }
                    other => {
                        let _ = shutdown.send(true);
                        finish_host_listener_pair(
                            other.map(|_| ()).map_err(HostIrohServeError::into_error),
                            local.as_mut(), "local",
                        ).await
                    }
                }
            }
            () = &mut cancellation => {
                let _ = shutdown.send(true);
                let (local_result, remote_result) = tokio::join!(local, remote);
                finish_host_listener_pair(
                    local_result,
                    async { remote_result.map(|_| ()).map_err(HostIrohServeError::into_error) },
                    "remote",
                )
                .await
            }
        }
    }
}

/// A later local failure is critical and remains primary. A clean local stop
/// still reports the earlier transport outage in the eventual host result.
fn finish_degraded_host(local: Result<()>, remote: MezError) -> Result<()> {
    let mut failures = TeardownErrors::default();
    if let Err(error) = local {
        failures.record("local host listener", error);
    }
    failures.record("remote transport unavailable", remote);
    failures.finish()
}

/// Drains the peer host listener after one listener completes, preserving the
/// initiating result while deterministically attaching a peer cleanup failure.
async fn finish_host_listener_pair<L>(
    primary_result: Result<()>,
    peer: L,
    peer_name: &str,
) -> Result<()>
where
    L: std::future::Future<Output = Result<()>>,
{
    let peer_result = peer.await;
    match (primary_result, peer_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(peer)) => Err(peer),
        (Err(primary), Err(peer)) => {
            let mut failures = TeardownErrors::default();
            failures.record("initiating host listener", primary);
            failures.record(&format!("{peer_name} listener cleanup"), peer);
            failures.finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Critical remote listener failure remains the reported service error
    /// only after the local Unix listener has completed its shutdown path.
    /// This result-combination helper does not classify optional transport loss.
    #[tokio::test(flavor = "current_thread")]
    async fn remote_host_failure_drains_local_listener_before_returning() {
        let drained = Arc::new(AtomicBool::new(false));
        let local_drained = drained.clone();
        let local = async move {
            local_drained.store(true, Ordering::Release);
            Ok(())
        };
        let remote: Result<u64> = Err(MezError::invalid_state(
            "persistent host Iroh listener closed unexpectedly",
        ));

        let error = finish_host_listener_pair(remote.map(|_| ()), local, "local")
            .await
            .unwrap_err();

        assert!(drained.load(Ordering::Acquire));
        assert!(
            error
                .message()
                .contains("persistent host Iroh listener closed unexpectedly"),
            "{error:?}"
        );
    }

    /// Local listener failure must await remote Iroh cleanup before returning
    /// the initiating local error.
    #[tokio::test(flavor = "current_thread")]
    async fn local_host_failure_drains_remote_listener_before_returning() {
        let drained = Arc::new(AtomicBool::new(false));
        let remote_drained = drained.clone();
        let remote = async move {
            remote_drained.store(true, Ordering::Release);
            Ok(())
        };
        let local = Err(MezError::invalid_state("local host listener failed"));

        let error = finish_host_listener_pair(local, remote, "remote")
            .await
            .unwrap_err();

        assert!(drained.load(Ordering::Acquire));
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        assert!(error.message().contains("local host listener failed"));
    }

    /// When both listeners fail, supervision preserves the initiating error
    /// kind and deterministically attaches the peer cleanup failure.
    #[tokio::test(flavor = "current_thread")]
    async fn host_listener_supervision_retains_dual_failures() {
        let primary = Err(MezError::conflict("local primary failure"));
        let remote = async { Err(MezError::invalid_state("remote cleanup failure")) };

        let error = finish_host_listener_pair(primary, remote, "remote")
            .await
            .unwrap_err();

        assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
        assert!(error.message().contains("local primary failure"));
        assert!(
            error.message().contains(
                "remote listener cleanup also failed: InvalidState: remote cleanup failure"
            ),
            "{error:?}"
        );
    }

    /// Normal listener shutdown remains successful after both listener futures
    /// complete their cleanup paths.
    #[tokio::test(flavor = "current_thread")]
    async fn host_listener_supervision_accepts_clean_shutdown() {
        finish_host_listener_pair(Ok(()), async { Ok(()) }, "remote")
            .await
            .unwrap();
    }

    /// A later critical local result must preserve its original typed policy
    /// observations even after remote service already degraded. A clean local
    /// stop still leaves that earlier outage visible in the final host result.
    #[test]
    fn degraded_host_retains_outage_and_later_critical_local_evidence() {
        let local = MezError::forbidden("critical local authority failure")
            .mark_pane_backend_failure()
            .with_provider_raw_text("local-origin");
        let error =
            finish_degraded_host(Err(local), MezError::invalid_state("remote lost")).unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
        assert!(error.is_pane_backend_failure());
        assert_eq!(error.provider_raw_text(), Some("local-origin"));
        assert!(error.message().contains("remote lost"));
        let error = finish_degraded_host(Ok(()), MezError::conflict("remote lost")).unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
        assert!(error.message().contains("remote lost"));
    }

    /// Peer cleanup diagnostics must not reconstruct the initiating error and
    /// discard its I/O classification or native/provider policy evidence.
    #[tokio::test(flavor = "current_thread")]
    async fn dual_listener_failure_preserves_typed_primary_evidence() {
        let primary = MezError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .mark_pane_backend_failure();
        let error = finish_host_listener_pair(
            Err(primary),
            async { Err(MezError::invalid_state("peer failed")) },
            "remote",
        )
        .await
        .unwrap_err();
        assert_eq!(error.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
        assert!(error.is_pane_backend_failure());
        assert!(error.message().contains("peer failed"));
    }
}
