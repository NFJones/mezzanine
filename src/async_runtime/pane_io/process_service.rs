//! Combined per-pane output and side-effect worker loop.

use super::helpers::{
    drain_pending_pane_io_side_effects, is_process_exit_event, pane_io_events_for_side_effects,
    submit_pane_runtime_event,
};
use super::{
    AsyncPaneProcessDriver, AsyncPaneProcessIo, AsyncPaneProcessServiceConfig,
    AsyncPaneProcessServiceReport, AsyncRuntimeSessionHandle, Duration, Instant, PaneEvent, Result,
    RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState, RuntimeSideEffect, VecDeque,
    is_terminal_runtime_lifecycle_state, sleep,
};

/// Runs one combined pane process worker until stopped.
///
/// The worker first drains a bounded burst of PTY output, then drains pending
/// pane I/O side effects for the same pane. This keeps the future live
/// ownership path from racing write, resize, terminate, and output handling
/// across independent tasks while avoiding one actor round trip per output
/// chunk during bursty pane redraws.
pub async fn run_async_pane_process_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessServiceReport::new(*lifecycle_watcher.borrow());
    let mut last_foreground_metadata_poll: Option<Instant> = None;
    let mut pending_pane_io_side_effects = VecDeque::new();

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if is_terminal_runtime_lifecycle_state(state) {
            terminate_pane_process_for_terminal_state(handle, driver, config, state, &mut report)
                .await?;
            return Ok(report);
        }
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let mut made_progress = false;
        let mut observed_output = false;
        let mut pane_exited = false;

        if drain_pane_output_events(
            handle,
            driver,
            config.output_drain_limit,
            &mut report.output_events,
            &mut report.submitted_events,
            &mut report.applied_events,
        )
        .await?
        {
            made_progress = true;
            observed_output = true;
        }

        let foreground_metadata_due = last_foreground_metadata_poll
            .is_none_or(|last_poll| last_poll.elapsed() >= config.foreground_metadata_interval);
        if foreground_metadata_due {
            last_foreground_metadata_poll = Some(Instant::now());
            if let Some(event) = driver.poll_foreground_process_event().await? {
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
                made_progress = true;
            }
        }

        let effects = if pending_pane_io_side_effects.is_empty() {
            handle
                .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
                .await?
        } else {
            drain_pending_pane_io_side_effects(
                &mut pending_pane_io_side_effects,
                config.drain_limit,
            )
        };
        if !effects.is_empty() {
            made_progress = true;
            report.drained = report
                .drained
                .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
            for event in
                pane_io_events_for_side_effects(driver, effects, &mut pending_pane_io_side_effects)
                    .await
            {
                pane_exited |= is_process_exit_event(&event);
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
            }
        }

        if !observed_output && let Some(event) = driver.poll_exit_event().await? {
            report.exit_events = report.exit_events.saturating_add(1);
            pane_exited = is_process_exit_event(&event);
            submit_pane_runtime_event(
                handle,
                event,
                &mut report.submitted_events,
                &mut report.applied_events,
            )
            .await?;
            made_progress = true;
        }

        if pane_exited {
            report.terminal_state = *lifecycle_watcher.borrow();
            return Ok(report);
        }

        if !made_progress && report.polls < config.max_polls {
            let idle_delay = pane_process_quiet_delay(last_foreground_metadata_poll, config);
            if let Some(output_activity) = driver.output_activity() {
                tokio::select! {
                    result = output_activity => result?,
                    _ = handle.wait_for_event_delivery() => {}
                    result = side_effect_watcher.changed() => {
                        let _ = result;
                    }
                    result = lifecycle_watcher.changed() => {
                        let _ = result;
                    }
                    _ = sleep(idle_delay) => {}
                }
            } else {
                tokio::select! {
                    _ = handle.wait_for_event_delivery() => {}
                    result = side_effect_watcher.changed() => {
                        let _ = result;
                    }
                    result = lifecycle_watcher.changed() => {
                        let _ = result;
                    }
                    _ = sleep(idle_delay) => {}
                }
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Drains currently available pane output chunks into one actor submission.
///
/// PTY output often arrives in bursts. Submitting a bounded burst as one actor
/// batch reduces event-loop hops and lets render invalidation coalescing happen
/// before the attached terminal is asked to repaint.
pub(super) async fn drain_pane_output_events<B>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    limit: usize,
    output_events: &mut u64,
    submitted_events: &mut usize,
    applied_events: &mut usize,
) -> Result<bool>
where
    B: AsyncPaneProcessIo,
{
    let pane_id = driver.pane_id().to_string();
    let mut bytes = Vec::new();
    for _ in 0..limit {
        match driver.poll_output_event().await {
            Ok(Some(event)) => {
                let RuntimeEvent::Pane(PaneEvent::Output {
                    bytes: output_bytes,
                    ..
                }) = event
                else {
                    continue;
                };
                *output_events = output_events.saturating_add(1);
                bytes.extend(output_bytes);
            }
            Ok(None) => break,
            Err(error) if !bytes.is_empty() => {
                let ingress = submit_batched_pane_output_event(handle, pane_id, bytes).await?;
                *submitted_events = submitted_events.saturating_add(ingress.accepted);
                *applied_events = applied_events.saturating_add(ingress.applied);
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    if bytes.is_empty() {
        return Ok(false);
    }
    let ingress = submit_batched_pane_output_event(handle, pane_id, bytes).await?;
    *submitted_events = submitted_events.saturating_add(ingress.accepted);
    *applied_events = applied_events.saturating_add(ingress.applied);
    Ok(true)
}

/// Submits coalesced pane output bytes as one ordered runtime event.
pub(super) async fn submit_batched_pane_output_event(
    handle: &AsyncRuntimeSessionHandle,
    pane_id: String,
    bytes: Vec<u8>,
) -> Result<super::RuntimeEventIngressReport> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Pane(PaneEvent::Output { pane_id, bytes }));
    handle.submit_runtime_events(batch).await
}

/// Runs the terminate pane process for terminal state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn terminate_pane_process_for_terminal_state<B>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
    state: RuntimeLifecycleState,
    report: &mut AsyncPaneProcessServiceReport,
) -> Result<()>
where
    B: AsyncPaneProcessIo,
{
    let pane_id = driver.pane_id().to_string();
    let mut force = matches!(
        state,
        RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
    );
    let effects = handle
        .drain_pane_io_side_effects(pane_id, config.drain_limit)
        .await?;
    report.drained = report
        .drained
        .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
    for effect in effects {
        if let RuntimeSideEffect::TerminatePane {
            force: requested_force,
            ..
        } = effect
        {
            force |= requested_force;
        }
    }
    let event = driver.terminate_event(force).await;
    if is_process_exit_event(&event) {
        report.exit_events = report.exit_events.saturating_add(1);
    }
    Ok(())
}

/// Runs the pane process quiet delay operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_process_quiet_delay(
    last_foreground_metadata_poll: Option<Instant>,
    config: AsyncPaneProcessServiceConfig,
) -> Duration {
    let Some(last_foreground_metadata_poll) = last_foreground_metadata_poll else {
        return config.idle_interval;
    };
    let remaining = config
        .foreground_metadata_interval
        .saturating_sub(last_foreground_metadata_poll.elapsed());
    if remaining.is_zero() {
        config.idle_interval
    } else {
        remaining
    }
}
