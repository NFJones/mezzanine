//! Filesystem producer custody before claim and across monitor cancellation.
//!
//! The root service creates this owner before polling an actor claim. The actor
//! retains the same allocation before issuing work and installs dispatch here;
//! a oneshot reply carries acceptance only. Closing unclaimed admission prevents
//! a later queued claim from starting work. Actual blocking handles and immutable
//! outcomes remain owned outside monitor futures. Last-owner drop requests
//! cancellation, but is not join, canonical settlement or durable acknowledgement.
//! Runtime service retirement must keep an external owner until those gates finish.

use std::sync::{Arc, Mutex};

use crate::error::{MezError, Result};
use crate::runtime::agent::native_filesystem::{
    NativeFilesystemAttempt, NativeFilesystemWork, RuntimeNativeFilesystemDispatch,
};
use crate::runtime::processes::native_patch::commit::{
    NativePatchCommitFence, NativePatchFenceStatus,
};
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;

mod archive;
mod execution;
mod progress;
use execution::Execution;
pub(crate) use progress::{
    NativeFilesystemProgress, NativeFilesystemProgressRecorder, endpoint_progress,
};

/// Short request-local admission state; no asynchronous wait holds this lock.
struct Admission {
    /// Retirement closes admission monotonically, even before an actor claim.
    closed: bool,
    /// A claim reservation is active without holding a lock across actor policy.
    claiming: bool,
    /// Original work remains available after dispatch moves to a producer.
    work: Option<Arc<NativeFilesystemWork>>,
    /// Exact action fence, retained independently of a running closure.
    fence: Option<NativePatchCommitFence>,
    /// Only positively claimed, never-started work can occupy this slot.
    dispatch: Option<Box<RuntimeNativeFilesystemDispatch>>,
}

/// Opaque producer allocation identity without retaining the producer itself.
/// Gates may carry this identity without creating a worker/owner custody cycle.
#[derive(Clone)]
pub(crate) struct NativeFilesystemProducerIdentity(Arc<()>);
impl PartialEq for NativeFilesystemProducerIdentity {
    /// Unit value equality would alias every producer; compare actual allocation.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for NativeFilesystemProducerIdentity {}

/// Shared custody retained independently by root service and actor registry.
struct Inner {
    /// Original allocation identity survives in gates without retaining this owner.
    identity: NativeFilesystemProducerIdentity,
    /// Exact opaque actor attempt whose work may be installed once.
    attempt: NativeFilesystemAttempt,
    /// Bounded synchronous claim/start/close transition owner.
    admission: Mutex<Admission>,
    /// Wakes retirement after a claim reservation finishes installing custody.
    claim_changed: tokio::sync::Notify,
    /// One original archive verifier remains root-owned across actor replacement.
    archive: Mutex<Option<archive::ArchiveWorkerOwner>>,
    /// Actual blocking task handle and original observed outcome.
    execution: tokio::sync::Mutex<Execution>,
    /// Bounded original confirmed receipts, independent of display delivery.
    progress: Mutex<
        Vec<Arc<crate::runtime::processes::native_patch::worker::NativePatchEndpointReceipt>>,
    >,
}

impl Drop for Inner {
    /// Signals cooperative cancellation on last-owner loss without claiming join.
    fn drop(&mut self) {
        if let Ok(admission) = self.admission.get_mut()
            && let Some(fence) = &admission.fence
        {
            fence.cancel();
        }
        if let Ok(Some(owner)) = self.archive.get_mut() {
            let _ = owner.receipt.close_for_replacement();
        }
    }
}

/// Closes a claim reservation if its synchronous actor callback unwinds.
struct ClaimReservation<'a> {
    /// Original shared owner, never a new replacement admission allocation.
    inner: &'a Inner,
    /// Normal completion has already installed or rejected the original claim.
    completed: bool,
}

impl Drop for ClaimReservation<'_> {
    /// Unwind releases waiting retirement without permitting a later producer.
    fn drop(&mut self) {
        if !self.completed {
            if let Ok(mut admission) = self.inner.admission.lock() {
                admission.closed = true;
                admission.claiming = false;
                if let Some(fence) = &admission.fence {
                    fence.cancel();
                }
            }
            self.inner.claim_changed.notify_waiters();
        }
    }
}

/// Exact per-offer producer registration, not an execution or settlement receipt.
#[derive(Clone)]
pub(crate) struct NativeFilesystemProducer(Arc<Inner>);

impl NativeFilesystemProducer {
    /// Creates root custody before an asynchronous claim can be polled.
    pub(crate) fn new(attempt: NativeFilesystemAttempt) -> Self {
        Self(Arc::new(Inner {
            identity: NativeFilesystemProducerIdentity(Arc::new(())),
            attempt,
            admission: Mutex::new(Admission {
                closed: false,
                claiming: false,
                work: None,
                fence: None,
                dispatch: None,
            }),
            claim_changed: tokio::sync::Notify::new(),
            archive: Mutex::new(None),
            execution: tokio::sync::Mutex::new(Execution::Unstarted),
            progress: Mutex::new(Vec::new()),
        }))
    }

    /// Returns exact actor identity; numeric labels cannot create this handle.
    pub(crate) fn attempt(&self) -> &NativeFilesystemAttempt {
        &self.0.attempt
    }

    /// Reports actual installed original work, not whether a reply was observed.
    pub(crate) fn is_claimed(&self) -> Result<bool> {
        Ok(self
            .0
            .admission
            .lock()
            .map_err(|_| MezError::invalid_state("native filesystem admission lock poisoned"))?
            .work
            .is_some())
    }

    /// Returns non-owning identity metadata for an actual registered producer.
    pub(crate) fn identity(&self) -> NativeFilesystemProducerIdentity {
        self.0.identity.clone()
    }

    /// Tests retained allocation identity, not equal attempt labels alone.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Installs the actual dispatch synchronously inside serialized actor claim.
    /// Closed or already claimed owners do not call the issuance closure. The
    /// actor must retain this allocation before its closure issues any producer.
    pub(crate) fn claim_with(
        &self,
        claim: impl FnOnce(&NativeFilesystemAttempt) -> Result<Option<RuntimeNativeFilesystemDispatch>>,
    ) -> Result<bool> {
        {
            let mut admission = self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
            if admission.closed || admission.claiming || admission.work.is_some() {
                return Ok(false);
            }
            admission.claiming = true;
        }
        let mut reservation = ClaimReservation {
            inner: &self.0,
            completed: false,
        };
        // Actor policy/source checks may perform synchronous I/O. Do not
        // hold the producer lock across that callback: root cancellation must
        // remain nonblocking even while claim preparation is outstanding.
        let result = claim(&self.0.attempt);
        let mut admission =
            self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
        admission.claiming = false;
        let result = match result {
            Ok(Some(dispatch)) => {
                if dispatch.attempt != self.0.attempt {
                    dispatch.fence.cancel();
                    Err(MezError::conflict(
                        "native filesystem producer received another attempt",
                    ))
                } else {
                    if admission.closed {
                        dispatch.fence.cancel();
                    }
                    admission.work = Some(Arc::clone(&dispatch.work));
                    admission.fence = Some(dispatch.fence.clone());
                    admission.dispatch = Some(Box::new(dispatch));
                    Ok(!admission.closed)
                }
            }
            Ok(None) => Ok(false),
            Err(error) => Err(error),
        };
        reservation.completed = true;
        self.0.claim_changed.notify_waiters();
        result
    }

    /// Closes admission and signals issued work before waiting for any join.
    /// InFlight reports a held commit guard, not a stopped producer or mutation.
    pub(crate) fn close(&self) -> Result<Option<NativePatchFenceStatus>> {
        let mut admission =
            self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
        admission.closed = true;
        Ok(admission.fence.as_ref().map(NativePatchCommitFence::cancel))
    }

    /// Starts claimed work once, storing the actual handle before yielding.
    /// Producer identity and the exact fence remain retained in admission state.
    pub(crate) async fn start(
        &self,
        execute: impl FnOnce(RuntimeNativeFilesystemDispatch) -> NativePatchWorkerOutcome
        + Send
        + 'static,
    ) -> Result<bool> {
        let mut execution = self.0.execution.lock().await;
        if !matches!(*execution, Execution::Unstarted) {
            return Ok(false);
        }
        let mut admission =
            self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
        if admission.closed {
            return Ok(false);
        }
        let Some(dispatch) = admission.dispatch.take() else {
            return Ok(false);
        };
        let work = Arc::clone(&dispatch.work);
        *execution = Execution::Running {
            worker: tokio::task::spawn_blocking(move || Arc::new(execute(*dispatch))),
            work,
        };
        Ok(true)
    }

    /// Borrows actual join custody; cancellation of this future cannot remove
    /// the handle or consume an observed immutable outcome. None is unstarted.
    pub(crate) async fn observe(&self) -> Result<Option<Arc<NativePatchWorkerOutcome>>> {
        let mut execution = self.0.execution.lock().await;
        self.observe_locked(&mut execution).await
    }

    /// Cancels before joining and preserves exact never-started evidence. A
    /// blocked OS operation remains owned here; no timeout fabricates its exit.
    pub(crate) async fn retire(&self) -> Result<Option<Arc<NativePatchWorkerOutcome>>> {
        self.close()?;
        loop {
            let changed = self.0.claim_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let claiming = self
                .0
                .admission
                .lock()
                .map_err(|_| MezError::invalid_state("native filesystem admission lock poisoned"))?
                .claiming;
            if !claiming {
                break;
            }
            changed.await;
        }
        let mut execution = self.0.execution.lock().await;
        if matches!(*execution, Execution::Unstarted) {
            let mut admission = self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
            if let Some(dispatch) = admission.dispatch.take() {
                *execution =
                    Execution::Joined(Arc::new(execution::unstarted(&dispatch.work.identity)));
            }
        }
        let outcome = self.observe_locked(&mut execution).await;
        drop(execution);
        if let Some((_, receipt)) = self.archive_worker()? {
            receipt.observe().await?;
        }
        outcome
    }

    /// Observes an actual task while retaining its handle across each await.
    async fn observe_locked(
        &self,
        execution: &mut Execution,
    ) -> Result<Option<Arc<NativePatchWorkerOutcome>>> {
        if let Execution::Running { worker, work } = execution {
            let result = worker.await;
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => Arc::new(execution::lost(&work.identity, error.is_cancelled())),
            };
            *execution = Execution::Joined(outcome);
        }
        match execution {
            Execution::Unstarted => Ok(None),
            Execution::Joined(outcome) => Ok(Some(Arc::clone(outcome))),
            Execution::Running { .. } => Err(MezError::invalid_state(
                "native filesystem join did not retain outcome",
            )),
        }
    }
}
