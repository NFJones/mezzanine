//! Combined per-pane output and side-effect worker loop.

use super::delivery::{
    PendingShellInputDelivery, filter_shell_input_acknowledgements,
    shell_input_acknowledgement_count,
};
use super::helpers::{
    drain_pending_pane_io_side_effects, is_process_exit_event, pane_input_written_bytes,
    pane_io_events_for_side_effects, submit_pane_runtime_event,
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
    let mut pending_shell_input: Option<PendingShellInputDelivery> = None;

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if is_terminal_runtime_lifecycle_state(state) {
            if let Some(delivery) = pending_shell_input.as_mut() {
                submit_shell_input_progress_checkpoint(handle, driver, delivery, true, &mut report)
                    .await?;
            }
            terminate_pane_process_for_terminal_state(handle, driver, config, state, &mut report)
                .await?;
            return Ok(report);
        }
        if should_stop(report.polls, state) {
            if let Some(delivery) = pending_shell_input.as_mut() {
                submit_shell_input_progress_checkpoint(handle, driver, delivery, true, &mut report)
                    .await?;
            }
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let mut made_progress = false;
        let mut observed_output = false;
        let mut pane_exited = false;

        let filter_acknowledgements = pending_shell_input
            .as_ref()
            .is_some_and(PendingShellInputDelivery::is_waiting_for_acknowledgement);
        let (drained_output, shell_input_acknowledgements) = drain_pane_output_events(
            handle,
            driver,
            config.output_drain_limit,
            filter_acknowledgements,
            &mut report.output_events,
            &mut report.submitted_events,
            &mut report.applied_events,
        )
        .await?;
        if drained_output {
            made_progress = true;
            observed_output = true;
            if let Some(delivery) = pending_shell_input.as_mut() {
                delivery.observe_output(true, shell_input_acknowledgements);
            }
        }
        if let Some(delivery) = pending_shell_input.as_mut() {
            let complete = delivery.is_complete();
            submit_shell_input_progress_checkpoint(handle, driver, delivery, complete, &mut report)
                .await?;
            if complete {
                pending_shell_input = None;
            }
        }

        if pending_shell_input
            .as_ref()
            .is_some_and(|delivery| delivery.timed_out(Instant::now()))
        {
            if let Some(delivery) = pending_shell_input.as_mut() {
                submit_shell_input_progress_checkpoint(handle, driver, delivery, true, &mut report)
                    .await?;
            }
            let delivery = pending_shell_input
                .take()
                .expect("timed out delivery exists");
            let delivery_id = delivery.delivery_id().unwrap_or("unidentified");
            let event = driver.scope_event(RuntimeEvent::Pane(PaneEvent::WriteFailed {
                pane_id: delivery.pane_id().to_string(),
                error: format!(
                    "InvalidState: shell delivery {delivery_id} record progress timed out after {} ms",
                    super::delivery::SHELL_INPUT_RECORD_PROGRESS_TIMEOUT.as_millis()
                ),
            }));
            submit_pane_runtime_event(
                handle,
                event,
                &mut report.submitted_events,
                &mut report.applied_events,
            )
            .await?;
            made_progress = true;
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

        let effects = if pending_shell_input.is_some() {
            let actor_effects = if let Some(instance) = driver.process_instance().cloned() {
                handle
                    .drain_pane_process_io_side_effects(instance, config.drain_limit)
                    .await?
            } else {
                handle
                    .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
                    .await?
            };
            if actor_effects.is_empty() {
                drain_pending_pane_io_side_effects(
                    &mut pending_pane_io_side_effects,
                    config.drain_limit,
                )
            } else {
                actor_effects
            }
        } else if pending_pane_io_side_effects.is_empty() {
            if let Some(instance) = driver.process_instance().cloned() {
                handle
                    .drain_pane_process_io_side_effects(instance, config.drain_limit)
                    .await?
            } else {
                handle
                    .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
                    .await?
            }
        } else {
            drain_pending_pane_io_side_effects(
                &mut pending_pane_io_side_effects,
                config.drain_limit,
            )
        };
        let mut effects = VecDeque::from(effects);
        let mut ordinary_effects = Vec::new();
        while let Some(effect) = effects.pop_front() {
            if let RuntimeSideEffect::PaneProcessIo {
                effect: super::PaneProcessIoEffect::CancelShellInput { delivery_id },
                ..
            } = &effect
            {
                if pending_shell_input
                    .as_ref()
                    .is_some_and(|delivery| delivery.matches_delivery_id(delivery_id))
                {
                    let delivery = pending_shell_input
                        .as_mut()
                        .expect("matching shell delivery exists");
                    submit_shell_input_progress_checkpoint(
                        handle,
                        driver,
                        delivery,
                        true,
                        &mut report,
                    )
                    .await?;
                    pending_shell_input = None;
                }
                continue;
            }
            let terminates_process = matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: super::PaneProcessIoEffect::Terminate { .. },
                    ..
                } | RuntimeSideEffect::TerminatePane { .. }
            );
            let is_priority_terminal_response = matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: super::PaneProcessIoEffect::WriteInputPriority { .. },
                    ..
                } | RuntimeSideEffect::WritePaneInputPriority { .. }
            );
            if pending_shell_input.is_some()
                && !terminates_process
                && !is_priority_terminal_response
            {
                pending_pane_io_side_effects.push_back(effect);
                pending_pane_io_side_effects.extend(effects);
                break;
            }
            if terminates_process {
                if let Some(delivery) = pending_shell_input.as_mut() {
                    submit_shell_input_progress_checkpoint(
                        handle,
                        driver,
                        delivery,
                        true,
                        &mut report,
                    )
                    .await?;
                }
                pending_shell_input = None;
            }
            if let Some(delivery) = PendingShellInputDelivery::from_effect(&effect) {
                match delivery {
                    Ok(delivery) => {
                        if !delivery.is_complete() {
                            pending_shell_input = Some(delivery);
                        }
                    }
                    Err(error) => {
                        let pane_id = match &effect {
                            RuntimeSideEffect::PaneProcessIo { instance, .. } => {
                                instance.pane_id.clone()
                            }
                            RuntimeSideEffect::WritePaneShellInput { pane_id, .. } => {
                                pane_id.clone()
                            }
                            _ => driver.pane_id().to_string(),
                        };
                        let event =
                            driver.scope_event(RuntimeEvent::Pane(PaneEvent::WriteFailed {
                                pane_id,
                                error: format!("InvalidState: {error}"),
                            }));
                        submit_pane_runtime_event(
                            handle,
                            event,
                            &mut report.submitted_events,
                            &mut report.applied_events,
                        )
                        .await?;
                    }
                }
                pending_pane_io_side_effects.extend(effects);
                break;
            }
            ordinary_effects.push(effect);
        }
        if !ordinary_effects.is_empty() {
            made_progress = true;
            report.drained = report
                .drained
                .saturating_add(u64::try_from(ordinary_effects.len()).unwrap_or(u64::MAX));
            for event in pane_io_events_for_side_effects(
                driver,
                ordinary_effects,
                &mut pending_pane_io_side_effects,
                &mut false,
                &mut false,
            )
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

        while pending_shell_input
            .as_ref()
            .is_some_and(|delivery| !delivery.is_waiting())
        {
            let event = {
                let delivery = pending_shell_input
                    .as_mut()
                    .expect("active delivery exists");
                driver
                    .write_input_event(delivery.pending_record_suffix())
                    .await
            };
            let written = pane_input_written_bytes(&event);
            made_progress = true;
            let Some(written) = written else {
                if let Some(delivery) = pending_shell_input.as_mut() {
                    submit_shell_input_progress_checkpoint(
                        handle,
                        driver,
                        delivery,
                        true,
                        &mut report,
                    )
                    .await?;
                }
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
                pending_shell_input = None;
                break;
            };
            let aggregates_progress = pending_shell_input
                .as_ref()
                .is_some_and(PendingShellInputDelivery::aggregates_progress);
            if !aggregates_progress {
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
            }
            let supports_acknowledgements = driver.supports_shell_input_acknowledgements();
            let progress = pending_shell_input
                .as_mut()
                .expect("active delivery exists")
                .record_write(written, supports_acknowledgements);
            if let Err(error) = progress {
                if let Some(delivery) = pending_shell_input.as_mut() {
                    submit_shell_input_progress_checkpoint(
                        handle,
                        driver,
                        delivery,
                        true,
                        &mut report,
                    )
                    .await?;
                }
                let pane_id = pending_shell_input
                    .as_ref()
                    .expect("invalid delivery exists")
                    .pane_id()
                    .to_string();
                pending_shell_input = None;
                let event = driver.scope_event(RuntimeEvent::Pane(PaneEvent::WriteFailed {
                    pane_id,
                    error: format!("InvalidState: {error}"),
                }));
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
                break;
            }
            let complete = pending_shell_input
                .as_ref()
                .is_some_and(PendingShellInputDelivery::is_complete);
            if let Some(delivery) = pending_shell_input.as_mut() {
                submit_shell_input_progress_checkpoint(
                    handle,
                    driver,
                    delivery,
                    complete,
                    &mut report,
                )
                .await?;
            }
            if complete {
                pending_shell_input = None;
                break;
            }
        }

        if !observed_output && let Some(event) = driver.poll_exit_event().await? {
            if let Some(delivery) = pending_shell_input.as_mut() {
                submit_shell_input_progress_checkpoint(handle, driver, delivery, true, &mut report)
                    .await?;
            }
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
            let idle_delay = pending_shell_input.as_ref().map_or_else(
                || pane_process_quiet_delay(last_foreground_metadata_poll, config),
                |delivery| {
                    let now = Instant::now();
                    let delay = pane_process_quiet_delay(last_foreground_metadata_poll, config)
                        .min(delivery.remaining_progress_time(now));
                    delivery
                        .remaining_progress_checkpoint_time(now)
                        .map_or(delay, |checkpoint| delay.min(checkpoint))
                },
            );
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

    if let Some(delivery) = pending_shell_input.as_mut() {
        submit_shell_input_progress_checkpoint(handle, driver, delivery, true, &mut report).await?;
    }
    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Publishes one cumulative receiver-input checkpoint with process fencing.
fn submit_shell_input_progress_checkpoint<'a, B>(
    handle: &'a AsyncRuntimeSessionHandle,
    driver: &AsyncPaneProcessDriver<B>,
    delivery: &mut PendingShellInputDelivery,
    force: bool,
    report: &'a mut AsyncPaneProcessServiceReport,
) -> impl std::future::Future<Output = Result<()>> + Send + 'a
where
    B: AsyncPaneProcessIo,
{
    let checkpoint = delivery
        .take_progress_checkpoint(Instant::now(), force)
        .map(|bytes| {
            let event = driver.scope_event(RuntimeEvent::Pane(PaneEvent::InputWritten {
                pane_id: delivery.pane_id().to_string(),
                bytes,
            }));
            (bytes, event)
        });
    async move {
        let Some((bytes, event)) = checkpoint else {
            return Ok(());
        };
        submit_pane_runtime_event(
            handle,
            event,
            &mut report.submitted_events,
            &mut report.applied_events,
        )
        .await?;
        report.shell_input_progress_events = report.shell_input_progress_events.saturating_add(1);
        report.shell_input_progress_bytes = report.shell_input_progress_bytes.saturating_add(bytes);
        Ok(())
    }
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
    filter_acknowledgements: bool,
    output_events: &mut u64,
    submitted_events: &mut usize,
    applied_events: &mut usize,
) -> Result<(bool, usize)>
where
    B: AsyncPaneProcessIo,
{
    let pane_id = driver.pane_id().to_string();
    let mut bytes = Vec::new();
    for _ in 0..limit {
        match driver.poll_output_event().await {
            Ok(Some(event)) => {
                let output_bytes = match event {
                    RuntimeEvent::Pane(PaneEvent::Output { bytes, .. }) => bytes,
                    RuntimeEvent::PaneProcess {
                        event: super::PaneProcessEvent::Pane(PaneEvent::Output { bytes, .. }),
                        ..
                    } => bytes,
                    _ => continue,
                };
                *output_events = output_events.saturating_add(1);
                bytes.extend(output_bytes);
            }
            Ok(None) => break,
            Err(error) if !bytes.is_empty() => {
                let event =
                    driver.scope_event(RuntimeEvent::Pane(PaneEvent::Output { pane_id, bytes }));
                let ingress = submit_batched_pane_output_event(handle, event).await?;
                *submitted_events = submitted_events.saturating_add(ingress.accepted);
                *applied_events = applied_events.saturating_add(ingress.applied);
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    if bytes.is_empty() {
        return Ok((false, 0));
    }
    let shell_input_acknowledgements = if filter_acknowledgements {
        filter_shell_input_acknowledgements(&mut bytes)
    } else {
        shell_input_acknowledgement_count(&bytes)
    };
    if bytes.is_empty() {
        return Ok((true, shell_input_acknowledgements));
    }
    let event = driver.scope_event(RuntimeEvent::Pane(PaneEvent::Output { pane_id, bytes }));
    let ingress = submit_batched_pane_output_event(handle, event).await?;
    *submitted_events = submitted_events.saturating_add(ingress.accepted);
    *applied_events = applied_events.saturating_add(ingress.applied);
    Ok((true, shell_input_acknowledgements))
}

/// Submits coalesced pane output bytes as one ordered runtime event.
pub(super) async fn submit_batched_pane_output_event(
    handle: &AsyncRuntimeSessionHandle,
    event: RuntimeEvent,
) -> Result<super::RuntimeEventIngressReport> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(event);
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
    let mut force = matches!(
        state,
        RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
    );
    let effects = if let Some(instance) = driver.process_instance().cloned() {
        handle
            .drain_pane_process_io_side_effects(instance, config.drain_limit)
            .await?
    } else {
        handle
            .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
            .await?
    };
    report.drained = report
        .drained
        .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
    for effect in effects {
        match effect {
            RuntimeSideEffect::TerminatePane {
                force: requested_force,
                ..
            }
            | RuntimeSideEffect::PaneProcessIo {
                effect:
                    super::PaneProcessIoEffect::Terminate {
                        force: requested_force,
                    },
                ..
            } => force |= requested_force,
            _ => {}
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
