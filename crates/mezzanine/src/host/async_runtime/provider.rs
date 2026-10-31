//! Provider and deferred-command service assembly for runtime supervision.
//!
//! These adapters install outside-wrapper retirement owners before supervision
//! starts. The provider wrapper borrows its actual driver's handle; cancellation
//! cannot interrupt request cleanup, and the driver receives separate work state
//! to avoid retaining its own handle. The command lane retains exact fork work.
//! No socket, permission, retry or native process authority is introduced here;
//! dispatch/adoption stay with the driver and actor, joins with the supervisor.

use super::{
    AsyncAgentProviderServiceConfig, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle, AttachedTerminalClientLoopReport, Result, RuntimeLifecycleState,
};

// Async agent provider polling service.

/// Builds a named provider adapter with supervisor-retained driver/work custody.
/// Invalid configuration fails before any task starts. Polling the wrapper
/// starts and observes its actual driver; request/actor/cleanup failures remain
/// errors, while supervisor cancellation still joins that driver before shutdown.
pub fn build_async_agent_provider_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncAgentProviderServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    let retirement = super::client::AgentProviderServiceRetirement::new(handle.clone());
    let worker_retirement = retirement.clone();
    let mut service = AsyncRuntimeService::new_auxiliary(name, async move {
        let work = worker_retirement.work();
        worker_retirement
            .start_driver(async move {
                super::client::run_async_agent_provider_service_owned(
                    &handle,
                    config,
                    |_, state| is_terminal_runtime_lifecycle_state(state),
                    work,
                )
                .await
            })
            .await?;
        let report = worker_retirement.join_driver().await?;
        Ok(AsyncRuntimeServiceExit::completed(report.executions))
    });
    service.agent_provider_retirement = Some(retirement);
    Ok(service)
}

/// Builds the dedicated deferred interactive-command worker service.
pub fn build_async_agent_command_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncAgentProviderServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    let retirement = super::client::SavedForkServiceRetirement::default();
    let worker_retirement = retirement.clone();
    let mut service = AsyncRuntimeService::new_auxiliary(name, async move {
        let executions = super::client::run_async_agent_command_service_owned(
            &handle,
            config,
            |_, state| is_terminal_runtime_lifecycle_state(state),
            worker_retirement,
        )
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(executions))
    });
    service.saved_fork_retirement = Some(retirement);
    Ok(service)
}

/// Runs the empty attached terminal loop report operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn empty_attached_terminal_loop_report() -> AttachedTerminalClientLoopReport {
    AttachedTerminalClientLoopReport {
        iterations: 0,
        actions: Vec::new(),
        output_frames: 0,
        bytes_written: 0,
        partial_writes: 0,
        pending_output_bytes: 0,
        input_hangups: 0,
        output_hangups: 0,
        error_roles: Vec::new(),
        host_bracketed_paste_active: false,
        host_bracketed_paste_buffer: Vec::new(),
        host_bracketed_paste_started_at: None,
    }
}

/// Runs the merge attached terminal loop report operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn merge_attached_terminal_loop_report(
    total: &mut AttachedTerminalClientLoopReport,
    batch: AttachedTerminalClientLoopReport,
) {
    total.iterations = total.iterations.saturating_add(batch.iterations);
    total.actions.extend(batch.actions);
    total.output_frames = total.output_frames.saturating_add(batch.output_frames);
    total.bytes_written = total.bytes_written.saturating_add(batch.bytes_written);
    total.partial_writes = total.partial_writes.saturating_add(batch.partial_writes);
    total.pending_output_bytes = batch.pending_output_bytes;
    total.input_hangups = total.input_hangups.saturating_add(batch.input_hangups);
    total.output_hangups = total.output_hangups.saturating_add(batch.output_hangups);
    total.error_roles.extend(batch.error_roles);
    total.host_bracketed_paste_active = batch.host_bracketed_paste_active;
    total.host_bracketed_paste_buffer = batch.host_bracketed_paste_buffer;
    total.host_bracketed_paste_started_at = batch.host_bracketed_paste_started_at;
}

/// Runs the is terminal runtime lifecycle state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_terminal_runtime_lifecycle_state(state: RuntimeLifecycleState) -> bool {
    matches!(
        state,
        RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Killed
            | RuntimeLifecycleState::Failed
    )
}
