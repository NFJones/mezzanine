//! Provider worker custody across cancellation of the entire service future.
//!
//! The supervisor retains the actual driver task outside its abortable wrapper.
//! Stop requests are monotonic; aborting the wrapper drops only a borrow of that
//! task's handle. The driver therefore finishes its original cleanup even when
//! a request owner is temporarily local to a join future. If the driver fails,
//! retained per-service state permits observed cleanup of remaining workers.
//! Normal supervision never detaches cleanup or grants effect-replay authority.
//! Last-owner drop requests stop but is not an observed retirement receipt.

use super::worker_ownership::{
    AgentProviderWorkerOwners, ProviderCleanupFailures, finish_provider_service_result,
    retire_agent_provider_workers,
};
use super::{
    AsyncAgentProviderPollReport, AsyncAgentProviderWorkerResult, AsyncRuntimeSessionHandle,
    JoinSet, ProviderObservationDrains,
};
use crate::error::{MezError, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;

/// All mutable driver custody retained outside the abortable service future.
#[derive(Default)]
pub(super) struct ProviderServiceState {
    /// Actual monitor tasks, including their completed but unadopted results.
    pub(super) workers: JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    /// Exact admitted request identities independent of monitor lifetimes.
    pub(super) owners: AgentProviderWorkerOwners,
    /// Producer destruction and reported expense ownership for model requests.
    pub(super) drains: ProviderObservationDrains,
    /// Primary service cause captured before cancellable cleanup begins.
    pub(super) service_failure: Option<MezError>,
    /// Cleanup failures survive cancellation between sibling joins.
    pub(super) cleanup_failures: ProviderCleanupFailures,
}

/// Work state passed into the actual driver without its own task handle. Keeping
/// the handle owner separate prevents a task -> owner -> task reference cycle.
#[derive(Clone)]
pub(in crate::host::async_runtime) struct AgentProviderWorkRetirement {
    /// Monotonic stop request; no deadline, policy or claim is renewed.
    shutdown: Arc<AtomicBool>,
    /// One driver borrow, not a global or actor lock. Child workers run freely.
    pub(super) state: Arc<Mutex<ProviderServiceState>>,
    /// Original actor destination retained until actual worker retirement.
    handle: AsyncRuntimeSessionHandle,
}

impl AgentProviderWorkRetirement {
    /// Creates one standalone or supervisor-retained work owner.
    pub(super) fn new(handle: AsyncRuntimeSessionHandle) -> Self {
        Self {
            shutdown: Arc::default(),
            state: Arc::default(),
            handle,
        }
    }

    /// Signals before wrapper abort; actual joins remain required afterwards.
    fn request_stop(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Reports the supervisor's monotonic request to the normal service loop.
    pub(super) fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Takes over only after the actual driver has joined, preserving every
    /// remaining request identity and cleanup failure without restarting work.
    async fn retire_remaining(&self) -> Result<u64> {
        self.request_stop();
        let mut state = self.state.lock().await;
        let ProviderServiceState {
            workers,
            owners,
            drains,
            cleanup_failures,
            ..
        } = &mut *state;
        retire_agent_provider_workers(workers, &self.handle, drains, owners, cleanup_failures).await
    }
}

/// Sole actual driver handle; None after its result has been reported once.
#[derive(Default)]
struct DriverState {
    /// Prevents replacement after either completion or cancellation.
    started: bool,
    /// Borrowed across join, never moved into an abortable wrapper future.
    handle: Option<tokio::task::JoinHandle<Result<AsyncAgentProviderPollReport>>>,
}

/// External handle owner; the driver receives only separate work custody.
struct ProviderDriver {
    /// Last-owner drop requests stop before a handle can become detached.
    shutdown: Arc<AtomicBool>,
    /// One observation path, shared by wrapper and supervisor takeover.
    task: Mutex<DriverState>,
    /// Genuine task registration witness for bounded lifecycle fixtures.
    #[cfg(test)]
    started: AtomicBool,
}

impl Drop for ProviderDriver {
    /// Requests cooperative cleanup on last external-owner loss. Normal
    /// supervision still observes the driver join; drop alone is no receipt.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

/// Supervisor-owned actual driver plus bounded provider/native work custody.
#[derive(Clone)]
pub(in crate::host::async_runtime) struct AgentProviderServiceRetirement {
    /// Separate work object avoids a self-retaining driver reference cycle.
    pub(super) work: AgentProviderWorkRetirement,
    /// Actual driver task retained outside its abortable service wrapper.
    driver: Arc<ProviderDriver>,
}

impl std::fmt::Debug for AgentProviderServiceRetirement {
    /// Never formats commands, output or arbitrary failure payloads.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentProviderServiceRetirement")
            .finish_non_exhaustive()
    }
}

impl AgentProviderServiceRetirement {
    /// Builds both owners before any service future or driver is spawned.
    pub(in crate::host::async_runtime) fn new(handle: AsyncRuntimeSessionHandle) -> Self {
        let work = AgentProviderWorkRetirement::new(handle);
        let driver = Arc::new(ProviderDriver {
            shutdown: work.shutdown.clone(),
            task: Mutex::new(DriverState::default()),
            #[cfg(test)]
            started: AtomicBool::new(false),
        });
        Self { work, driver }
    }

    /// Supplies only work custody to the driver, never its own join owner.
    pub(in crate::host::async_runtime) fn work(&self) -> AgentProviderWorkRetirement {
        self.work.clone()
    }

    /// Signals the actual driver before the wrapper is aborted.
    pub(in crate::host::async_runtime) fn request_stop(&self) {
        self.work.request_stop();
    }

    /// Registers the actual driver before yielding; its future cannot retain
    /// this handle owner because callers pass only the separate work object.
    pub(in crate::host::async_runtime) async fn start_driver(
        &self,
        future: impl std::future::Future<Output = Result<AsyncAgentProviderPollReport>> + Send + 'static,
    ) -> Result<()> {
        let mut driver = self.driver.task.lock().await;
        if driver.started {
            return Err(MezError::invalid_state(
                "provider service driver already started",
            ));
        }
        driver.started = true;
        driver.handle = Some(tokio::spawn(Box::pin(future)));
        #[cfg(test)]
        self.driver.started.store(true, Ordering::Release);
        Ok(())
    }

    /// Observes the same actual driver while a wrapper can still be cancelled.
    pub(in crate::host::async_runtime) async fn join_driver(
        &self,
    ) -> Result<AsyncAgentProviderPollReport> {
        self.take_driver_result().await.ok_or_else(|| {
            MezError::invalid_state("provider service driver has no unconsumed join owner")
        })?
    }

    /// Returns whether the actual task was registered, not a readiness guess.
    #[cfg(test)]
    pub(super) fn driver_started_for_tests(&self) -> bool {
        self.driver.started.load(Ordering::Acquire)
    }

    /// Keeps the handle in shared custody across the join and consumes its
    /// result only on a ready poll; wrapper cancellation releases just a borrow.
    async fn take_driver_result(&self) -> Option<Result<AsyncAgentProviderPollReport>> {
        let mut driver = self.driver.task.lock().await;
        let handle = driver.handle.as_mut()?;
        let result = handle
            .await
            .map_err(|error| {
                let cause = if error.is_panic() {
                    "panic"
                } else if error.is_cancelled() {
                    "cancelled"
                } else {
                    "unknown"
                };
                MezError::invalid_state(format!(
                    "agent provider service driver task failed: {cause}"
                ))
            })
            .and_then(|result| result);
        driver.handle = None;
        Some(result)
    }

    /// Requests stop and observes the original driver through normal cleanup.
    /// Shared driver failure preserves its primary cause and still retires all
    /// remaining workers before the session may shut down actor ingress.
    pub(in crate::host::async_runtime) async fn retire(&self) -> Result<u64> {
        self.request_stop();
        let driver = self
            .take_driver_result()
            .await
            .map_or(Ok(0), |result| result.map(|report| report.executions));
        let saved = self.work.state.lock().await.service_failure.clone();
        let primary = match saved {
            Some(error) => finish_provider_service_result(Err(error), driver.map(|_| ())),
            None => driver,
        };
        match self.work.retire_remaining().await {
            Ok(completed) => primary.map(|count| count.saturating_add(completed)),
            Err(error) => finish_provider_service_result(primary, Err(error)),
        }
    }
}
