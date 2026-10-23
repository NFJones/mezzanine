//! Shared pane worker, supervisor, and side-effect helpers.

use super::{
    AsyncPaneProcessDriver, AsyncPaneProcessDriverConfig, AsyncPaneProcessIo,
    AsyncPaneProcessServiceConfig, AsyncPaneProcessSupervisorServiceReport,
    AsyncPaneProcessWorkerOutcome, AsyncPtyPaneProcessIo, AsyncRuntimeSessionHandle, Duration,
    HashSet, JoinSet, MezError, PaneEvent, PaneProcessEvent, PaneProcessInstance,
    PaneProcessIoEffect, ProcessEvent, Result, RuntimeEvent, RuntimeEventBatch,
    RuntimeLifecycleState, RuntimeSideEffect, VecDeque, is_terminal_runtime_lifecycle_state, sleep,
    watch,
};

/// Number of fresh PTY foreground queries allowed after bootstrap completion.
/// Leave headroom beneath the runtime-owned two-second certification deadline.
const FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS: usize = 150;

/// Delay between foreground queries while the isolated child relinquishes the PTY.
const FOREGROUND_CERTIFICATION_OBSERVATION_DELAY: Duration = Duration::from_millis(10);

/// Bounded attempts for one pane-produced event batch rejected before
/// application because unrelated effects transiently filled the queue.
const PANE_EVENT_CAPACITY_RETRY_ATTEMPTS: usize = 32;

/// Delay between bounded pane-event capacity retries.
const PANE_EVENT_CAPACITY_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Submits one pane-side event batch, retrying only pre-application capacity
/// pressure so a transient backlog cannot abort a pane service.
pub(crate) async fn submit_pane_runtime_events_bounded(
    handle: &AsyncRuntimeSessionHandle,
    batch: RuntimeEventBatch,
) -> Result<super::RuntimeEventIngressReport> {
    let mut attempts = 0usize;
    loop {
        match handle.submit_runtime_events(batch.clone()).await {
            Ok(ingress) => return Ok(ingress),
            Err(error)
                if attempts < PANE_EVENT_CAPACITY_RETRY_ATTEMPTS
                    && crate::host::async_runtime::error_is_retryable_side_effect_capacity(
                        &error,
                    ) =>
            {
                attempts = attempts.saturating_add(1);
                sleep(PANE_EVENT_CAPACITY_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Submits one pane-produced runtime event and accumulates ingress counters.
pub(super) async fn submit_pane_runtime_event(
    handle: &AsyncRuntimeSessionHandle,
    event: RuntimeEvent,
    submitted_events: &mut usize,
    applied_events: &mut usize,
) -> Result<()> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(event);
    // A pane worker must not fail because unrelated effects transiently filled
    // the bounded side-effect queue; only pre-application rejections retry.
    let ingress = submit_pane_runtime_events_bounded(handle, batch).await?;
    *submitted_events = submitted_events.saturating_add(ingress.accepted);
    *applied_events = applied_events.saturating_add(ingress.applied);
    Ok(())
}

/// Returns whether an event reports a terminal pane process exit.
pub(super) fn is_process_exit_event(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::Process(ProcessEvent::Exited { .. })
            | RuntimeEvent::PaneProcess {
                event: PaneProcessEvent::Process(ProcessEvent::Exited { .. }),
                ..
            }
    )
}

/// Requires native termination success, never merely a Failed event.
pub(super) fn require_pane_retirement_event(event: &RuntimeEvent) -> Result<()> {
    if is_process_exit_event(event) {
        return Ok(());
    }
    let diagnostic = match event {
        RuntimeEvent::Process(ProcessEvent::Failed { error, .. })
        | RuntimeEvent::PaneProcess {
            event: PaneProcessEvent::Process(ProcessEvent::Failed { error, .. }),
            ..
        } => error.as_str(),
        _ => "backend did not acknowledge native termination",
    };
    Err(MezError::invalid_state(format!(
        "pane native retirement failed: {diagnostic}"
    )))
}

/// Runs the spawn owned pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn spawn_owned_pane_process_worker(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>>,
    handle: AsyncRuntimeSessionHandle,
    instance: PaneProcessInstance,
    process: super::PaneProcess,
    config: AsyncPaneProcessServiceConfig,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let pane_id = instance.pane_id.clone();
    let primary_pid = process.primary_pid();
    workers.spawn(async move {
        let backend = match AsyncPtyPaneProcessIo::new(pane_id, process) {
            Ok(backend) => backend,
            Err(error) => {
                super::retirement::drain_returned_native_panes(
                    &handle.native_pane_retirement,
                    super::Instant::now() + super::NATIVE_PANE_RETIREMENT_TIMEOUT,
                )
                .await?;
                retire_failed_pane_process(&handle, &instance, primary_pid, &error).await?;
                return Ok((instance, AsyncPaneProcessWorkerOutcome::Failed));
            }
        };
        let driver = match AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        ) {
            Ok(driver) => driver,
            Err(error) => {
                super::retirement::drain_returned_native_panes(
                    &handle.native_pane_retirement,
                    super::Instant::now() + super::NATIVE_PANE_RETIREMENT_TIMEOUT,
                )
                .await?;
                retire_failed_pane_process(&handle, &instance, primary_pid, &error).await?;
                return Ok((instance, AsyncPaneProcessWorkerOutcome::Failed));
            }
        };
        run_owned_pane_process_worker_until(
            handle,
            instance,
            primary_pid,
            driver,
            config,
            Some(stop),
        )
        .await
    });
    Ok(())
}

/// Runs one pane worker and applies its pane-local failure policy.
#[cfg(test)]
pub(crate) async fn run_owned_pane_process_worker<B>(
    handle: AsyncRuntimeSessionHandle,
    instance: PaneProcessInstance,
    primary_pid: u32,
    driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
) -> Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    run_owned_pane_process_worker_until(handle, instance, primary_pid, driver, config, None).await
}

/// Stops I/O cooperatively, then settles the exact backend before returning it.
async fn run_owned_pane_process_worker_until<B>(
    handle: AsyncRuntimeSessionHandle,
    instance: PaneProcessInstance,
    primary_pid: u32,
    mut driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
    stop: Option<watch::Receiver<bool>>,
) -> Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    let result = super::process_service::run_async_pane_process_service_until(
        &handle,
        &mut driver,
        config,
        |_, state| is_terminal_runtime_lifecycle_state(state),
        stop,
    )
    .await;
    // Normal/bounded stops preserve graceful policy; failure permits force.
    let termination = driver.terminate_event(result.is_err()).await;
    if let Err(cleanup) = require_pane_retirement_event(&termination) {
        return Err(match result {
            Err(primary) => {
                MezError::new(primary.kind(), format!("{}; {cleanup}", primary.message()))
            }
            Ok(_) => cleanup,
        });
    }
    match result {
        Ok(report) => Ok((instance, AsyncPaneProcessWorkerOutcome::Completed(report))),
        Err(error) if is_terminal_pane_supervisor_error(&error) => Err(error),
        Err(error) if error.is_pane_backend_failure() => {
            drop(driver);
            retire_failed_pane_process(&handle, &instance, primary_pid, &error).await?;
            Ok((instance, AsyncPaneProcessWorkerOutcome::Failed))
        }
        Err(error) => Err(error),
    }
}

/// Reports and closes the exact pane generation whose I/O worker failed.
///
/// A failed pane backend cannot safely be returned to the manager because its
/// PTY/process state may be unusable. The actor first records a pane diagnostic,
/// then applies normal exit cleanup; if actor ingress is unavailable, the error
/// remains fatal to the supervisor rather than being silently discarded.
pub(super) async fn retire_failed_pane_process(
    handle: &AsyncRuntimeSessionHandle,
    instance: &PaneProcessInstance,
    primary_pid: u32,
    error: &MezError,
) -> Result<()> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::PaneProcess {
        instance: instance.clone(),
        event: PaneProcessEvent::Process(ProcessEvent::Failed {
            pane_id: instance.pane_id.clone(),
            error: error.to_string(),
        }),
    });
    batch.push(RuntimeEvent::PaneProcess {
        instance: instance.clone(),
        event: PaneProcessEvent::Process(ProcessEvent::Exited {
            pane_id: instance.pane_id.clone(),
            primary_pid: Some(primary_pid),
            exit_code: None,
            signal: Some("pane worker failed".to_string()),
        }),
    });
    let mut attempts = 0usize;
    loop {
        match handle.submit_runtime_events(batch.clone()).await {
            Ok(_ingress) => return Ok(()),
            Err(error)
                if attempts < PANE_EVENT_CAPACITY_RETRY_ATTEMPTS
                    && crate::host::async_runtime::error_is_retryable_side_effect_capacity(
                        &error,
                    ) =>
            {
                attempts = attempts.saturating_add(1);
                sleep(PANE_EVENT_CAPACITY_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Runs the drain completed pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn drain_completed_pane_process_workers(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>>,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    while let Some(joined) = workers.try_join_next() {
        record_joined_pane_process_worker(joined, active_panes, report)?;
    }
    Ok(())
}

/// Observes all consuming worker outcomes within the existing native grace.
/// A deadline failure cancels workers only to return exact child guards to the
/// retained session owner; it is an error, never a native retirement receipt.
pub(super) async fn drain_pane_process_workers(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>>,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    let deadline = super::Instant::now() + Duration::from_secs(2);
    let mut first_error = None;
    while !workers.is_empty() {
        match tokio::time::timeout_at(deadline, workers.join_next()).await {
            Ok(Some(joined)) => {
                if let Err(error) = record_joined_pane_process_worker(joined, active_panes, report)
                {
                    first_error.get_or_insert(error);
                }
            }
            Ok(None) => break,
            Err(_) => {
                workers.abort_all();
                let pending = MezError::invalid_state(
                    "pane workers did not settle; exact native cleanup remains retained by session owner",
                );
                return Err(match first_error {
                    Some(primary) => {
                        MezError::new(primary.kind(), format!("{}; {pending}", primary.message()))
                    }
                    None => pending,
                });
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Runs the record joined pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn record_joined_pane_process_worker(
    joined: std::result::Result<
        Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>,
        tokio::task::JoinError,
    >,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    match joined {
        Ok(Ok((instance, outcome))) => {
            active_panes.remove(&instance);
            match outcome {
                AsyncPaneProcessWorkerOutcome::Completed(worker_report) => {
                    report.terminal_state = worker_report.terminal_state;
                    report.completed_workers = report.completed_workers.saturating_add(1);
                }
                AsyncPaneProcessWorkerOutcome::Failed => {
                    report.failed_workers = report.failed_workers.saturating_add(1);
                }
            }
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => Err(MezError::invalid_state(
            "pane worker was cancelled without a native completion receipt; exact cleanup remains retained",
        )),
        Err(error) => Err(MezError::invalid_state(format!(
            "async pane process worker task failed: {error}"
        ))),
    }
}

/// Runs the wait for pane process supervisor wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn wait_for_pane_process_supervisor_wakeup(
    handle: &AsyncRuntimeSessionHandle,
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    bounded_idle: Option<Duration>,
) -> Option<
    std::result::Result<
        Result<(PaneProcessInstance, AsyncPaneProcessWorkerOutcome)>,
        tokio::task::JoinError,
    >,
> {
    match (workers.is_empty(), bounded_idle) {
        (true, Some(idle_interval)) => {
            tokio::select! {
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
                _ = sleep(idle_interval) => None,
            }
        }
        (true, None) => {
            tokio::select! {
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
            }
        }
        (false, Some(idle_interval)) => {
            tokio::select! {
                biased;
                joined = workers.join_next() => joined,
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
                _ = sleep(idle_interval) => None,
            }
        }
        (false, None) => {
            tokio::select! {
                biased;
                joined = workers.join_next() => joined,
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
            }
        }
    }
}

/// Runs the is terminal pane supervisor error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_terminal_pane_supervisor_error(error: &MezError) -> bool {
    error.kind() == crate::error::MezErrorKind::InvalidState
        && matches!(
            error.message(),
            "runtime service is stopping"
                | "runtime service has already been killed"
                | "runtime service is in a failed lifecycle state"
        )
}

/// Drains locally deferred pane I/O side effects before actor-queued work.
///
/// Locally deferred effects preserve byte order for large input writes that
/// were split across service polls. They must run before newly drained actor
/// effects so a later keystroke cannot overtake a remaining paste chunk.
pub(super) fn drain_pending_pane_io_side_effects(
    pending: &mut VecDeque<RuntimeSideEffect>,
    limit: usize,
) -> Vec<RuntimeSideEffect> {
    let mut effects = Vec::new();
    while effects.len() < limit {
        let Some(effect) = pending.pop_front() else {
            break;
        };
        effects.push(effect);
    }
    effects
}

/// Runs the pane io events for side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn pane_io_events_for_side_effects<B>(
    driver: &mut AsyncPaneProcessDriver<B>,
    effects: Vec<RuntimeSideEffect>,
    pending: &mut VecDeque<RuntimeSideEffect>,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    paced_input_requires_output: &mut bool,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    paced_input_requires_ack: &mut bool,
) -> Vec<RuntimeEvent>
where
    B: AsyncPaneProcessIo,
{
    let mut events = Vec::new();
    let mut effects: VecDeque<_> = effects.into();
    while let Some(effect) = effects.pop_front() {
        let event = match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::WriteShellInput { delivery },
            } => {
                if let Err(error) = delivery.validate_logical_records() {
                    events.push(
                        driver.scope_event(RuntimeEvent::Pane(PaneEvent::WriteFailed {
                            pane_id: instance.pane_id,
                            error: format!("InvalidState: {error}"),
                        })),
                    );
                    continue;
                }
                #[cfg(target_os = "macos")]
                let generated_source = matches!(
                    delivery.pacing,
                    mez_mux::process::ShellInputPacing::GeneratedSource
                );
                let bytes = &delivery.bytes;
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = pane_input_chunk_len(bytes);
                #[cfg(target_os = "macos")]
                let supports_acknowledgements = driver.supports_shell_input_acknowledgements();
                let event = driver.write_input_event(&bytes[..chunk_len]).await;
                if let Some(written) = pane_input_written_bytes(&event)
                    && written > 0
                    && written < bytes.len()
                {
                    let mut remaining_delivery = delivery.clone();
                    remaining_delivery.bytes = bytes[written..].to_vec();
                    let existing_pending = std::mem::take(pending);
                    pending.push_back(RuntimeSideEffect::PaneProcessIo {
                        instance,
                        effect: PaneProcessIoEffect::WriteShellInput {
                            delivery: remaining_delivery,
                        },
                    });
                    pending.extend(effects);
                    pending.extend(existing_pending);
                    #[cfg(target_os = "macos")]
                    if supports_acknowledgements && generated_source {
                        *paced_input_requires_output = true;
                        *paced_input_requires_ack =
                            mez_mux::process::shell_input_record_requires_ack(&bytes[..written]);
                    }
                    events.push(event);
                    break;
                }
                event
            }
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    PaneProcessIoEffect::WriteInput { bytes }
                    | PaneProcessIoEffect::WriteInputPriority { bytes },
            } => {
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = pane_input_chunk_len(&bytes);
                let event = driver.write_input_event(&bytes[..chunk_len]).await;
                if let Some(written) = pane_input_written_bytes(&event)
                    && written > 0
                    && written < bytes.len()
                {
                    let existing_pending = std::mem::take(pending);
                    pending.push_back(RuntimeSideEffect::PaneProcessIo {
                        instance,
                        effect: PaneProcessIoEffect::WriteInput {
                            bytes: bytes[written..].to_vec(),
                        },
                    });
                    pending.extend(effects);
                    pending.extend(existing_pending);
                    events.push(event);
                    break;
                }
                event
            }
            RuntimeSideEffect::PaneProcessIo {
                effect: PaneProcessIoEffect::CancelShellInput { .. },
                ..
            } => continue,
            RuntimeSideEffect::PaneProcessIo {
                effect: PaneProcessIoEffect::Resize { size },
                ..
            } => driver.resize_event(size).await,
            RuntimeSideEffect::PaneProcessIo {
                effect:
                    PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id,
                    },
                ..
            } => {
                correlated_foreground_process_observation_event(
                    driver,
                    observation_id,
                    expected_process_group_id,
                )
                .await
            }
            RuntimeSideEffect::PaneProcessIo {
                effect: PaneProcessIoEffect::Terminate { force },
                ..
            } => driver.terminate_event(force).await,
            RuntimeSideEffect::WritePaneInput { pane_id, bytes }
            | RuntimeSideEffect::WritePaneInputPriority { pane_id, bytes } => {
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = pane_input_chunk_len(&bytes);
                let event = driver.write_input_event(&bytes[..chunk_len]).await;
                if let RuntimeEvent::Pane(PaneEvent::InputWritten { bytes: written, .. }) = &event
                    && *written > 0
                    && *written < bytes.len()
                {
                    let existing_pending = std::mem::take(pending);
                    pending.push_back(RuntimeSideEffect::WritePaneInput {
                        pane_id,
                        bytes: bytes[*written..].to_vec(),
                    });
                    pending.extend(effects);
                    pending.extend(existing_pending);
                    events.push(event);
                    break;
                }
                event
            }
            RuntimeSideEffect::WritePaneShellInput { pane_id, delivery } => {
                if let Err(error) = delivery.validate_logical_records() {
                    events.push(
                        driver.scope_event(RuntimeEvent::Pane(PaneEvent::WriteFailed {
                            pane_id,
                            error: format!("InvalidState: {error}"),
                        })),
                    );
                    continue;
                }
                let bytes = &delivery.bytes;
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = pane_input_chunk_len(bytes);
                let event = driver.write_input_event(&bytes[..chunk_len]).await;
                if let RuntimeEvent::Pane(PaneEvent::InputWritten { bytes: written, .. }) = &event
                    && *written > 0
                    && *written < bytes.len()
                {
                    let mut remaining_delivery = delivery.clone();
                    remaining_delivery.bytes = bytes[*written..].to_vec();
                    let existing_pending = std::mem::take(pending);
                    pending.push_back(RuntimeSideEffect::WritePaneShellInput {
                        pane_id,
                        delivery: remaining_delivery,
                    });
                    pending.extend(effects);
                    pending.extend(existing_pending);
                    events.push(event);
                    break;
                }
                event
            }
            RuntimeSideEffect::ResizePane { size, .. } => driver.resize_event(size).await,
            RuntimeSideEffect::TerminatePane { force, .. } => driver.terminate_event(force).await,
            _ => continue,
        };
        events.push(event);
    }
    events
}

/// Selects one bounded pane-input chunk without splitting a complete shell
/// record when a newline is available.
///
/// Darwin PTYs can stop reporting write readiness when a bulk write leaves a
/// partial canonical-input record at the buffer boundary. On macOS, returning
/// one complete record at a time also gives the interactive shell an actor
/// scheduling boundary between generated commands. Other hosts retain the
/// higher-throughput last-record behavior. Inputs with no newline still make
/// bounded byte progress, preserving ordinary keystroke and binary delivery.
fn pane_input_chunk_len(bytes: &[u8]) -> usize {
    let limit = bytes
        .len()
        .min(mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES);
    let bounded = &bytes[..limit];
    #[cfg(target_os = "macos")]
    let newline = bounded.iter().position(|byte| *byte == b'\n');
    #[cfg(not(target_os = "macos"))]
    let newline = bounded.iter().rposition(|byte| *byte == b'\n');
    newline.map_or(limit, |index| index + 1)
}

/// Performs one fresh start capture or bounded completion observation.
///
/// At completion, output is written by an isolated child, so the worker polls
/// until the start-captured receiver group reappears. The polling bound leaves
/// headroom for the runtime-owned certification deadline. An unmatched final
/// sample is reported as a failure, never as successful foreground evidence.
async fn correlated_foreground_process_observation_event<B>(
    driver: &mut AsyncPaneProcessDriver<B>,
    observation_id: String,
    expected_process_group_id: Option<u32>,
) -> RuntimeEvent
where
    B: AsyncPaneProcessIo,
{
    let mut last_metadata = None;
    let mut previous_group_id: Option<u32> = None;
    for attempt in 0..FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS {
        match driver.foreground_process_observation().await {
            Ok(metadata) => {
                let matched = match expected_process_group_id {
                    Some(expected) => metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.process_group_id == expected),
                    // A start capture has no expectation yet. A job-controlling
                    // shell can run the bootstrap body as its own short-lived
                    // foreground job group, so accepting the first sample would
                    // record a group that is gone by the time completion waits
                    // for it. Require the same group across two consecutive
                    // samples instead, which a transient job group cannot
                    // satisfy but the persistent receiver group does.
                    None => metadata.as_ref().is_some_and(|metadata| {
                        previous_group_id == Some(metadata.process_group_id)
                    }),
                };
                previous_group_id = metadata.as_ref().map(|metadata| metadata.process_group_id);
                last_metadata = metadata;
                if matched {
                    return driver.foreground_process_observation_event(
                        observation_id,
                        last_metadata,
                        None,
                    );
                }
            }
            Err(error) => {
                return driver.foreground_process_observation_event(
                    observation_id,
                    last_metadata,
                    Some(error.to_string()),
                );
            }
        }
        if attempt + 1 < FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS {
            sleep(FOREGROUND_CERTIFICATION_OBSERVATION_DELAY).await;
        }
    }
    let error = match expected_process_group_id {
        Some(expected) => format!(
            "foreground process group {expected} was not observed within {} ms",
            FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS.saturating_sub(1)
                * FOREGROUND_CERTIFICATION_OBSERVATION_DELAY.as_millis() as usize
        ),
        None => "foreground process group did not remain stable across observations".to_string(),
    };
    driver.foreground_process_observation_event(observation_id, None, Some(error))
}

/// Returns the accepted byte count from legacy or instance-scoped write events.
pub(super) fn pane_input_written_bytes(event: &RuntimeEvent) -> Option<usize> {
    match event {
        RuntimeEvent::Pane(PaneEvent::InputWritten { bytes, .. })
        | RuntimeEvent::PaneProcess {
            event: PaneProcessEvent::Pane(PaneEvent::InputWritten { bytes, .. }),
            ..
        } => Some(*bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::async_runtime::{
        AsyncFakePaneProcessIo, AsyncPaneForegroundProcess, AsyncPaneProcessDriverConfig,
        PaneForegroundProcessObservation,
    };

    /// A scheduler-yield budget is not a native retirement acknowledgement.
    ///
    /// This real PTY child belongs exclusively to the worker below. A gate
    /// deliberately prevents its termination/reap operation from completing
    /// while the production supervisor drain is polled. The drain must remain
    /// pending, then observe the joined native completion after release. The
    /// test always releases and joins its fixture before evaluating the early
    /// completion assertion; that fixture settlement cannot turn an early
    /// successful product return into a passing regression.
    #[tokio::test]
    async fn pane_worker_drain_waits_for_gated_native_reap() {
        use rustix::process::{Pid, test_kill_process};
        use tokio::sync::oneshot;

        let process = mez_mux::process::spawn_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            super::super::Size::new(80, 24).unwrap(),
            None,
        )
        .unwrap();
        let pid = Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        // The unreaped exact Child handle fences PID reuse throughout the gate.
        // Linux additionally records the kernel incarnation witness for useful
        // failure diagnostics; Darwin retains the exact Child/PID witness.
        #[cfg(target_os = "linux")]
        let start_witness =
            std::fs::read_to_string(format!("/proc/{}/stat", pid.as_raw_pid())).unwrap();
        #[cfg(not(target_os = "linux"))]
        let start_witness = format!("owned portable-pty child {}", pid.as_raw_pid());
        assert_eq!(test_kill_process(pid), Ok(()), "{start_witness}");
        let instance = PaneProcessInstance {
            pane_id: "%gated-native".to_string(),
            generation: 1,
        };
        let mut active = HashSet::from([instance.clone()]);
        let mut report =
            AsyncPaneProcessSupervisorServiceReport::new(RuntimeLifecycleState::Running);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let mut workers = JoinSet::new();
        workers.spawn(async move {
            let mut backend = AsyncPtyPaneProcessIo::new("%gated-native", process)?;
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            backend.terminate(true).await?;
            Ok((
                instance,
                AsyncPaneProcessWorkerOutcome::Completed(
                    super::super::AsyncPaneProcessServiceReport::new(RuntimeLifecycleState::Killed),
                ),
            ))
        });
        entered_rx.await.unwrap();

        let (early, completed, alive_while_gated) = {
            let drain = drain_pane_process_workers(&mut workers, &mut active, &mut report);
            tokio::pin!(drain);
            let early = tokio::time::timeout(Duration::from_millis(30), &mut drain).await;
            let alive_while_gated = test_kill_process(pid);
            release_tx.send(()).unwrap();
            // Keep the same operation alive: a permanently pending drain must
            // not pass merely because the first observation timed out.
            let completed = if early.is_err() {
                Some(tokio::time::timeout(Duration::from_secs(2), &mut drain).await)
            } else {
                None
            };
            (early, completed, alive_while_gated)
        };
        while let Some(joined) = workers.join_next().await {
            record_joined_pane_process_worker(joined, &mut active, &mut report).unwrap();
        }
        let immediately_after_completed_reap = test_kill_process(pid);

        assert!(
            early.is_err(),
            "drain reported completion before native termination/reap: {early:?}; \
             alive_while_gated={alive_while_gated:?}; \
             immediate_after_fixture_reap={immediately_after_completed_reap:?}; \
             {start_witness}"
        );
        assert!(
            matches!(completed, Some(Ok(Ok(())))),
            "the same product drain must observe completion after release: {completed:?}"
        );
        assert_eq!(alive_while_gated, Ok(()), "{start_witness}");
        assert_eq!(
            immediately_after_completed_reap,
            Err(rustix::io::Errno::SRCH),
            "completed native operation must already have reaped its exact child: {start_witness}"
        );
        assert_eq!(report.completed_workers, 1);
        assert!(active.is_empty());
    }

    /// Verifies the compatibility pane-I/O owner rejects an oversized shell
    /// record before invoking its backend and reports only framing metadata.
    #[tokio::test(flavor = "current_thread")]
    async fn compatibility_shell_delivery_rejects_oversized_record_before_write() {
        let backend = AsyncFakePaneProcessIo::default();
        let mut driver =
            AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
                .unwrap();
        let mut bytes = b"private-generated-source:".to_vec();
        bytes.resize(mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES + 1, b'x');
        let effects = vec![RuntimeSideEffect::WritePaneShellInput {
            pane_id: "%1".to_string(),
            delivery: mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                bytes,
                "delivery-1",
            ),
        }];
        let mut pending = VecDeque::new();

        let events = pane_io_events_for_side_effects(
            &mut driver,
            effects,
            &mut pending,
            &mut false,
            &mut false,
        )
        .await;

        assert!(driver.into_backend().writes.is_empty());
        assert!(pending.is_empty());
        assert_eq!(events.len(), 1);
        let RuntimeEvent::Pane(PaneEvent::WriteFailed { error, .. }) = &events[0] else {
            panic!("oversized shell delivery must emit a write failure");
        };
        assert!(error.contains("record_len=1025"), "{error}");
        assert!(!error.contains("private-generated-source"), "{error}");
    }

    #[test]
    /// Verifies pane-input chunks preserve an appropriate complete shell record.
    ///
    /// Generated wrapper transports contain many short records. Keeping the
    /// macOS emits one record per actor turn to pace its constrained terminal
    /// input path; other hosts retain the final bounded record for throughput.
    fn pane_input_chunks_end_at_a_platform_appropriate_bounded_newline() {
        let mut bytes = vec![b'a'; mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES + 20];
        bytes[700] = b'\n';
        bytes[900] = b'\n';

        #[cfg(target_os = "macos")]
        assert_eq!(pane_input_chunk_len(&bytes), 701);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(pane_input_chunk_len(&bytes), 901);
    }

    #[test]
    /// Verifies newline-free pane input still advances by the bounded limit.
    ///
    /// Direct terminal keystrokes and binary protocol input need not contain a
    /// shell record terminator, so newline-aware chunking must retain the prior
    /// fixed-size fallback instead of waiting indefinitely for a delimiter.
    fn pane_input_chunks_bound_newline_free_input() {
        let bytes = vec![b'a'; mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES + 20];

        assert_eq!(
            pane_input_chunk_len(&bytes),
            mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES
        );
    }

    /// Verifies the explicit observation ignores a transient child group and
    /// returns only after the persistent receiver group becomes foreground.
    ///
    /// This models the real bootstrap race in which the end marker is consumed
    /// while the isolated `setsid` child still owns the PTY, followed by the
    /// agent subshell regaining it on the next host observation.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_waits_for_expected_group() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 7,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("setsid".to_string()),
            process_group_id: 22,
            current_working_directory: None,
        })));
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("sh".to_string()),
            process_group_id: 11,
            current_working_directory: None,
        })));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-1".to_string(),
            Some(11),
        )
        .await;

        assert_eq!(
            event,
            RuntimeEvent::PaneProcess {
                instance,
                event: PaneProcessEvent::ForegroundProcessObservation(
                    PaneForegroundProcessObservation {
                        observation_id: "observation-1".to_string(),
                        process_name: Some("sh".to_string()),
                        process_group_id: Some(11),
                        current_working_directory: None,
                        error: None,
                    }
                ),
            }
        );
    }

    /// Verifies start-boundary capture accepts a stable fresh PTY observation.
    ///
    /// Start capture has no expected group because it establishes the receiver
    /// identity used by completion certification. It must query the backend
    /// instead of consulting periodic runtime metadata, and it must see the same
    /// group across two consecutive samples so a transient job group cannot be
    /// recorded as the receiver identity.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_captures_first_fresh_group() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 8,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("bash".to_string()),
            process_group_id: 41,
            current_working_directory: Some(std::path::PathBuf::from("/tmp")),
        })));
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("bash".to_string()),
            process_group_id: 41,
            current_working_directory: Some(std::path::PathBuf::from("/tmp")),
        })));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-start".to_string(),
            None,
        )
        .await;

        assert_eq!(
            event,
            RuntimeEvent::PaneProcess {
                instance,
                event: PaneProcessEvent::ForegroundProcessObservation(
                    PaneForegroundProcessObservation {
                        observation_id: "observation-start".to_string(),
                        process_name: Some("bash".to_string()),
                        process_group_id: Some(41),
                        current_working_directory: Some("/tmp".to_string()),
                        error: None,
                    }
                ),
            }
        );
    }

    /// Verifies a transient start-boundary group is not captured as receiver identity.
    ///
    /// A job-controlling shell can run the bootstrap body as its own short-lived
    /// foreground job group, so the first fresh sample can belong to a process
    /// that is already gone by completion and would then be waited for in vain.
    /// The capture must accept only a group observed across two consecutive
    /// samples, which the persistent receiver group satisfies and the transient
    /// job group does not.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_ignores_transient_start_group() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 10,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("sh".to_string()),
            process_group_id: 41,
            current_working_directory: None,
        })));
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("zsh".to_string()),
            process_group_id: 42,
            current_working_directory: None,
        })));
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("zsh".to_string()),
            process_group_id: 42,
            current_working_directory: None,
        })));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-start-raced".to_string(),
            None,
        )
        .await;

        assert_eq!(
            event,
            RuntimeEvent::PaneProcess {
                instance,
                event: PaneProcessEvent::ForegroundProcessObservation(
                    PaneForegroundProcessObservation {
                        observation_id: "observation-start-raced".to_string(),
                        process_name: Some("zsh".to_string()),
                        process_group_id: Some(42),
                        current_working_directory: None,
                        error: None,
                    }
                ),
            }
        );
    }

    /// A correlated ownership observation must retain a readable process group
    /// even when the host cannot resolve its display name.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_keeps_group_without_process_name() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 11,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: None,
            process_group_id: 42,
            current_working_directory: None,
        })));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-name-unavailable".to_string(),
            Some(42),
        )
        .await;

        assert_eq!(
            event,
            RuntimeEvent::PaneProcess {
                instance,
                event: PaneProcessEvent::ForegroundProcessObservation(
                    PaneForegroundProcessObservation {
                        observation_id: "observation-name-unavailable".to_string(),
                        process_name: None,
                        process_group_id: Some(42),
                        current_working_directory: None,
                        error: None,
                    }
                ),
            }
        );
    }

    /// Completion must keep waiting when the receiver appears after the old
    /// half-second sampling window, rather than rejecting its valid process group.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_accepts_receiver_after_old_window() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 12,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        for _ in 0..51 {
            backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
                process_name: Some("bootstrap-helper".to_string()),
                process_group_id: 41,
                current_working_directory: None,
            })));
        }
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: Some("agent-shell".to_string()),
            process_group_id: 42,
            current_working_directory: None,
        })));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-delayed-receiver".to_string(),
            Some(42),
        )
        .await;

        assert_eq!(
            event,
            RuntimeEvent::PaneProcess {
                instance,
                event: PaneProcessEvent::ForegroundProcessObservation(
                    PaneForegroundProcessObservation {
                        observation_id: "observation-delayed-receiver".to_string(),
                        process_name: Some("agent-shell".to_string()),
                        process_group_id: Some(42),
                        current_working_directory: None,
                        error: None,
                    }
                ),
            }
        );
    }

    /// Exhausted completion polling must not return the last mismatched group as
    /// apparently successful foreground evidence.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_reports_unmatched_group_timeout() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 13,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        for _ in 0..FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS {
            backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
                process_name: Some("bootstrap-helper".to_string()),
                process_group_id: 41,
                current_working_directory: None,
            })));
        }
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-unmatched".to_string(),
            Some(42),
        )
        .await;

        let RuntimeEvent::PaneProcess {
            instance: observed_instance,
            event: PaneProcessEvent::ForegroundProcessObservation(observation),
        } = event
        else {
            panic!("expected a correlated foreground observation");
        };
        assert_eq!(observed_instance, instance);
        assert_eq!(observation.process_group_id, None);
        assert!(
            observation
                .error
                .as_deref()
                .is_some_and(|error| error.contains("group 42 was not observed")),
            "unmatched foreground metadata must be an explicit failure: {observation:?}"
        );
    }

    /// Verifies a host query failure produces a correlated failure event
    /// instead of dropping the request and leaving bootstrap pending forever.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_reports_backend_failure() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 9,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Err(MezError::invalid_state(
            "foreground query failed",
        )));
        let mut driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();

        let event = correlated_foreground_process_observation_event(
            &mut driver,
            "observation-2".to_string(),
            Some(11),
        )
        .await;

        let RuntimeEvent::PaneProcess {
            instance: observed_instance,
            event:
                PaneProcessEvent::ForegroundProcessObservation(PaneForegroundProcessObservation {
                    observation_id,
                    process_group_id,
                    error,
                    ..
                }),
        } = event
        else {
            panic!("expected a correlated pane-process observation event");
        };
        assert_eq!(observed_instance, instance);
        assert_eq!(observation_id, "observation-2");
        assert_eq!(process_group_id, None);
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("foreground query failed"))
        );
    }
}
