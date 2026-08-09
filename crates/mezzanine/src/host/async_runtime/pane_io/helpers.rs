//! Shared pane worker, supervisor, and side-effect helpers.

use super::{
    AsyncPaneProcessDriver, AsyncPaneProcessDriverConfig, AsyncPaneProcessIo,
    AsyncPaneProcessServiceConfig, AsyncPaneProcessServiceReport,
    AsyncPaneProcessSupervisorServiceReport, AsyncPtyPaneProcessIo, AsyncRuntimeSessionHandle,
    Duration, HashSet, JoinSet, MezError, PaneEvent, PaneProcessEvent, PaneProcessInstance,
    PaneProcessIoEffect, ProcessEvent, Result, RuntimeEvent, RuntimeEventBatch,
    RuntimeLifecycleState, RuntimeSideEffect, VecDeque, is_terminal_runtime_lifecycle_state,
    run_async_pane_process_service, sleep, watch,
};

/// Number of fresh PTY foreground queries allowed after bootstrap completion.
const FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS: usize = 50;

/// Delay between foreground queries while the isolated child relinquishes the PTY.
const FOREGROUND_CERTIFICATION_OBSERVATION_DELAY: Duration = Duration::from_millis(10);

/// Submits one pane-produced runtime event and accumulates ingress counters.
pub(super) async fn submit_pane_runtime_event(
    handle: &AsyncRuntimeSessionHandle,
    event: RuntimeEvent,
    submitted_events: &mut usize,
    applied_events: &mut usize,
) -> Result<()> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(event);
    let ingress = handle.submit_runtime_events(batch).await?;
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

/// Runs the spawn owned pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn spawn_owned_pane_process_worker(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>>,
    handle: AsyncRuntimeSessionHandle,
    instance: PaneProcessInstance,
    process: super::PaneProcess,
    config: AsyncPaneProcessServiceConfig,
) -> Result<()> {
    let pane_id = instance.pane_id.clone();
    let backend = AsyncPtyPaneProcessIo::new(pane_id.clone(), process)?;
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        backend,
        AsyncPaneProcessDriverConfig::default(),
    )?;
    workers.spawn(async move {
        let mut driver = driver;
        let report = run_async_pane_process_service(&handle, &mut driver, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok((instance, report))
    });
    Ok(())
}

/// Runs the drain completed pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn drain_completed_pane_process_workers(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    while let Some(joined) = workers.try_join_next() {
        record_joined_pane_process_worker(joined, active_panes, report)?;
    }
    Ok(())
}

/// Runs the drain completed pane process workers after yields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn drain_completed_pane_process_workers_after_yields(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    for _ in 0..16 {
        drain_completed_pane_process_workers(workers, active_panes, report)?;
        if workers.is_empty() {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    drain_completed_pane_process_workers(workers, active_panes, report)
}

/// Runs the record joined pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn record_joined_pane_process_worker(
    joined: std::result::Result<
        Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>,
        tokio::task::JoinError,
    >,
    active_panes: &mut HashSet<PaneProcessInstance>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    match joined {
        Ok(Ok((instance, worker_report))) => {
            active_panes.remove(&instance);
            report.terminal_state = worker_report.terminal_state;
            report.completed_workers = report.completed_workers.saturating_add(1);
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => Ok(()),
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
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    bounded_idle: Option<Duration>,
) -> Option<
    std::result::Result<
        Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>,
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

/// Runs the abort pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn abort_pane_process_workers(
    workers: &mut JoinSet<Result<(PaneProcessInstance, AsyncPaneProcessServiceReport)>>,
) {
    workers.abort_all();
    while workers.join_next().await.is_some() {}
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
    paced_input_requires_output: &mut bool,
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
/// A missing expected group captures the first live foreground process at the
/// start boundary. At completion, output is written by an isolated child, so
/// the worker performs fresh observations until the start-captured receiver
/// group reappears or the bound expires.
async fn correlated_foreground_process_observation_event<B>(
    driver: &mut AsyncPaneProcessDriver<B>,
    observation_id: String,
    expected_process_group_id: Option<u32>,
) -> RuntimeEvent
where
    B: AsyncPaneProcessIo,
{
    let mut last_metadata = None;
    for attempt in 0..FOREGROUND_CERTIFICATION_OBSERVATION_ATTEMPTS {
        match driver.foreground_process_observation().await {
            Ok(metadata) => {
                let matched = metadata.as_ref().is_some_and(|metadata| {
                    expected_process_group_id
                        .is_none_or(|expected| metadata.process_group_id == expected)
                });
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
    driver.foreground_process_observation_event(observation_id, last_metadata, None)
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
            process_name: "setsid".to_string(),
            process_group_id: 22,
            current_working_directory: None,
        })));
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: "sh".to_string(),
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

    /// Verifies start-boundary capture returns the first fresh PTY observation.
    ///
    /// Start capture has no expected group because it establishes the receiver
    /// identity used by completion certification. It must query the backend
    /// instead of consulting periodic runtime metadata.
    #[tokio::test(flavor = "current_thread")]
    async fn correlated_foreground_observation_captures_first_fresh_group() {
        let instance = PaneProcessInstance {
            pane_id: "%1".to_string(),
            generation: 8,
        };
        let mut backend = AsyncFakePaneProcessIo::default();
        backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
            process_name: "bash".to_string(),
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
