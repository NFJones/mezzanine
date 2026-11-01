//! Actual ordered-writer custody across service-wrapper and cleanup cancellation.
//!
//! The supervisor binds typed dependencies before spawning any service wrapper.
//! Its own dependency spans all retained producer retirement; provider work also
//! retains a same-actor dependency independently of the outer supervisor future.
//! The actual writer captures only work control, never its handle owner. Joins
//! borrow the original handle and cache its result, so lost observers neither
//! restart writes nor detach a handle during ordinary supervised retirement.
//! Last-owner drop requests stop only; it is not an observed drain/join receipt.

use super::side_effects::{
    AsyncPersistenceSideEffectServiceReport, AsyncRuntimeSideEffectServiceConfig,
    run_async_persistence_side_effect_service_with_retirement,
};
use super::{
    AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, MezError, Result,
    is_terminal_runtime_lifecycle_state,
};
use futures_util::FutureExt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Writer inputs/control, deliberately separate from the actual handle owner.
struct PersistenceWork {
    /// Original actor, never a same-name replacement or another session writer.
    handle: AsyncRuntimeSessionHandle,
    /// Original normal-service drain budget; cleanup does not renew action budgets.
    config: AsyncRuntimeSideEffectServiceConfig,
    /// Stop permits the final drain only after all admitted publishers retire.
    stop: AtomicBool,
    /// Exact live dependency leases, including the supervisor retirement barrier.
    dependents: AtomicUsize,
    /// Normal poll/lifecycle completion can release the wrapper during cleanup.
    normal_complete: tokio::sync::watch::Sender<bool>,
    /// First actual driver failure, observed before dependent retirement waits.
    failure: Mutex<Option<MezError>>,
}

/// Actual running handle or its observed historical result.
enum PersistenceDriverState {
    /// No writer task exists; supervisor starts it before wrappers can be aborted.
    Unstarted,
    /// Never moved out of shared custody across an await.
    Running(tokio::task::JoinHandle<Result<AsyncPersistenceSideEffectServiceReport>>),
    /// Cached actual join, including failure, independently of wrapper observation.
    Joined(Result<AsyncPersistenceSideEffectServiceReport>),
}

/// External handle owner; its actual task captures only separate work control.
struct PersistenceDriver {
    /// Short synchronous start gate; never held across a join or actor operation.
    started: Mutex<bool>,
    /// Actual task and result remain available after cancelled join observers.
    state: tokio::sync::Mutex<PersistenceDriverState>,
    /// Original writer and dependency control shared with its task.
    work: Arc<PersistenceWork>,
    /// Actual task cancellation control independent of a borrowed join handle.
    #[cfg(all(test, target_os = "linux"))]
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}
impl Drop for PersistenceDriver {
    /// Last-owner loss requests a final drain; dropping a handle proves no join.
    fn drop(&mut self) {
        self.work.stop.store(true, Ordering::Release);
    }
}

/// Shared actual persistence driver, retained outside its abortable wrapper.
#[derive(Clone)]
pub(super) struct PersistenceServiceRetirement(Arc<PersistenceDriver>);
impl std::fmt::Debug for PersistenceServiceRetirement {
    /// Never prints actor identity, paths, source bytes or allocation addresses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersistenceServiceRetirement")
    }
}

/// One non-cloneable publisher lifetime; grants neither write nor replay rights.
pub(super) struct PersistenceDependency {
    /// Keeps both the actual handle and original actor writer in root custody.
    writer: PersistenceServiceRetirement,
}
impl std::fmt::Debug for PersistenceDependency {
    /// Dependency debug output carries no source or actor authority data.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersistenceDependency")
    }
}
impl PersistenceDependency {
    /// Requests drain-only operation before waiting on this publisher's joins.
    pub(super) fn request_stop(&self) {
        self.writer.request_stop();
    }

    /// Reads the original writer's observed failure without asserting storage
    /// success or invalidating an already completed historical transfer.
    pub(super) fn failure(&self) -> Result<Option<MezError>> {
        self.writer.observe_completed_nonblocking()?;
        self.writer
            .0
            .work
            .failure
            .lock()
            .map(|failure| failure.clone())
            .map_err(|_| MezError::invalid_state("persistence failure custody lock poisoned"))
    }
}

impl Drop for PersistenceDependency {
    /// Exactly one decrement matches this lease's checked increment.
    fn drop(&mut self) {
        self.writer.0.work.dependents.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PersistenceServiceRetirement {
    /// Captures a validated builder's original writer without spawning work.
    pub(super) fn new(
        handle: AsyncRuntimeSessionHandle,
        config: AsyncRuntimeSideEffectServiceConfig,
    ) -> Self {
        let (normal_complete, _) = tokio::sync::watch::channel(false);
        Self(Arc::new(PersistenceDriver {
            started: Mutex::new(false),
            state: tokio::sync::Mutex::new(PersistenceDriverState::Unstarted),
            #[cfg(all(test, target_os = "linux"))]
            abort: Mutex::new(None),
            work: Arc::new(PersistenceWork {
                handle,
                config,
                stop: AtomicBool::new(false),
                dependents: AtomicUsize::new(0),
                normal_complete,
                failure: Mutex::new(None),
            }),
        }))
    }

    /// Compares actual admission allocation, never session labels or store paths.
    pub(super) fn same_actor(&self, handle: &AsyncRuntimeSessionHandle) -> bool {
        Arc::ptr_eq(&self.0.work.handle.sender.ingress, &handle.sender.ingress)
    }

    /// Tests exact writer actor identity before sole-writer registration.
    pub(super) fn same_writer_actor(&self, other: &Self) -> bool {
        self.same_actor(&other.0.work.handle)
    }

    /// Registers one bounded lifetime before starting any supervised wrapper.
    pub(super) fn dependency(&self) -> Result<PersistenceDependency> {
        let started = self
            .0
            .started
            .lock()
            .map_err(|_| MezError::invalid_state("persistence driver start lock poisoned"))?;
        if *started || self.0.work.stop.load(Ordering::Acquire) {
            return Err(MezError::invalid_state(
                "persistence dependencies must register before writer start",
            ));
        }
        self.0
            .work
            .dependents
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| MezError::invalid_state("persistence dependency count overflow"))?;
        Ok(PersistenceDependency {
            writer: self.clone(),
        })
    }

    /// Starts the sole actual writer synchronously, including pre-poll shutdown.
    /// Join paths always call this gate before locking the asynchronous state.
    pub(super) fn start(&self) -> Result<()> {
        let mut started = self
            .0
            .started
            .lock()
            .map_err(|_| MezError::invalid_state("persistence driver start lock poisoned"))?;
        if *started {
            return Ok(());
        }
        let mut state = self.0.state.try_lock().map_err(|_| {
            MezError::invalid_state("unstarted persistence driver unexpectedly borrowed")
        })?;
        let work = self.0.work.clone();
        let worker = tokio::spawn(async move {
            // Normal budget completion still releases the wrapper. This SAME
            // writer continues final draining while registered publishers can
            // create required records, without recovering/replaying their work.
            let config = AsyncRuntimeSideEffectServiceConfig {
                max_polls: u64::MAX,
                ..work.config
            };
            run_async_persistence_side_effect_service_with_retirement(
                &work.handle,
                config,
                |polls, lifecycle| {
                    let normal_end = polls >= work.config.max_polls
                        || is_terminal_runtime_lifecycle_state(lifecycle);
                    let dependents = work.dependents.load(Ordering::Acquire);
                    if normal_end && dependents != 0 {
                        work.normal_complete.send_replace(true);
                    }
                    normal_end || work.stop.load(Ordering::Acquire)
                },
                || work.dependents.load(Ordering::Acquire) == 0,
            )
            .await
        });
        #[cfg(all(test, target_os = "linux"))]
        let abort = worker.abort_handle();
        *state = PersistenceDriverState::Running(worker);
        *started = true;
        #[cfg(all(test, target_os = "linux"))]
        {
            *self
                .0
                .abort
                .lock()
                .map_err(|_| MezError::invalid_state("persistence abort control poisoned"))? =
                Some(abort);
        }
        Ok(())
    }

    /// Observes an actually ready retained join when its wrapper already ended.
    /// Readiness only selects a poll: the actual JoinHandle result is required
    /// and cached before a dependent can report writer unavailability.
    fn observe_completed_nonblocking(&self) -> Result<()> {
        let Ok(mut state) = self.0.state.try_lock() else {
            return Ok(());
        };
        if let PersistenceDriverState::Running(worker) = &mut *state
            && worker.is_finished()
            && let Some(joined) = (&mut *worker).now_or_never()
        {
            let result = Self::joined_result(joined);
            *state = PersistenceDriverState::Joined(result);
            if let PersistenceDriverState::Joined(result) = &*state {
                self.retain_failure(result)?;
            }
        }
        Ok(())
    }

    /// Converts an observed actual join without arbitrary panic payloads.
    fn joined_result(
        joined: std::result::Result<
            Result<AsyncPersistenceSideEffectServiceReport>,
            tokio::task::JoinError,
        >,
    ) -> Result<AsyncPersistenceSideEffectServiceReport> {
        joined
            .map_err(|error| {
                let cause = if error.is_panic() {
                    "panic"
                } else {
                    "cancelled"
                };
                MezError::invalid_state(format!("persistence driver task failed: {cause}"))
            })
            .and_then(|result| result)
    }

    /// Conserves the first actual failure independently of wrapper observation.
    fn retain_failure(
        &self,
        result: &Result<AsyncPersistenceSideEffectServiceReport>,
    ) -> Result<()> {
        if let Err(error) = result {
            self.0
                .work
                .failure
                .lock()
                .map_err(|_| MezError::invalid_state("persistence failure custody lock poisoned"))?
                .get_or_insert_with(|| error.clone());
        }
        Ok(())
    }

    /// Borrows the actual writer join; cancellation leaves its handle installed.
    async fn join(&self) -> Result<AsyncPersistenceSideEffectServiceReport> {
        self.start()?;
        let mut state = self.0.state.lock().await;
        if let PersistenceDriverState::Running(worker) = &mut *state {
            let result = Self::joined_result(worker.await);
            *state = PersistenceDriverState::Joined(result);
            if let PersistenceDriverState::Joined(result) = &*state {
                self.retain_failure(result)?;
            }
        }
        match &*state {
            PersistenceDriverState::Joined(result) => result.clone(),
            _ => Err(MezError::invalid_state("persistence driver join missing")),
        }
    }

    /// Observes normal completion without dropping actual cleanup handle custody.
    /// Finite poll completion is separate from a supervised final writer join.
    pub(super) async fn run_wrapper(&self) -> Result<AsyncRuntimeServiceExit> {
        self.start()?;
        let mut normal = self.0.work.normal_complete.subscribe();
        tokio::select! {
            biased;
            joined = self.join() => joined.map(|report| AsyncRuntimeServiceExit::completed(
                report.completed.saturating_add(report.failed))),
            completed = normal.wait_for(|complete| *complete) => {
                completed.map_err(|_| MezError::invalid_state("persistence completion signal lost"))?;
                Ok(AsyncRuntimeServiceExit::completed(0))
            }
        }
    }

    /// Cancels only the actual writer task for owned failure-custody controls.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn abort_driver_for_tests(&self) {
        self.start().unwrap();
        self.0.abort.lock().unwrap().as_ref().unwrap().abort();
    }

    /// Stops optional admission while retained publishers keep essential I/O live.
    /// This atomic signal grants no join, storage acceptance or replay authority.
    pub(super) fn request_stop(&self) {
        self.0.work.stop.store(true, Ordering::Release);
    }

    /// Requests final drain after publisher retirement and observes the SAME task.
    /// Cancelling this observer leaves the actual join available for resumption.
    pub(super) async fn retire(&self) -> Result<AsyncPersistenceSideEffectServiceReport> {
        self.request_stop();
        self.join().await
    }
}

/// Actual actor/driver qualification independent of native worker fixtures.
#[cfg(test)]
mod tests;
