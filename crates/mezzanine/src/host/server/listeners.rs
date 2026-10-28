//! Joint ownership of local management and optional remote host listeners.
//!
//! This owner composes both listeners over the existing session router. Every
//! terminal branch observes peer cleanup before returning; the CLI supplies
//! only its shutdown signal. Keeping the serving futures here makes listener
//! failure scope testable against the actual Unix management owner rather than
//! a second router or a detached background listener.

use std::future::Future;

use crate::error::{MezError, Result};
use crate::host::iroh::HostIrohRuntime;

use super::HostServer;

impl HostServer {
    /// Runs local and remote listeners under one shutdown signal, preserving
    /// the initiating error after both serving futures complete their cleanup.
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
                    async { remote.await.map(|_| ()) },
                    "remote",
                )
                .await
            }
            result = &mut remote => {
                let _ = shutdown.send(true);
                finish_host_listener_pair(result.map(|_| ()), local.as_mut(), "local").await
            }
            () = &mut cancellation => {
                let _ = shutdown.send(true);
                let (local_result, remote_result) = tokio::join!(local, remote);
                finish_host_listener_pair(
                    local_result,
                    async { remote_result.map(|_| ()) },
                    "remote",
                )
                .await
            }
        }
    }
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
        (Err(primary), Err(peer)) => Err(MezError::new(
            primary.kind(),
            format!(
                "{}; {peer_name} listener cleanup also failed: {peer}",
                primary.message()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Remote listener failure remains the reported service error only after
    /// the local Unix listener has completed its shutdown path.
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
}
