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
}

impl SavedForkServiceRetirement {
    /// Exposes only retained task count to owned lifecycle regression fixtures.
    #[cfg(test)]
    pub(in crate::host::async_runtime) fn retained_workers_for_tests(&self) -> usize {
        self.workers.lock().unwrap().len()
    }

    /// Requests cancellation before service wrapper abort or retirement.
    pub(in crate::host::async_runtime) fn request_stop(&self) {
        self.shutdown.store(true, Ordering::Release);
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
            joined.map_err(|error| {
                MezError::invalid_state(format!("saved fork command worker task failed: {error}"))
            })??;
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    /// Observes retirement while the service can still be aborted. No worker set
    /// is moved into this future: supervisor ownership survives its cancellation.
    pub(super) async fn retire_in_service(&self) -> Result<u64> {
        self.request_stop();
        let mut completed = 0u64;
        let mut failure = None;
        while self.len()? != 0 {
            match self.drain_completed() {
                Ok(count) => completed = completed.saturating_add(count),
                Err(error) if failure.is_none() => failure = Some(error),
                Err(_) => {}
            }
            if self.len()? != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }

    /// Drains every exact worker after the service is joined, preserving the first
    /// failure but never dropping remaining workers because a sibling failed.
    pub(in crate::host::async_runtime) async fn retire(&self) -> Result<u64> {
        self.request_stop();
        let mut workers = std::mem::take(&mut *self.workers.lock().map_err(|_| Self::poisoned())?);
        let mut completed = 0u64;
        let mut failure = None;
        while let Some(joined) = workers.join_next().await {
            completed = completed.saturating_add(1);
            let result = joined
                .map_err(|error| {
                    MezError::invalid_state(format!("saved fork retirement task failed: {error}"))
                })
                .and_then(|result| result);
            if failure.is_none() {
                failure = result.err();
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }

    /// Reports uncertain ownership rather than claiming successful cleanup.
    fn poisoned() -> MezError {
        MezError::invalid_state("saved fork retirement registry poisoned; ownership unproven")
    }
}
