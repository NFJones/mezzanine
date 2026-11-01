//! Provider-service task retirement and cancellation-safe request ownership.
//!
//! The service retains task identities outside monitor futures. Retirement first
//! observes producer destruction, then conserves reported expense and submits one
//! captured failure. Normal completed outcomes keep their original authority;
//! shared monitor/actor failures remain fatal. Registries live only as long as
//! their bounded in-flight workers and never authorize another provider request.
//! Native requests retain actual blocking handles and original outcomes in a
//! separate per-request owner; model observation journals never impersonate
//! native completion, process custody or replay authority.

use super::native_ownership::NativeWorkerOwner;
use super::{
    AsyncAgentProviderWorkerResult, ProviderObservationDrain, ProviderObservationDrains,
    flush_pending_provider_observations, record_joined_agent_provider_worker,
    submit_authoritative_runtime_event,
};
use crate::error::{MezError, Result};
use crate::host::async_runtime::{AsyncAgentProviderPollReport, AsyncRuntimeSessionHandle};
use crate::integrations::agent::provider::ProviderReportedUsage;
use crate::runtime::{
    AgentCompactionEvent, AgentProviderEvent, AgentRememberEvent, AgentSessionTitleEvent,
    AgentSessionTitleOutcome, RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentRememberDispatch, RuntimeAgentSessionTitleDispatch, RuntimeEvent,
};
use mez_core::ids::AgentId;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use tokio::task::JoinSet;

/// Collision-free owner of one retained observation producer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ProviderObservationOwner {
    /// Ordinary inference retains its issued turn generation.
    Inference(String, u64),
    /// Compaction retains its pane and exact task generation.
    Compaction(String, u64),
    /// Memory observations are minted once by the owning scheduler.
    Remember(String),
    /// A title attempt is distinct from its unissued dispatch descriptor.
    Title(String),
}

/// Immutable request identity retained before its monitor can be polled.
#[derive(Debug, Clone)]
pub(super) enum ProviderWorkerOwner {
    /// Inference retains its validated agent and actual issued claim.
    Inference(AgentId, String, u64),
    /// Exact compaction completion destination.
    Compaction(String, u64),
    /// Exact memory completion destination.
    Remember(String, String),
    /// Exact optional title completion destination.
    Title(String, String),
}

impl ProviderWorkerOwner {
    /// Captures a genuinely admitted inference request. Wire checkpointing
    /// remains authoritative for its expense; cancellation supplies no new bill.
    pub(super) fn inference(agent_id: AgentId, dispatch: &RuntimeAgentProviderDispatch) -> Self {
        Self::Inference(
            agent_id,
            dispatch.turn.turn_id.clone(),
            dispatch.claim_generation,
        )
    }

    /// Captures a genuinely admitted compaction dispatch without mutable state.
    pub(super) fn compaction(dispatch: &RuntimeAgentCompactionDispatch) -> Self {
        Self::Compaction(dispatch.task.pane_id.clone(), dispatch.task.task_generation)
    }

    /// Captures a genuinely admitted memory dispatch.
    pub(super) fn remember(dispatch: &RuntimeAgentRememberDispatch) -> Self {
        Self::Remember(
            dispatch.task.pane_id.clone(),
            dispatch.task.observation_id.clone(),
        )
    }

    /// Captures an issued title attempt, never its queued dispatch id.
    pub(super) fn title(dispatch: &RuntimeAgentSessionTitleDispatch) -> Self {
        Self::Title(
            dispatch.task.conversation_id.clone(),
            dispatch.task.attempt_id.clone(),
        )
    }

    /// Locates the producer retained independently of monitor cancellation.
    fn key(&self) -> ProviderObservationOwner {
        match self {
            Self::Inference(_, turn, generation) => {
                ProviderObservationOwner::Inference(turn.clone(), *generation)
            }
            Self::Compaction(pane, generation) => {
                ProviderObservationOwner::Compaction(pane.clone(), *generation)
            }
            Self::Remember(_, observation) => {
                ProviderObservationOwner::Remember(observation.clone())
            }
            Self::Title(_, attempt) => ProviderObservationOwner::Title(attempt.clone()),
        }
    }

    /// Builds failure against the captured owner with only actually reported cost.
    fn event(&self, cause: RetirementCause, usage: mez_agent::ModelTokenUsage) -> RuntimeEvent {
        match self {
            Self::Inference(agent, turn, generation) => {
                RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                    agent_id: agent.clone(),
                    turn_id: turn.clone(),
                    claim_generation: *generation,
                    kind: "invalid_state".into(),
                    message: cause.message().into(),
                    provider_failure_json: None,
                    provider_raw_text: None,
                    provider_output_limit_state: None,
                })
            }
            Self::Compaction(pane, generation) => {
                RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                    pane_id: pane.clone(),
                    task_generation: *generation,
                    usage,
                    kind: "invalid_state".into(),
                    message: cause.message().into(),
                    provider_failure_json: None,
                    provider_raw_text: None,
                })
            }
            Self::Remember(pane, observation) => {
                RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                    pane_id: pane.clone(),
                    observation_id: observation.clone(),
                    usage,
                    kind: "invalid_state".into(),
                    message: cause.message().into(),
                    provider_failure_json: None,
                    provider_raw_text: None,
                })
            }
            Self::Title(conversation, attempt) => {
                RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled {
                    conversation_id: conversation.clone(),
                    attempt_id: attempt.clone(),
                    usage,
                    outcome: AgentSessionTitleOutcome::Rejected(cause.title_reason().into()),
                })
            }
        }
    }
}

/// Bounded cause kept separately from arbitrary panic payloads.
#[derive(Debug, Clone, Copy)]
enum RetirementCause {
    /// Service cancellation destroyed the monitor without a terminal result.
    Cancelled,
    /// A monitor unwind compromises its service and remains fatal.
    MonitorPanic,
    /// A monitor returned a shared actor/observation error before completion.
    MonitorError,
}

impl RetirementCause {
    /// Supplies public failure text without task debug data or payloads.
    fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "provider worker join failed: cancelled",
            Self::MonitorPanic => "provider monitor failed: panic",
            Self::MonitorError => "provider monitor failed before completion",
        }
    }

    /// Supplies optional-title rejection vocabulary without ordinary turn failure.
    fn title_reason(self) -> &'static str {
        use crate::session_title::SessionTitleFailureReason;
        match self {
            Self::Cancelled => SessionTitleFailureReason::WorkerCancelled.as_str(),
            Self::MonitorPanic => SessionTitleFailureReason::MonitorPanic.as_str(),
            Self::MonitorError => SessionTitleFailureReason::MonitorFailed.as_str(),
        }
    }
}

/// Work retained independently of a request monitor's lifetime.
enum OwnedAgentWork {
    /// Inference or auxiliary request with its observation custody.
    Provider(ProviderWorkerOwner),
    /// Native dispatch with its actual blocking handle and original result.
    Native(NativeWorkerOwner),
    /// Actual filesystem producer retained before its first actor claim poll.
    Filesystem(super::filesystem_ownership::FilesystemRootOwner),
}

/// One registered task, including explicit non-request ownership.
pub(super) struct WorkerOwnership {
    /// Request metadata whose lifetime precedes the monitor future.
    request: Option<OwnedAgentWork>,
    /// Fatal monitor loss is retained through cleanup without changing its cause.
    cause: RetirementCause,
}

/// Every root provider task is registered, so missing identity remains fatal.
#[derive(Default)]
pub(super) struct AgentProviderWorkerOwners {
    /// Exact live/unadopted task registrations, excluding settled history.
    entries: HashMap<tokio::task::Id, WorkerOwnership>,
    /// External stop access never depends on this driver's async mutex.
    filesystem: std::sync::Arc<super::filesystem_ownership::FilesystemRootStops>,
}
impl std::ops::Deref for AgentProviderWorkerOwners {
    type Target = HashMap<tokio::task::Id, WorkerOwnership>;
    /// Preserves existing bounded non-filesystem registry operations.
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}
impl std::ops::DerefMut for AgentProviderWorkerOwners {
    /// Mutations remain exclusive to the retained service driver.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}
impl AgentProviderWorkerOwners {
    /// Shares reduce-only controls without borrowing driver state across stop.
    pub(super) fn filesystem_stops(
        &self,
    ) -> std::sync::Arc<super::filesystem_ownership::FilesystemRootStops> {
        self.filesystem.clone()
    }
    /// Finds only the retained original allocation for this opaque source token.
    pub(super) fn known_filesystem(
        &self,
        offer: &crate::runtime::RuntimeNativeFilesystemOffer,
    ) -> Option<crate::runtime::native_filesystem::NativeFilesystemProducer> {
        self.values()
            .filter_map(filesystem_owner)
            .find(|owner| owner.producer().attempt() == offer.attempt())
            .map(|owner| owner.producer().clone())
    }

    /// Removes an acknowledged original owner and its reduce-only stop metadata.
    fn remove_filesystem(&mut self, id: tokio::task::Id) {
        self.filesystem.remove(id);
        self.entries.remove(&id);
    }
}

/// Cleanup evidence retained outside an abortable provider-service future.
#[derive(Default)]
pub(super) struct ProviderCleanupFailures {
    /// First actual cleanup failure, including its original typed facts.
    first: Option<MezError>,
    /// Bounded count of further failures while sibling retirement continues.
    additional: usize,
}

/// Observed task identity paired with its unmodified monitor result.
pub(super) type AgentProviderJoined = std::result::Result<
    (tokio::task::Id, Result<AsyncAgentProviderWorkerResult>),
    tokio::task::JoinError,
>;

/// Registers identity synchronously after spawn and before another service await.
pub(super) fn spawn_owned_agent_provider_worker<F>(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    request: Option<ProviderWorkerOwner>,
    future: F,
) -> tokio::task::AbortHandle
where
    F: Future<Output = Result<AsyncAgentProviderWorkerResult>> + Send + 'static,
{
    spawn_owned_worker(
        workers,
        owners,
        request.map(OwnedAgentWork::Provider),
        future,
    )
}

/// Registers actual native custody before its monitor can be polled.
pub(super) fn spawn_owned_native_agent_worker<F>(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    request: NativeWorkerOwner,
    future: F,
) -> tokio::task::AbortHandle
where
    F: Future<Output = Result<AsyncAgentProviderWorkerResult>> + Send + 'static,
{
    spawn_owned_worker(
        workers,
        owners,
        Some(OwnedAgentWork::Native(request)),
        future,
    )
}

/// Registers the exact filesystem producer outside its abortable monitor.
pub(super) fn spawn_owned_filesystem_agent_worker<F>(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    producer: crate::runtime::native_filesystem::NativeFilesystemProducer,
    future: F,
) -> tokio::task::AbortHandle
where
    F: Future<Output = Result<AsyncAgentProviderWorkerResult>> + Send + 'static,
{
    let stop_control = producer.clone();
    let (registered, admission) = tokio::sync::oneshot::channel();
    let task = spawn_owned_worker(
        workers,
        owners,
        Some(OwnedAgentWork::Filesystem(
            super::filesystem_ownership::FilesystemRootOwner::new(producer),
        )),
        async move {
            admission.await.map_err(|_| {
                MezError::invalid_state("filesystem root registration lost before claim")
            })?;
            future.await
        },
    );
    // Tokio may poll concurrently on another thread immediately after spawn.
    // Only this send, after map insertion, releases the monitor's first poll.
    owners.filesystem.register(task.id(), &stop_control);
    let _ = registered.send(());
    task
}

/// Clones only actual filesystem custody while its root map entry stays installed.
fn filesystem_owner(
    ownership: &WorkerOwnership,
) -> Option<super::filesystem_ownership::FilesystemRootOwner> {
    match &ownership.request {
        Some(OwnedAgentWork::Filesystem(producer)) => Some(producer.clone()),
        _ => None,
    }
}

/// Installs every root task identity synchronously before a service await.
fn spawn_owned_worker<F>(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    owners: &mut AgentProviderWorkerOwners,
    request: Option<OwnedAgentWork>,
    future: F,
) -> tokio::task::AbortHandle
where
    F: Future<Output = Result<AsyncAgentProviderWorkerResult>> + Send + 'static,
{
    let task = workers.spawn(future);
    owners.insert(
        task.id(),
        WorkerOwnership {
            request,
            cause: RetirementCause::Cancelled,
        },
    );
    task
}

/// Exposes the real retained allocation for isolated root-custody controls.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn filesystem_owner_for_tests(
    owners: &AgentProviderWorkerOwners,
    id: tokio::task::Id,
) -> Option<crate::runtime::native_filesystem::NativeFilesystemProducer> {
    owners
        .get(&id)
        .and_then(filesystem_owner)
        .map(|owner| owner.producer().clone())
}

/// Exposes the actual recovery-handle owner for cancellation qualification.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn filesystem_recovery_for_tests(
    owners: &AgentProviderWorkerOwners,
    id: tokio::task::Id,
) -> Option<super::filesystem_ownership::FilesystemRootOwner> {
    owners.get(&id).and_then(filesystem_owner)
}

/// Takes the expense of an already joined auxiliary producer. Missing retained
/// identity is corruption, not evidence of zero usage or permission to continue.
pub(super) fn take_completed_auxiliary_usage(
    drains: &ProviderObservationDrains,
    key: &ProviderObservationOwner,
) -> Result<mez_agent::ModelTokenUsage> {
    let drain = drains
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(key)
        .ok_or_else(|| {
            MezError::invalid_state("completed auxiliary worker has no retained observation owner")
        })?;
    let reported = drain.reported.ok_or_else(|| {
        MezError::invalid_state("completed auxiliary worker has no retained reported expense")
    })?;
    let usage = reported
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .usage;
    Ok(usage)
}

/// Supplies reported expense only when no terminal request result survived its
/// join. Unexpected success/event kinds remain fatal rather than fabricating
/// response content or silently dropping cost under a mismatched owner.
pub(super) fn with_reported_auxiliary_failure_usage(
    completion: (RuntimeEvent, bool),
    reported: mez_agent::ModelTokenUsage,
) -> Result<(RuntimeEvent, bool)> {
    let (mut event, completed) = completion;
    if completed {
        return Err(MezError::invalid_state(
            "lost auxiliary request produced a successful completion",
        ));
    }
    match &mut event {
        RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed { usage, .. })
        | RuntimeEvent::AgentRemember(AgentRememberEvent::Failed { usage, .. })
        | RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled {
            usage,
            outcome: AgentSessionTitleOutcome::Rejected(_),
            ..
        }) => *usage = reported,
        _ => {
            return Err(MezError::invalid_state(
                "lost auxiliary request produced an unexpected failure event",
            ));
        }
    }
    Ok((event, false))
}

/// Destroys one retained producer before reading its final reported counters.
/// Observation delivery errors remain distinct from the available cost snapshot.
async fn retire_observation_drain(
    handle: &AsyncRuntimeSessionHandle,
    drain: ProviderObservationDrain,
) -> (ProviderReportedUsage, Result<()>) {
    drain.abort.abort();
    let _ = drain.completed.await;
    let reported = drain
        .reported
        .as_ref()
        .map_or_else(ProviderReportedUsage::default, |reported| {
            *reported.lock().unwrap_or_else(|error| error.into_inner())
        });
    let delivery = flush_pending_provider_observations(handle, &drain.pending).await;
    (reported, delivery)
}

/// Retires a captured request producer and attempts both diagnostic delivery
/// and exact failure accounting even if one of those independent operations fails.
async fn retire_request_owner(
    handle: &AsyncRuntimeSessionHandle,
    owner: OwnedAgentWork,
    cause: RetirementCause,
    drains: &ProviderObservationDrains,
) -> Result<()> {
    let owner = match owner {
        OwnedAgentWork::Provider(owner) => owner,
        OwnedAgentWork::Native(owner) => return retire_native_owner(handle, owner, cause).await,
        OwnedAgentWork::Filesystem(_) => {
            return Err(MezError::invalid_state(
                "filesystem retirement must retain root map custody",
            ));
        }
    };
    let drain = {
        drains
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&owner.key())
    };
    let (reported, delivery) = match drain {
        Some(drain) => retire_observation_drain(handle, drain).await,
        None => (ProviderReportedUsage::default(), Ok(())),
    };
    let closed = handle.lifecycle_state_watcher().has_changed().is_err();
    let settlement = if closed && reported.known_observations == 0 {
        // The actor is already destroyed, not merely terminal or temporarily
        // busy. No reported expense or live claim can be adopted there.
        Ok(())
    } else {
        submit_authoritative_runtime_event(handle, owner.event(cause, reported.usage)).await
    };
    finish_provider_service_result(delivery, settlement)
}

/// Observes actual native producer retirement and adopts its original outcome.
/// Actor ingress failure remains fatal after resources have actually joined;
/// known native outcomes are not silently discarded as successful cleanup.
async fn retire_native_owner(
    handle: &AsyncRuntimeSessionHandle,
    owner: NativeWorkerOwner,
    cause: RetirementCause,
) -> Result<()> {
    let message = match cause {
        RetirementCause::Cancelled => "native shell worker retired: cancelled",
        RetirementCause::MonitorPanic => "native shell monitor failed: panic",
        RetirementCause::MonitorError => "native shell monitor failed before completion",
    };
    let outcome = owner.retire(message).await?;
    submit_authoritative_runtime_event(handle, RuntimeEvent::NativeShell(outcome)).await
}

/// Preserves fatal shared errors while retaining their cleanup-only identity.
/// A normal returned event is removed before submission, so an unknown/applied
/// actor error can never trigger replay or a fabricated cancellation over it.
pub(super) async fn record_owned_agent_provider_worker(
    joined: AgentProviderJoined,
    handle: &AsyncRuntimeSessionHandle,
    report: &mut AsyncAgentProviderPollReport,
    owners: &mut AgentProviderWorkerOwners,
    drains: &ProviderObservationDrains,
) -> Result<()> {
    let id = match &joined {
        Ok((id, _)) => *id,
        Err(error) => error.id(),
    };
    if let Some(owner) = owners.get(&id).and_then(filesystem_owner) {
        // Keep map custody while starting recovery. Its actual task handle is
        // retained separately; serial monitor draining must not wait on siblings.
        let result = match joined {
            Err(error) if !error.is_cancelled() => {
                Err(MezError::invalid_state(if error.is_panic() {
                    "filesystem monitor failed: panic; original owner retained"
                } else {
                    "filesystem monitor join failed; original owner retained"
                }))
            }
            Ok((_, Err(error))) => Err(error),
            Ok((_, Ok(Some(_)))) => Err(MezError::invalid_state(
                "filesystem monitor returned an unrelated runtime event",
            )),
            _ => {
                if !owner.acknowledged()? {
                    owner.start_recovery(handle).await;
                }
                Ok(())
            }
        };
        if result.is_ok() && owner.acknowledged()? {
            owners.remove_filesystem(id);
        }
        return result;
    }
    let mut ownership = owners.remove(&id).ok_or_else(|| {
        MezError::invalid_state("provider worker completion has no retained ownership")
    })?;
    match joined {
        Err(error) if error.is_cancelled() => {
            if let Some(owner) = ownership.request {
                retire_request_owner(handle, owner, ownership.cause, drains).await
            } else {
                Ok(())
            }
        }
        Err(error) => {
            ownership.cause = if error.is_panic() {
                RetirementCause::MonitorPanic
            } else {
                RetirementCause::MonitorError
            };
            owners.insert(id, ownership);
            record_joined_agent_provider_worker(Err(error), handle, report).await
        }
        Ok((_, Err(error))) => {
            ownership.cause = RetirementCause::MonitorError;
            owners.insert(id, ownership);
            Err(error)
        }
        Ok((_, Ok(None))) => match ownership.request {
            Some(owner) => retire_request_owner(handle, owner, ownership.cause, drains).await,
            None => Ok(()),
        },
        Ok((_, result)) => record_joined_agent_provider_worker(Ok(result), handle, report).await,
    }
}

/// Adopts only completed recovery joins; live I/O never blocks unrelated dispatch.
pub(super) async fn drain_filesystem_recoveries(
    handle: &AsyncRuntimeSessionHandle,
    owners: &mut AgentProviderWorkerOwners,
) -> Result<()> {
    let completed: Vec<_> = owners
        .iter()
        .filter_map(|(id, ownership)| {
            filesystem_owner(ownership)
                .filter(|owner| owner.recovery_ready())
                .map(|owner| (*id, owner))
        })
        .collect();
    for (id, owner) in completed {
        let retired = owner.observe_recovery(handle).await;
        // A failed observer cannot invalidate a later genuine transfer. Observe
        // its actual join first, release qualified custody, then report the
        // original failure rather than keeping an already transferred owner.
        let released = release_filesystem_custody(owners, id, &owner);
        finish_provider_service_result(retired, released)?;
    }
    Ok(())
}

/// Releases only a qualified transfer after the caller observes the retained
/// recovery join. Missing acknowledgement remains an explicit cleanup failure;
/// acknowledgement-read failures cannot override an earlier observer error.
fn release_filesystem_custody(
    owners: &mut AgentProviderWorkerOwners,
    id: tokio::task::Id,
    owner: &super::filesystem_ownership::FilesystemRootOwner,
) -> Result<()> {
    if !owner.acknowledged()? {
        return Err(MezError::invalid_state(
            "filesystem root retains source awaiting original transfer acknowledgement",
        ));
    }
    owners.remove_filesystem(id);
    Ok(())
}

/// Keeps the original failure classification and typed facts when cleanup also
/// fails. Secondary details are bounded categories rather than panic payloads.
pub(super) fn finish_provider_service_result<T>(
    primary: Result<T>,
    cleanup: Result<()>,
) -> Result<T> {
    match (primary, cleanup) {
        (Err(error), Err(cleanup)) => Err(error.with_context(format!(
            "provider worker cleanup incomplete; secondary_kind={:?}",
            cleanup.kind()
        ))),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Finishes every owned task and producer even after a cleanup error. Completed
/// outcomes are adopted once; cancellation never impersonates a successful result.
pub(super) async fn retire_agent_provider_workers(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    handle: &AsyncRuntimeSessionHandle,
    drains: &ProviderObservationDrains,
    owners: &mut AgentProviderWorkerOwners,
    failures: &mut ProviderCleanupFailures,
) -> Result<u64> {
    owners.filesystem.request_stop();
    workers.abort_all();
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: *handle.lifecycle_state_watcher().borrow(),
    };
    while let Some(joined) = workers.join_next_with_id().await {
        keep_cleanup_error(
            failures,
            record_owned_agent_provider_worker(joined, handle, &mut report, owners, drains).await,
        );
    }
    // Fatal monitor failures were observed above but remain retained until their
    // producer is destroyed. Other root producers use the same expense drain.
    let retained = std::mem::take(&mut *drains.lock().unwrap_or_else(|error| error.into_inner()));
    let mut reported = BTreeMap::new();
    for (key, drain) in retained {
        let (usage, delivery) = retire_observation_drain(handle, drain).await;
        reported.insert(key, usage);
        keep_cleanup_error(failures, delivery);
    }
    // Never move filesystem owners into a local drain: a driver panic must
    // leave every not-yet-transferred producer in supervisor-visible state.
    let recoveries: Vec<_> = owners.values().filter_map(filesystem_owner).collect();
    for owner in recoveries {
        owner.start_recovery(handle).await;
    }
    let retained_ids: Vec<_> = owners.keys().copied().collect();
    for id in retained_ids {
        if let Some(owner) = owners.get(&id).and_then(filesystem_owner) {
            let retired = owner.observe_recovery(handle).await;
            let released = release_filesystem_custody(owners, id, &owner);
            keep_cleanup_error(failures, finish_provider_service_result(retired, released));
            continue;
        }
        let Some(ownership) = owners.remove(&id) else {
            continue;
        };
        match ownership.request {
            Some(OwnedAgentWork::Native(owner)) => {
                keep_cleanup_error(
                    failures,
                    retire_native_owner(handle, owner, ownership.cause).await,
                );
            }
            Some(OwnedAgentWork::Provider(owner)) => {
                let usage = reported.remove(&owner.key()).unwrap_or_default();
                let closed = handle.lifecycle_state_watcher().has_changed().is_err();
                if !closed || usage.known_observations != 0 {
                    keep_cleanup_error(
                        failures,
                        submit_authoritative_runtime_event(
                            handle,
                            owner.event(ownership.cause, usage.usage),
                        )
                        .await,
                    );
                }
            }
            Some(OwnedAgentWork::Filesystem(producer)) => {
                // The serialized map was checked above; retain rather than
                // discard if a future owner variant changes this drain shape.
                owners.insert(
                    id,
                    WorkerOwnership {
                        request: Some(OwnedAgentWork::Filesystem(producer)),
                        cause: ownership.cause,
                    },
                );
            }
            None => {}
        }
    }
    match failures.first.as_ref() {
        Some(error) => Err(error.clone().with_context(format!(
            "provider worker cleanup incomplete; additional_failures={}",
            failures.additional
        ))),
        None => Ok(report.executions),
    }
}

/// Accumulates bounded cleanup uncertainty while continuing sibling retirement.
fn keep_cleanup_error(failures: &mut ProviderCleanupFailures, result: Result<()>) {
    if let Err(error) = result {
        if failures.first.is_none() {
            failures.first = Some(error);
        } else {
            failures.additional = failures.additional.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ownership-corruption, fatal-monitor and primary-error retirement controls.

    use super::*;
    use crate::error::MezErrorKind;
    use crate::runtime::RuntimeLifecycleState;

    /// Missing retained identity must fail even for a nominally successful task;
    /// optional cleanup cannot turn corrupted ownership into a silent success.
    #[tokio::test]
    async fn missing_provider_worker_identity_remains_fatal() {
        let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        let (handle, _actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
                .unwrap();
        let mut workers = JoinSet::new();
        workers.spawn(async { Ok(None) });
        let joined = workers.join_next_with_id().await.unwrap();
        let mut report = AsyncAgentProviderPollReport {
            polls: 0,
            executions: 0,
            idle_polls: 0,
            terminal_state: RuntimeLifecycleState::Running,
        };
        let error = record_owned_agent_provider_worker(
            joined,
            &handle,
            &mut report,
            &mut Default::default(),
            &Default::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::InvalidState);
        assert!(error.message().contains("no retained ownership"));
    }

    /// A monitor panic still fails its provider service and retains identity for
    /// cleanup. Neither its arbitrary payload nor a fake task-local success may
    /// replace that primary failure while sibling owners are drained.
    #[tokio::test]
    async fn provider_monitor_panic_is_fatal_and_payload_free() {
        let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        let (handle, _actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
                .unwrap();
        let mut workers = JoinSet::new();
        let mut owners = AgentProviderWorkerOwners::default();
        let drains: ProviderObservationDrains = Default::default();
        spawn_owned_agent_provider_worker(&mut workers, &mut owners, None, async {
            panic!("private monitor panic payload");
        });
        let joined = workers.join_next_with_id().await.unwrap();
        let mut report = AsyncAgentProviderPollReport {
            polls: 0,
            executions: 0,
            idle_polls: 0,
            terminal_state: RuntimeLifecycleState::Running,
        };
        let error =
            record_owned_agent_provider_worker(joined, &handle, &mut report, &mut owners, &drains)
                .await
                .unwrap_err();
        assert_eq!(
            error.message(),
            "async agent provider worker task failed: panic"
        );
        assert_eq!(owners.len(), 1);
        assert_eq!(
            retire_agent_provider_workers(
                &mut workers,
                &handle,
                &drains,
                &mut owners,
                &mut ProviderCleanupFailures::default()
            )
            .await
            .unwrap(),
            0
        );
        assert!(owners.is_empty());
    }

    /// Cleanup failure adds bounded uncertainty while the primary classification
    /// and typed provider evidence remain intact. Secondary arbitrary text must
    /// not be copied over the original causal failure.
    #[test]
    fn provider_cleanup_preserves_primary_error_and_typed_facts() {
        let primary = MezError::conflict("original primary cause")
            .with_provider_failure_json(r#"{"code":"original"}"#);
        let cleanup = MezError::forbidden("private secondary diagnostic");
        let error = finish_provider_service_result::<()>(Err(primary), Err(cleanup)).unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::Conflict);
        assert!(error.message().contains("original primary cause"));
        assert!(error.message().contains("secondary_kind=Forbidden"));
        assert!(!error.message().contains("private secondary diagnostic"));
        assert_eq!(
            error.provider_failure_json(),
            Some(r#"{"code":"original"}"#)
        );
    }
}
