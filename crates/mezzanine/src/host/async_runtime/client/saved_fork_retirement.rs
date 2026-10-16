//! Exact saved-fork worker retirement across auxiliary service cancellation.
//!
//! The command service inserts bounded fork wrappers here before starting them;
//! its supervisor retains the same owner outside the abortable service future.
//! Only synchronous insertion/inspection/take holds the mutex. Retirement takes
//! the set after the service stops, then observes every wrapper without a lock.
//! This owner grants no storage cleanup authority and never aborts a publication
//! worker: cancellation is monotonic and existing job deadlines remain intact.

use super::{JoinSet, MezError, Result};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Supervisor-retained ownership of the command lane's exact fork workers.
#[derive(Clone, Debug, Default)]
pub(in crate::host::async_runtime) struct SavedForkServiceRetirement {
    /// Independent service stop signal; no deadline or lease is renewed.
    pub(super) shutdown: Arc<AtomicBool>,
    /// Shares the command lane's existing total capacity, not a second lane.
    workers: Arc<Mutex<JoinSet<Result<()>>>>,
    /// Observed causes retained across cancellation of service cleanup.
    failures: Arc<Mutex<SavedForkFailures>>,
    /// Owned-fixture pause after durable publication, before actor adoption.
    #[cfg(test)]
    publication_gate: Arc<Mutex<Option<ForkPublicationGate>>>,
    /// Owned-fixture evidence that normal service retirement has begun.
    #[cfg(test)]
    draining: Arc<AtomicBool>,
    /// Exact supervisor takeover acknowledgment, independent of scheduling delay.
    #[cfg(test)]
    takeover: Arc<tokio::sync::Notify>,
}

/// Bounded service and worker causes; neither grants cleanup authority.
#[derive(Debug, Default)]
struct SavedForkFailures {
    /// Primary service cause captured before its first cleanup await.
    service: Option<(crate::error::MezErrorKind, String)>,
    /// First removed worker failure retained independently of its wrapper.
    worker: Option<(crate::error::MezErrorKind, String)>,
}

/// Two-phase test handshake; production workers have no pause or new deadline.
#[cfg(test)]
pub(in crate::host::async_runtime) type ForkPublicationGate =
    (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);

impl SavedForkServiceRetirement {
    /// Inserts a failed sibling in an owned two-slot command-lane fixture.
    #[cfg(test)]
    pub(in crate::host::async_runtime) fn fail_worker_for_tests(&self) {
        self.spawn(async { Err(MezError::invalid_state("owned consumed worker failure")) })
            .unwrap();
    }

    /// Awaits actual supervisor takeover rather than a fixed scheduling delay.
    #[cfg(test)]
    pub(in crate::host::async_runtime) async fn wait_for_takeover_for_tests(&self) {
        self.takeover.notified().await;
    }

    /// Installs an exact worker pause for postpublication cancellation controls.
    #[cfg(test)]
    pub(in crate::host::async_runtime) fn gate_publication_for_tests(&self) -> ForkPublicationGate {
        let gate = (Arc::default(), Arc::default());
        *self.publication_gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    /// Moves the configured gate into its sole blocking publication owner.
    #[cfg(test)]
    pub(super) fn take_publication_gate_for_tests(&self) -> Option<ForkPublicationGate> {
        self.publication_gate.lock().unwrap().take()
    }

    /// Reports entry into normal service cleanup before auxiliary abort.
    #[cfg(test)]
    pub(in crate::host::async_runtime) fn draining_for_tests(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    /// Exposes only retained task count to owned lifecycle regression fixtures.
    #[cfg(test)]
    pub(in crate::host::async_runtime) fn retained_workers_for_tests(&self) -> usize {
        self.workers.lock().unwrap().len()
    }

    /// Requests cancellation before service wrapper abort or retirement.
    pub(in crate::host::async_runtime) fn request_stop(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Retains the service cause synchronously before cleanup can be cancelled.
    pub(super) fn remember_service_failure(&self, error: &MezError) -> Result<()> {
        self.failures
            .lock()
            .map_err(|_| Self::poisoned())?
            .service
            .get_or_insert_with(|| (error.kind(), error.message().to_owned()));
        Ok(())
    }

    /// Retains a removed worker failure before any subsequent await or abort.
    fn remember_worker_failure(&self, error: &MezError) -> Result<()> {
        self.failures
            .lock()
            .map_err(|_| Self::poisoned())?
            .worker
            .get_or_insert_with(|| (error.kind(), error.message().to_owned()));
        Ok(())
    }

    /// Reconstructs distinct service and worker causes without consuming evidence.
    fn observed_failure(&self) -> Result<Option<MezError>> {
        let failures = self.failures.lock().map_err(|_| Self::poisoned())?;
        Ok(match (&failures.service, &failures.worker) {
            (Some((kind, service)), Some((_, worker))) if service != worker => Some(MezError::new(
                *kind,
                format!("{service}; saved fork worker retirement failed: {worker}"),
            )),
            (Some((kind, message)), _) | (_, Some((kind, message))) => {
                Some(MezError::new(*kind, message.clone()))
            }
            (None, None) => None,
        })
    }

    /// Reports worker slots still occupied, including completed unobserved jobs.
    pub(super) fn len(&self) -> Result<usize> {
        Ok(self.workers.lock().map_err(|_| Self::poisoned())?.len())
    }

    /// Inserts before starting the job; an abort cannot strand an unregistered task.
    pub(super) fn spawn(
        &self,
        future: impl Future<Output = Result<()>> + Send + 'static,
    ) -> Result<()> {
        self.workers
            .lock()
            .map_err(|_| Self::poisoned())?
            .spawn(future);
        Ok(())
    }

    /// Observes completed jobs without parking the command service or taking I/O.
    pub(super) fn drain_completed(&self) -> Result<u64> {
        let mut workers = self.workers.lock().map_err(|_| Self::poisoned())?;
        let mut completed = 0u64;
        while let Some(joined) = workers.try_join_next() {
            let result = joined
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "saved fork command worker task failed: {error}"
                    ))
                })
                .and_then(|result| result);
            if let Err(error) = result {
                self.remember_worker_failure(&error)?;
                return Err(error);
            }
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    /// Observes retirement while the service can still be aborted. No worker set
    /// is moved into this future: supervisor ownership survives its cancellation.
    pub(super) async fn retire_in_service(&self) -> Result<u64> {
        self.request_stop();
        #[cfg(test)]
        self.draining.store(true, Ordering::Release);
        let mut completed = 0u64;
        while self.len()? != 0 {
            match self.drain_completed() {
                Ok(count) => completed = completed.saturating_add(count),
                Err(_) => {}
            }
            if self.len()? != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        match self.observed_failure()? {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }

    /// Drains every exact worker after the service is joined, preserving the first
    /// failure but never dropping remaining workers because a sibling failed.
    pub(in crate::host::async_runtime) async fn retire(&self) -> Result<u64> {
        self.request_stop();
        let mut workers = std::mem::take(&mut *self.workers.lock().map_err(|_| Self::poisoned())?);
        #[cfg(test)]
        self.takeover.notify_one();
        let mut completed = 0u64;
        while let Some(joined) = workers.join_next().await {
            completed = completed.saturating_add(1);
            let result = joined
                .map_err(|error| {
                    MezError::invalid_state(format!("saved fork retirement task failed: {error}"))
                })
                .and_then(|result| result);
            if let Err(error) = result {
                self.remember_worker_failure(&error)?;
            }
        }
        match self.observed_failure()? {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }

    /// Reports uncertain ownership rather than claiming successful cleanup.
    fn poisoned() -> MezError {
        MezError::invalid_state("saved fork retirement registry poisoned; ownership unproven")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed or panicking sibling must not drop another job's blocking owner.
    /// Retirement observes the actual producer's destruction while cancellation
    /// releases its held acquisition; the first failure remains an error.
    #[tokio::test(flavor = "current_thread")]
    async fn saved_fork_retirement_observes_blocking_sibling_after_error_or_panic() {
        for panic in [false, true] {
            let owner = SavedForkServiceRetirement::default();
            let started = Arc::new(tokio::sync::Notify::new());
            let retired = Arc::new(AtomicBool::new(false));
            let signal = owner.shutdown.clone();
            let worker_started = started.clone();
            let worker_retired = retired.clone();
            owner
                .spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        worker_started.notify_one();
                        while !signal.load(Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        worker_retired.store(true, Ordering::Release);
                    })
                    .await
                    .map_err(|error| MezError::invalid_state(error.to_string()))?;
                    Ok(())
                })
                .unwrap();
            owner
                .spawn(async move {
                    assert!(!panic, "owned sibling panic");
                    Err(MezError::invalid_state("owned sibling error"))
                })
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
                .await
                .unwrap();
            assert!(!retired.load(Ordering::Acquire));
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), owner.retire())
                .await
                .unwrap();
            assert!(result.is_err());
            assert!(retired.load(Ordering::Acquire));
            assert_eq!(owner.retained_workers_for_tests(), 0);
        }
    }
}
