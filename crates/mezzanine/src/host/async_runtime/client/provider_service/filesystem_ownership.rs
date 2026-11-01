//! Root provider custody for actual filesystem producers before first claim.
//!
//! The registered root map retains each producer independently of its monitor.
//! Monitors perform no effect replay: they claim/start once and observe original
//! canonical/storage transfer until its exact acknowledgement exists. Retirement
//! closes queued admission before joining actual workers, and map removal requires
//! that original acknowledgement. Pending source/storage and unissued refusal
//! ownership remain explicit errors with custody retained, never fabricated joins.

use super::*;
use crate::runtime::RuntimeNativeFilesystemOffer;
use crate::runtime::native_filesystem::NativeFilesystemProducer;

/// Reduce-only stop controls remain reachable while the driver holds its async
/// state mutex or waits for an unrelated worker/storage operation. Registration
/// and stopping share a short synchronous fence, never held across an await.
#[derive(Default)]
pub(super) struct FilesystemRootStops {
    /// Weak reduce-only controls for exactly the registered root task ids.
    controls: std::sync::Mutex<
        std::collections::HashMap<
            tokio::task::Id,
            crate::runtime::native_filesystem::NativeFilesystemProducerStop,
        >,
    >,
    /// Monotonic registration fence shared with the external supervisor.
    stopping: std::sync::atomic::AtomicBool,
}
impl FilesystemRootStops {
    /// Registers before releasing first-poll admission. A racing/already requested
    /// service stop closes this actual producer before its claim can start.
    pub(super) fn register(&self, id: tokio::task::Id, producer: &NativeFilesystemProducer) {
        let mut controls = match self.controls.lock() {
            Ok(controls) => controls,
            Err(poisoned) => poisoned.into_inner(),
        };
        let control = producer.stop_control();
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            control.request_stop();
        }
        controls.insert(id, control);
    }
    /// Removes cancellation metadata only after the exact transfer is accepted.
    pub(super) fn remove(&self, id: tokio::task::Id) {
        match self.controls.lock() {
            Ok(mut controls) => {
                controls.remove(&id);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(&id);
            }
        }
    }
    /// Signals every captured source before waiting on the driver's async mutex.
    /// Poison recovery conserves already registered reduce-only controls; this
    /// operation never claims joins, canonical facts or completed transfers.
    pub(super) fn request_stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        let controls = match self.controls.lock() {
            Ok(controls) => controls,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Fence every sibling before taking even a short admission lock.
        for control in controls.values() {
            control.fence();
        }
        for control in controls.values() {
            control.request_stop();
        }
    }
}

/// Shared actual recovery handle outside the abortable monitor. The recovery
/// closure captures only producer/actor, never this handle owner or root map.
#[derive(Clone)]
pub(super) struct FilesystemRootOwner(std::sync::Arc<FilesystemRootInner>);
/// Root recovery data, independent of its task closure and driver future.
struct FilesystemRootInner {
    /// Original allocation retained through every recovery observer.
    producer: NativeFilesystemProducer,
    /// Actual recovery join/result retained even when an observer is dropped.
    recovery: tokio::sync::Mutex<FilesystemRecovery>,
}
/// No observer can move a running actual handle out across an await.
enum FilesystemRecovery {
    /// No recovery observer task exists yet.
    Unstarted,
    /// Actual observer task remains owned by this state.
    Running(tokio::task::JoinHandle<Result<()>>),
    /// Cached actual join result, never a task-status success bit.
    Joined(Result<()>),
}
impl Drop for FilesystemRootInner {
    /// Last-owner loss only requests stop and aborts an observer; it cannot
    /// certify actual worker joins or a completed transfer outside supervision.
    fn drop(&mut self) {
        self.producer.stop_control().request_stop();
        if let FilesystemRecovery::Running(handle) = self.recovery.get_mut() {
            handle.abort();
        }
    }
}
impl FilesystemRootOwner {
    /// Creates independent root recovery custody before first claim admission.
    pub(super) fn new(producer: NativeFilesystemProducer) -> Self {
        Self(std::sync::Arc::new(FilesystemRootInner {
            producer,
            recovery: tokio::sync::Mutex::new(FilesystemRecovery::Unstarted),
        }))
    }
    /// Returns the actual registered allocation, never a same-label replacement.
    pub(super) fn producer(&self) -> &NativeFilesystemProducer {
        &self.0.producer
    }
    /// Requires the exact opaque original transfer, not a finished monitor bit.
    pub(super) fn acknowledged(&self) -> Result<bool> {
        Ok(self
            .0
            .producer
            .transfer_acknowledgement()?
            .is_some_and(|ack| ack.acknowledges(&self.0.producer)))
    }
    /// Installs one actual recovery handle before yielding. Concurrent sibling
    /// recoveries can project original facts needed by ordered publication.
    pub(super) async fn start_recovery(&self, handle: &AsyncRuntimeSessionHandle) {
        let mut state = self.0.recovery.lock().await;
        if matches!(*state, FilesystemRecovery::Unstarted) {
            let producer = self.0.producer.clone();
            let handle = handle.clone();
            *state = FilesystemRecovery::Running(tokio::spawn(async move {
                retire_filesystem_owner(&handle, &producer).await
            }));
        }
    }
    /// Cancels the actual recovery task without replacing its retained handle.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) async fn abort_recovery_for_tests(&self) {
        let state = self.0.recovery.lock().await;
        match &*state {
            FilesystemRecovery::Running(worker) => worker.abort(),
            _ => panic!("expected an actual running recovery task"),
        }
    }
    /// Polls readiness without borrowing any actual handle out of root custody.
    pub(super) fn recovery_ready(&self) -> bool {
        self.0.recovery.try_lock().is_ok_and(|state| match &*state {
            FilesystemRecovery::Joined(_) => true,
            FilesystemRecovery::Running(handle) => handle.is_finished(),
            _ => false,
        })
    }
    /// Borrows the actual recovery join across await and caches its bounded
    /// result. Cancelled observers leave this handle available for supervision.
    pub(super) async fn observe_recovery(&self, handle: &AsyncRuntimeSessionHandle) -> Result<()> {
        self.start_recovery(handle).await;
        let mut state = self.0.recovery.lock().await;
        if let FilesystemRecovery::Running(worker) = &mut *state {
            let result = worker
                .await
                .map_err(|_| {
                    MezError::invalid_state(
                        "filesystem recovery observer failed; original owner retained",
                    )
                })
                .and_then(|result| result);
            *state = FilesystemRecovery::Joined(result);
        }
        match &*state {
            FilesystemRecovery::Joined(result) => result.clone(),
            _ => Err(MezError::invalid_state("filesystem recovery join missing")),
        }
    }
}

/// Restores actual source custody once at sole-driver startup. Lost offers do
/// not require an unrelated timer, and registered workers are never recreated
/// from matching labels. Existing root entries remain the authority on retries.
pub(super) async fn recover_filesystem_workers(
    handle: &AsyncRuntimeSessionHandle,
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    drains: &ProviderObservationDrains,
) -> Result<()> {
    for (offer, recovered) in handle.recover_native_filesystem_producers().await? {
        if let Some(known) = owners.known_filesystem(&offer) {
            if recovered
                .as_ref()
                .is_some_and(|actual| !actual.same(&known))
            {
                return Err(MezError::conflict(
                    "filesystem recovered allocation differs from retained root",
                ));
            }
            continue;
        }
        if let Some(producer) = recovered {
            worker_ownership::spawn_owned_filesystem_agent_worker(
                workers,
                owners,
                producer.clone(),
                monitor(handle.clone(), producer),
            );
        } else {
            Box::pin(super::dispatch_agent_provider_side_effects(
                handle,
                vec![RuntimeSideEffect::DispatchNativeFilesystemAction { offer }],
                workers,
                drains,
                owners,
            ))
            .await?;
        }
    }
    Ok(())
}

/// Installs root custody synchronously before the monitor can poll its claim.
pub(super) fn spawn_filesystem_action_worker(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    handle: AsyncRuntimeSessionHandle,
    offer: RuntimeNativeFilesystemOffer,
) -> tokio::task::AbortHandle {
    let producer = NativeFilesystemProducer::new(offer.attempt().clone());
    worker_ownership::spawn_owned_filesystem_agent_worker(
        workers,
        owners,
        producer.clone(),
        monitor(handle, producer),
    )
}

/// Waits only for original claim and transfer dependencies. The admitted action
/// deadline is never renewed, and observer loss cannot remove actual handle custody.
async fn monitor(
    handle: AsyncRuntimeSessionHandle,
    producer: NativeFilesystemProducer,
) -> Result<AsyncAgentProviderWorkerResult> {
    use crate::host::async_runtime::native_filesystem::{
        claim_and_start_native_filesystem_producer, transfer_joined_native_filesystem_producer,
    };
    loop {
        if producer.is_claimed()? {
            if producer.is_closed()? {
                producer.retire().await?;
            } else {
                crate::host::async_runtime::native_filesystem::start_native_filesystem_producer(
                    handle.clone(),
                    &producer,
                )
                .await?;
            }
            break;
        }
        if claim_and_start_native_filesystem_producer(handle.clone(), &producer).await? {
            break;
        }
        if producer.is_closed()? {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    loop {
        if transfer_joined_native_filesystem_producer(&handle, &producer)
            .await?
            .is_some()
        {
            // Root adoption separately checks the cached opaque acknowledgement;
            // None carries no fabricated shell event or caller completion bit.
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Observes real native/storage handles before requesting exact original transfer.
/// Missing admission retains explicit unissued ownership. Pending storage waits
/// with the root entry installed; it is not a new required failure or completion.
pub(super) async fn retire_filesystem_owner(
    handle: &AsyncRuntimeSessionHandle,
    producer: &NativeFilesystemProducer,
) -> Result<()> {
    if producer.retire().await?.is_none() {
        return Err(MezError::invalid_state(
            "filesystem root retains unissued source awaiting canonical refusal handoff",
        ));
    }
    loop {
        let accepted = crate::host::async_runtime::native_filesystem::transfer_joined_native_filesystem_producer(handle, producer).await?;
        if accepted.is_some_and(|ack| ack.acknowledges(producer)) {
            break;
        }
        if handle.lifecycle_state_watcher().has_changed().is_err() {
            return Err(MezError::invalid_state(
                "filesystem root retains source after actor loss",
            ));
        }
        // The ordered persistence service can still settle this original
        // source. Waiting is not a new failure or permission to erase custody.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}
