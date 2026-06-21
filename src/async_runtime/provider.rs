//! Async Runtime Provider implementation.
//!
//! This module owns the async runtime provider boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsyncAgentProviderServiceConfig, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle, AttachedTerminalClientLoopReport, Result, RuntimeLifecycleState,
    run_async_agent_provider_service,
};

// Async agent provider polling service.

/// Runs the build async agent provider service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_async_agent_provider_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncAgentProviderServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_agent_provider_service(&handle, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.executions))
    }))
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
