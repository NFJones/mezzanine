//! Zsh fixture teardown ordering and process-free admission regressions.
//!
//! The fixture owns the original supervisor result separately from its
//! completion notification. Keep both completion and Shutdown outcomes intact;
//! neither notification nor these inert controls proves native retirement or
//! identifies the producer of a historical Zsh failure.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stops only the original supervisor, then awaits its original completion
/// before Shutdown so already-owned output and accounting retain a Running
/// actor. The dedicated Notify retains a permit even before registration and
/// is never consumed by a worker. Both outcomes transfer without normalization;
/// NativeFixture separately retains the actual supervisor report/error before
/// sending completion. Neither an error nor a notification is native evidence.
pub(super) async fn settle_zsh_supervisor<C, S>(
    done: &AtomicBool,
    wake: &tokio::sync::Notify,
    completion: impl Future<Output = C>,
    shutdown: impl Future<Output = S>,
) -> (C, S) {
    done.store(true, Ordering::SeqCst);
    wake.notify_one();
    let completion = completion.await;
    (completion, shutdown.await)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use futures_util::FutureExt;

    use super::super::*;
    use super::settle_zsh_supervisor;
    use crate::host::async_runtime::pane_io::{
        AsyncPaneIoFuture, AsyncPaneProcessIo, wait_for_fixture_supervisor_event_or_stop,
    };

    /// Gates only the first fake read, after the real worker's Running check.
    /// All output construction, batching and actor submission remain real.
    struct GatedOutput {
        backend: AsyncFakePaneProcessIo,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    impl AsyncPaneProcessIo for GatedOutput {
        /// Retains the same read future until the controller releases it.
        fn read_output(&mut self, max_bytes: usize) -> AsyncPaneIoFuture<'_, Option<Vec<u8>>> {
            Box::pin(async move {
                if let Some(release) = self.release.take() {
                    release
                        .await
                        .map_err(|_| MezError::invalid_state("fake read release lost"))?;
                }
                self.backend.read_output(max_bytes).await
            })
        }

        /// Forwards real driver writes to the existing fake backend.
        fn write_input<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncPaneIoFuture<'a, usize> {
            self.backend.write_input(bytes)
        }

        /// Forwards resize operations unchanged.
        fn resize(&mut self, size: Size) -> AsyncPaneIoFuture<'_, ()> {
            self.backend.resize(size)
        }

        /// Forwards termination; this fake has no native process or PID.
        fn terminate(&mut self, force: bool) -> AsyncPaneIoFuture<'_, ProcessEvent> {
            self.backend.terminate(force)
        }
    }

    /// The actual extracted Zsh coordinator must leave the real actor Running
    /// until a previously started output read is submitted by the real worker.
    /// Shutdown-first rejects that exact output; no parallel reducer model,
    /// process, synthetic generation, deadline, or native receipt is involved.
    /// Every gated future is released and the actor closed before assertions.
    #[tokio::test(flavor = "current_thread")]
    async fn zsh_teardown_drains_owned_output_before_shutdown() {
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
            .build()
            .unwrap();
        let done = AtomicBool::new(false);
        let wake = tokio::sync::Notify::new();
        let shutdown_polled = Cell::new(false);
        let (release, read) = tokio::sync::oneshot::channel();
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_output(b"owned-before-shutdown\n".to_vec());
        let mut driver = AsyncPaneProcessDriver::new(
            "%1",
            GatedOutput {
                backend,
                release: Some(read),
            },
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let client = async {
            let mut worker = Box::pin(run_async_pane_process_service(
                &handle,
                &mut driver,
                AsyncPaneProcessServiceConfig {
                    max_polls: 1,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
                |_, _| false,
            ));
            let blocked_read = worker.as_mut().now_or_never().is_none();
            let original_completion = async {
                // Exercise the actual supervisor idle wait seam. Its stop
                // wake does not cancel the already-started worker read.
                let _ = wait_for_fixture_supervisor_event_or_stop::<()>(
                    Some(&wake),
                    std::future::pending(),
                )
                .await;
                worker.await
            };
            let mut coordinator = Box::pin(settle_zsh_supervisor(
                &done,
                &wake,
                original_completion,
                async {
                    shutdown_polled.set(true);
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
                        reason: "process-free Zsh coordinator ordering".into(),
                        force: false,
                        failed: false,
                    }));
                    handle.submit_runtime_events(batch).await
                },
            ));
            let early = coordinator.as_mut().now_or_never();
            let early_shutdown = shutdown_polled.get();
            // FIFO actor query observes any Shutdown already submitted by the
            // real helper before releasing the original in-progress read.
            let state_before_release = handle.lifecycle_state().await;
            let released = release.send(()).is_ok();
            let outcomes = match early {
                Some(outcomes) => outcomes,
                None => coordinator.await,
            };
            let state_after_completion = handle.lifecycle_state().await;
            let close = handle.shutdown().await;
            (
                blocked_read,
                early_shutdown,
                state_before_release,
                released,
                outcomes,
                state_after_completion,
                close,
            )
        };
        let ((blocked, early_shutdown, before, released, (worker, shutdown), after, close), _exit) =
            tokio::join!(client, actor.run());
        eprintln!(
            "process-free owned Output: early_shutdown={early_shutdown} before={before:?} worker_ok={} worker_error={:?} after={after:?}",
            worker.is_ok(),
            worker
                .as_ref()
                .err()
                .map(|error| (error.kind(), error.message()))
        );
        assert!(blocked && released);
        assert!(done.load(Ordering::SeqCst));
        assert!(
            !early_shutdown,
            "Shutdown was polled before original worker completion"
        );
        assert_eq!(before.unwrap(), RuntimeLifecycleState::Running);
        let report = worker.expect("owned output must be admitted before Shutdown");
        assert_eq!(report.output_events, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Running);
        let admission = shutdown.unwrap();
        assert_eq!(admission.accepted, 1);
        assert_eq!(admission.applied, 1);
        assert_eq!(after.unwrap(), RuntimeLifecycleState::Stopping);
        assert_eq!(close.unwrap(), RuntimeLifecycleState::Stopping);
    }

    /// The actual Zsh helper must wake the original supervisor both before
    /// registration and when a worker has consumed the shared event wake. The
    /// actual wait seam is exercised, with a controlled event fallback to
    /// release all futures before asserting rather than relying on a timeout.
    #[tokio::test(flavor = "current_thread")]
    async fn zsh_teardown_dedicated_stop_is_durable_and_not_shared() {
        for registered in [false, true] {
            let done = AtomicBool::new(false);
            let wake = tokio::sync::Notify::new();
            let shared = tokio::sync::Notify::new();
            let used_dedicated = Cell::new(false);
            let (release, fallback) = tokio::sync::oneshot::channel();
            let mut competitor = Box::pin(shared.notified());
            let competitor_registered = competitor.as_mut().now_or_never().is_none();
            let mut supervisor = Box::pin(wait_for_fixture_supervisor_event_or_stop(
                Some(&wake),
                async {
                    tokio::select! {
                        _ = shared.notified() => Some(()),
                        _ = fallback => Some(()),
                    }
                },
            ));
            let pending_before_stop = !registered || supervisor.as_mut().now_or_never().is_none();
            shared.notify_one();
            let worker_consumed = competitor.as_mut().now_or_never().is_some();
            let mut coordinator = Box::pin(settle_zsh_supervisor(
                &done,
                &wake,
                async {
                    used_dedicated.set(supervisor.await.is_none());
                },
                async {},
            ));
            let early = coordinator.as_mut().now_or_never();
            let completed_without_event = early.is_some();
            let _ = release.send(());
            if early.is_none() {
                coordinator.await;
            }
            assert!(competitor_registered && pending_before_stop && worker_consumed);
            assert!(done.load(Ordering::SeqCst));
            assert!(completed_without_event);
            assert!(used_dedicated.get());
        }
    }

    /// Non-Clone error whose allocation and final release can be checked.
    struct OwnedError(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for OwnedError {
        /// Records only owned release; this is not a native cleanup oracle.
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The original completion notification and Shutdown failure remain
    /// separately owned outcomes. Neither failure masks or clones the other,
    /// including when both fail; NativeFixture still owns the separate actual
    /// supervisor report/error. Completion always precedes Shutdown polling.
    #[tokio::test(flavor = "current_thread")]
    async fn zsh_teardown_preserves_both_owned_errors_and_order() {
        for (completion_fails, shutdown_fails) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let done = AtomicBool::new(false);
            let wake = tokio::sync::Notify::new();
            let completed = Cell::new(false);
            let shutdown_after_completion = Cell::new(false);
            let completion_drops = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
            let shutdown_drops = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
            let completion_error = Box::new(OwnedError(completion_drops.clone()));
            let shutdown_error = Box::new(OwnedError(shutdown_drops.clone()));
            let original_completion = completion_error.as_ref() as *const OwnedError;
            let original_shutdown = shutdown_error.as_ref() as *const OwnedError;
            let (completion, shutdown) = settle_zsh_supervisor(
                &done,
                &wake,
                async {
                    completed.set(true);
                    if completion_fails {
                        Err(completion_error)
                    } else {
                        Ok(completion_error)
                    }
                },
                async {
                    shutdown_after_completion.set(completed.get());
                    if shutdown_fails {
                        Err(shutdown_error)
                    } else {
                        Ok(shutdown_error)
                    }
                },
            )
            .await;
            assert!(done.load(Ordering::SeqCst) && shutdown_after_completion.get());
            assert_eq!(completion.is_err(), completion_fails);
            assert_eq!(shutdown.is_err(), shutdown_fails);
            let completion = match completion {
                Ok(value) | Err(value) => value,
            };
            let shutdown = match shutdown {
                Ok(value) | Err(value) => value,
            };
            assert_eq!(
                completion.as_ref() as *const OwnedError,
                original_completion
            );
            assert_eq!(shutdown.as_ref() as *const OwnedError, original_shutdown);
            assert_eq!(completion_drops.load(Ordering::SeqCst), 0);
            assert_eq!(shutdown_drops.load(Ordering::SeqCst), 0);
            drop((completion, shutdown));
            assert_eq!(completion_drops.load(Ordering::SeqCst), 1);
            assert_eq!(shutdown_drops.load(Ordering::SeqCst), 1);
        }
    }
}
