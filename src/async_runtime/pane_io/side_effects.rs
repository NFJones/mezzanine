//! Standalone pane side-effect execution service.

use super::helpers::{drain_pending_pane_io_side_effects, pane_io_events_for_side_effects};
use super::*;

/// Drains pane I/O side effects for one pane and executes them through that
/// pane's async driver.
pub async fn run_async_pane_io_side_effect_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneIoSideEffectServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneIoSideEffectServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut pending_pane_io_side_effects = VecDeque::new();
    let mut report = AsyncPaneIoSideEffectServiceReport {
        polls: 0,
        drained: 0,
        submitted_events: 0,
        applied_events: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
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
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_pane_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }

        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        for event in
            pane_io_events_for_side_effects(driver, effects, &mut pending_pane_io_side_effects)
                .await
        {
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            let ingress = handle.submit_runtime_events(batch).await?;
            report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
            report.applied_events = report.applied_events.saturating_add(ingress.applied);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Runs the wait for pane side effects or bounded idle operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn wait_for_pane_side_effects_or_bounded_idle(
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    config: AsyncPaneIoSideEffectServiceConfig,
) {
    if config.max_polls == u64::MAX {
        tokio::select! {
            result = side_effect_watcher.changed() => {
                let _ = result;
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
            }
        }
    } else {
        tokio::select! {
            result = side_effect_watcher.changed() => {
                let _ = result;
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
            }
            _ = sleep(config.idle_interval) => {}
        }
    }
}

/// Builds an auxiliary service for one pane's side-effect-driven I/O path.
pub fn build_async_pane_io_side_effect_service<B>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    mut driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneIoSideEffectServiceConfig,
) -> Result<AsyncRuntimeService>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report =
            run_async_pane_io_side_effect_service(&handle, &mut driver, config, |_, state| {
                is_terminal_runtime_lifecycle_state(state)
            })
            .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.drained))
    }))
}
