//! Pane worker construction and daemon supervisor service.

use super::helpers::{
    abort_pane_process_workers, drain_completed_pane_process_workers,
    drain_completed_pane_process_workers_after_yields, is_terminal_pane_supervisor_error,
    record_joined_pane_process_worker, spawn_owned_pane_process_worker,
    wait_for_pane_process_supervisor_wakeup,
};
#[cfg(test)]
use super::{
    AsyncPaneProcessDriver, AsyncPaneProcessIo, AsyncPaneProcessServiceConfig,
    run_async_pane_process_service,
};
use super::{
    AsyncPaneProcessSupervisorServiceConfig, AsyncPaneProcessSupervisorServiceReport,
    AsyncRuntimeService, AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, HashSet, JoinSet,
    Result, RuntimeLifecycleState, is_terminal_runtime_lifecycle_state,
};

/// Builds an auxiliary service for the combined async pane process path.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn build_async_pane_process_service<B>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    mut driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
) -> Result<AsyncRuntimeService>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_pane_process_service(&handle, &mut driver, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        let work_units = report.drained.saturating_add(report.exit_events);
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            Ok(AsyncRuntimeServiceExit::shutdown(work_units))
        } else {
            Ok(AsyncRuntimeServiceExit::completed(work_units))
        }
    }))
}

/// Runs the daemon pane-process supervisor until stopped.
///
/// The supervisor claims any manager-owned running pane processes through the
/// actor and immediately moves each claimed process into a combined async pane
/// worker. Each worker owns its pane backend until the pane exits or the daemon
/// enters a terminal lifecycle state.
pub async fn run_async_pane_process_supervisor_service<F>(
    handle: AsyncRuntimeSessionHandle,
    config: AsyncPaneProcessSupervisorServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessSupervisorServiceReport>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessSupervisorServiceReport::new(*lifecycle_watcher.borrow());
    let mut active_panes = HashSet::<String>::new();
    let mut workers = JoinSet::new();

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            drain_completed_pane_process_workers_after_yields(
                &mut workers,
                &mut active_panes,
                &mut report,
            )
            .await?;
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }
        if should_stop(report.polls, state) {
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let mut made_progress = false;
        let processes = match handle
            .take_running_pane_processes_for_adapter(config.take_limit)
            .await
        {
            Ok(processes) => processes,
            Err(error) if is_terminal_pane_supervisor_error(&error) => {
                abort_pane_process_workers(&mut workers).await;
                report.terminal_state = *lifecycle_watcher.borrow();
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        for (pane_id, process) in processes {
            active_panes.insert(pane_id.clone());
            spawn_owned_pane_process_worker(
                &mut workers,
                handle.clone(),
                pane_id,
                process,
                config.pane_service,
            )?;
            report.spawned_workers = report.spawned_workers.saturating_add(1);
            made_progress = true;
        }

        drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            drain_completed_pane_process_workers_after_yields(
                &mut workers,
                &mut active_panes,
                &mut report,
            )
            .await?;
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }

        if !made_progress && report.polls < config.max_polls {
            let bounded_idle = (config.max_polls != u64::MAX).then_some(config.idle_interval);
            if let Some(joined) = wait_for_pane_process_supervisor_wakeup(
                &handle,
                &mut workers,
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                bounded_idle,
            )
            .await
            {
                record_joined_pane_process_worker(joined, &mut active_panes, &mut report)?;
            }
            drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
            if is_terminal_runtime_lifecycle_state(report.terminal_state) {
                abort_pane_process_workers(&mut workers).await;
                return Ok(report);
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    abort_pane_process_workers(&mut workers).await;
    Ok(report)
}

/// Builds the production dynamic pane-process supervisor service.
pub fn build_async_pane_process_supervisor_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncPaneProcessSupervisorServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = match run_async_pane_process_supervisor_service(handle, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await
        {
            Ok(report) => report,
            Err(error) if is_terminal_pane_supervisor_error(&error) => {
                return Ok(AsyncRuntimeServiceExit::shutdown(0));
            }
            Err(error) => return Err(error),
        };
        let work_units = report
            .spawned_workers
            .saturating_add(report.completed_workers);
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            Ok(AsyncRuntimeServiceExit::shutdown(work_units))
        } else {
            Ok(AsyncRuntimeServiceExit::completed(work_units))
        }
    }))
}
