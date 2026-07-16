//! Shared pane worker, supervisor, and side-effect helpers.

use super::*;

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
    matches!(event, RuntimeEvent::Process(ProcessEvent::Exited { .. }))
}

/// Runs the spawn owned pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn spawn_owned_pane_process_worker(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    process: super::PaneProcess,
    config: AsyncPaneProcessServiceConfig,
) -> Result<()> {
    let backend = AsyncPtyPaneProcessIo::new(pane_id.clone(), process)?;
    let driver =
        AsyncPaneProcessDriver::new(&pane_id, backend, AsyncPaneProcessDriverConfig::default())?;
    workers.spawn(async move {
        let mut driver = driver;
        let report = run_async_pane_process_service(&handle, &mut driver, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok((pane_id, report))
    });
    Ok(())
}

/// Runs the drain completed pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn drain_completed_pane_process_workers(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<String>,
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
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<String>,
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
        Result<(String, AsyncPaneProcessServiceReport)>,
        tokio::task::JoinError,
    >,
    active_panes: &mut HashSet<String>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    match joined {
        Ok(Ok((pane_id, worker_report))) => {
            active_panes.remove(&pane_id);
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
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    bounded_idle: Option<Duration>,
) -> Option<
    std::result::Result<Result<(String, AsyncPaneProcessServiceReport)>, tokio::task::JoinError>,
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
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
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
) -> Vec<RuntimeEvent>
where
    B: AsyncPaneProcessIo,
{
    let mut events = Vec::new();
    let mut effects: VecDeque<_> = effects.into();
    while let Some(effect) = effects.pop_front() {
        let event = match effect {
            RuntimeSideEffect::WritePaneInput { pane_id, bytes }
            | RuntimeSideEffect::WritePaneInputPriority { pane_id, bytes } => {
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = bytes
                    .len()
                    .min(mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES);
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
            RuntimeSideEffect::ResizePane { size, .. } => driver.resize_event(size).await,
            RuntimeSideEffect::TerminatePane { force, .. } => driver.terminate_event(force).await,
            _ => continue,
        };
        events.push(event);
    }
    events
}
