//! Async Runtime Client implementation.
//!
//! This module owns the async runtime client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentCompactionEvent, AgentProviderEvent, AgentTurnLedger, AgentTurnRunner,
    AsyncAgentProviderPollReport, AsyncAgentProviderServiceConfig, AsyncAttachedTerminalIo,
    AsyncAttachedTerminalLoopRequest, AsyncAttachedTerminalPaneIoMode, AsyncRuntimeService,
    AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, AsyncTerminalIoFuture,
    AsyncTerminalOutputWriteReport, AttachedTerminalClientLoopReport, AttachedTerminalFdReadiness,
    AttachedTerminalFdRole, ClientStatusLine, MezError, MouseAction, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, TerminalClientLoopAction,
    empty_attached_terminal_loop_report, is_terminal_runtime_lifecycle_state,
    merge_attached_terminal_loop_report, run_async_attached_terminal_client_loop,
    run_async_attached_terminal_client_loop_deferred_pane_io, sleep,
};
use crate::agent::AsyncModelProvider;
use crate::error::MezErrorKind;
use crate::ids::AgentId;
use crate::runtime::runtime_execute_auto_sizing_with_async_provider;
use crate::terminal::{TerminalFdInterest, TerminalStyleSpan};
use tokio::sync::watch;

// Attached terminal client service construction.

/// Carries Async Attached Terminal Client Service Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncAttachedTerminalClientServiceConfig {
    /// Maximum foreground client-loop batches before the service returns.
    pub max_batches: u64,
}

impl AsyncAttachedTerminalClientServiceConfig {
    /// Validates foreground attached-terminal service limits.
    pub fn validate(self) -> Result<()> {
        if self.max_batches == 0 {
            return Err(MezError::invalid_args(
                "attached terminal service max_batches must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for AsyncAttachedTerminalClientServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_batches: u64::MAX,
        }
    }
}

/// Carries Async Attached Terminal Client Service Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncAttachedTerminalClientServiceReport {
    /// Stores the batches value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub batches: u64,
    /// Stores the loop report value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub loop_report: AttachedTerminalClientLoopReport,
    /// Stores the terminal state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_state: RuntimeLifecycleState,
    /// Stores the stopped by lifecycle value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stopped_by_lifecycle: bool,
    /// Stores the terminal resizes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_resizes: u64,
}

/// Runs the run async attached terminal client service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run_async_attached_terminal_client_service<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    request: AsyncAttachedTerminalLoopRequest,
    service_config: AsyncAttachedTerminalClientServiceConfig,
    status_provider: S,
) -> Result<AsyncAttachedTerminalClientServiceReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    run_async_attached_terminal_client_service_with_pane_io_mode(
        handle,
        io,
        request,
        service_config,
        AsyncAttachedTerminalPaneIoMode::Inline,
        status_provider,
    )
    .await
}

/// Runs an attached-terminal service whose primary pane input is queued for
/// async pane process workers.
pub async fn run_async_attached_terminal_client_service_deferred_pane_io<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    request: AsyncAttachedTerminalLoopRequest,
    service_config: AsyncAttachedTerminalClientServiceConfig,
    status_provider: S,
) -> Result<AsyncAttachedTerminalClientServiceReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    run_async_attached_terminal_client_service_with_pane_io_mode(
        handle,
        io,
        request,
        service_config,
        AsyncAttachedTerminalPaneIoMode::Deferred,
        status_provider,
    )
    .await
}

/// Runs the run async attached terminal client service with pane io mode operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn run_async_attached_terminal_client_service_with_pane_io_mode<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    mut request: AsyncAttachedTerminalLoopRequest,
    service_config: AsyncAttachedTerminalClientServiceConfig,
    pane_io_mode: AsyncAttachedTerminalPaneIoMode,
    mut status_provider: S,
) -> Result<AsyncAttachedTerminalClientServiceReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    service_config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut report = AsyncAttachedTerminalClientServiceReport {
        batches: 0,
        loop_report: empty_attached_terminal_loop_report(),
        terminal_state: *lifecycle_watcher.borrow(),
        stopped_by_lifecycle: false,
        terminal_resizes: 0,
    };
    let mut pending_resize_debounce_timer: Option<RuntimeTimerKey> = None;
    let mut resize_debounce_generation = 0u64;
    let mut render_requested = true;

    while report.batches < service_config.max_batches {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if is_attached_terminal_client_stop_state(state) {
            report.stopped_by_lifecycle = true;
            return Ok(report);
        }

        request.terminal_config = handle
            .terminal_client_loop_config(request.terminal_config.clone())
            .await?;

        let wake = wait_for_attached_terminal_batch_readiness(
            handle,
            io,
            &request.client_id,
            render_requested,
            &mut lifecycle_watcher,
        )
        .await?;
        render_requested = false;
        let AttachedTerminalBatchWake::Readiness(mut readiness) = wake else {
            continue;
        };

        let mut resized_this_batch = false;
        if let Some(size) = io.terminal_size().await?
            && size != request.client_size
        {
            request.client_size = size;
            if request.role == super::ClientViewRole::Primary
                && let Some(primary_client_id) = request.primary_client_id.clone()
            {
                handle
                    .resize_attached_primary_terminal(primary_client_id, size)
                    .await?;
            }
            report.terminal_resizes = report.terminal_resizes.saturating_add(1);
            resized_this_batch = true;
        }
        if resized_this_batch {
            ensure_output_readiness(&mut readiness);
        }

        let iteration_offset = report.loop_report.iterations;
        let mut prepolled_io = PrepolledAttachedTerminalIo::new(io, readiness);
        let batch = match pane_io_mode {
            AsyncAttachedTerminalPaneIoMode::Inline => {
                run_async_attached_terminal_client_loop(
                    handle,
                    &mut prepolled_io,
                    request.clone(),
                    |iteration| status_provider(iteration_offset.saturating_add(iteration)),
                )
                .await?
            }
            AsyncAttachedTerminalPaneIoMode::Deferred => {
                run_async_attached_terminal_client_loop_deferred_pane_io(
                    handle,
                    &mut prepolled_io,
                    request.clone(),
                    |iteration| status_provider(iteration_offset.saturating_add(iteration)),
                )
                .await?
            }
        };
        report.batches = report.batches.saturating_add(1);
        let should_finish =
            batch.input_hangups > 0 || batch.output_hangups > 0 || !batch.error_roles.is_empty();
        resized_this_batch |= attached_terminal_actions_include_resize(&batch.actions);
        request.terminal_config.host_bracketed_paste_active = batch.host_bracketed_paste_active;
        request.terminal_config.host_bracketed_paste_buffer =
            batch.host_bracketed_paste_buffer.clone();
        merge_attached_terminal_loop_report(&mut report.loop_report, batch);
        if resized_this_batch {
            resize_debounce_generation = resize_debounce_generation.saturating_add(1);
            let next_key = RuntimeTimerKey::new(
                RuntimeTimerKind::ResizeDebounce,
                request.client_id.as_str(),
                resize_debounce_generation,
            );
            queue_resize_debounce_timer(
                handle,
                pending_resize_debounce_timer.replace(next_key.clone()),
                next_key,
                request.terminal_config.resize_debounce_ms,
            )
            .await?;
        }
        if should_finish {
            return Ok(report);
        }
        if report.batches >= service_config.max_batches {
            return Ok(report);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Runs the attached terminal actions include resize operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn attached_terminal_actions_include_resize(actions: &[TerminalClientLoopAction]) -> bool {
    actions.iter().any(|action| {
        matches!(
            action,
            TerminalClientLoopAction::HandleMouse(
                MouseAction::ResizePane { .. } | MouseAction::FinishResizePane
            )
        )
    })
}

/// Runs the queue resize debounce timer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn queue_resize_debounce_timer(
    handle: &AsyncRuntimeSessionHandle,
    previous_key: Option<RuntimeTimerKey>,
    next_key: RuntimeTimerKey,
    resize_debounce_ms: u64,
) -> Result<()> {
    let mut side_effects = Vec::new();
    if let Some(key) = previous_key {
        side_effects.push(RuntimeSideEffect::CancelTimer { key });
    }
    side_effects.push(RuntimeSideEffect::ScheduleTimer {
        key: next_key,
        delay_ms: resize_debounce_ms.max(1),
    });
    handle
        .queue_runtime_side_effects(side_effects)
        .await
        .map(|_| ())
}

/// Carries Attached Terminal Batch Wake state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachedTerminalBatchWake {
    /// Represents the Readiness case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Readiness(Vec<AttachedTerminalFdReadiness>),
    /// Represents the State Changed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    StateChanged,
}

/// Runs the wait for attached terminal batch readiness operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_attached_terminal_batch_readiness<I>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    client_id: &super::ClientId,
    render_requested: bool,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
) -> Result<AttachedTerminalBatchWake>
where
    I: AsyncAttachedTerminalIo,
{
    if render_requested {
        return Ok(AttachedTerminalBatchWake::Readiness(vec![
            synthetic_output_readiness(),
        ]));
    }

    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    loop {
        let render_effects = handle
            .drain_render_side_effects_for_client(client_id.clone(), 8)
            .await?;
        if !render_effects.is_empty() {
            return Ok(AttachedTerminalBatchWake::Readiness(vec![
                synthetic_output_readiness(),
            ]));
        }
        if io.pending_output_bytes() > 0 {
            return Ok(AttachedTerminalBatchWake::Readiness(
                io.poll_readiness().await?,
            ));
        }

        tokio::select! {
            biased;
            readiness = io.poll_input_readiness() => {
                let mut readiness = readiness?;
                if readiness.iter().any(attached_terminal_readiness_is_readable_input_or_control) {
                    ensure_output_readiness(&mut readiness);
                }
                return Ok(AttachedTerminalBatchWake::Readiness(readiness));
            }
            _ = handle.wait_for_event_delivery() => {
                let render_effects = handle
                    .drain_render_side_effects_for_client(client_id.clone(), 8)
                    .await?;
                if !render_effects.is_empty() {
                    return Ok(AttachedTerminalBatchWake::Readiness(vec![synthetic_output_readiness()]));
                }
                return Ok(AttachedTerminalBatchWake::StateChanged);
            }
            result = side_effect_watcher.changed() => {
                let _ = result;
                let render_effects = handle
                    .drain_render_side_effects_for_client(client_id.clone(), 8)
                    .await?;
                if !render_effects.is_empty() {
                    return Ok(AttachedTerminalBatchWake::Readiness(vec![synthetic_output_readiness()]));
                }
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
                return Ok(AttachedTerminalBatchWake::StateChanged);
            }
        }
    }
}

/// Runs the attached terminal readiness is readable input or control operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn attached_terminal_readiness_is_readable_input_or_control(
    readiness: &AttachedTerminalFdReadiness,
) -> bool {
    readiness.readable
        && matches!(
            readiness.role,
            AttachedTerminalFdRole::Input | AttachedTerminalFdRole::Control
        )
}

/// Runs the ensure output readiness operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ensure_output_readiness(readiness: &mut Vec<AttachedTerminalFdReadiness>) {
    if !readiness
        .iter()
        .any(|ready| ready.role == AttachedTerminalFdRole::Output)
    {
        readiness.push(synthetic_output_readiness());
    }
}

/// Runs the synthetic output readiness operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn synthetic_output_readiness() -> AttachedTerminalFdReadiness {
    AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    }
}

/// Carries Prepolled Attached Terminal Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct PrepolledAttachedTerminalIo<'a, I> {
    /// Stores the inner value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    inner: &'a mut I,
    /// Stores the readiness value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    readiness: Option<Vec<AttachedTerminalFdReadiness>>,
}

impl<'a, I> PrepolledAttachedTerminalIo<'a, I> {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn new(inner: &'a mut I, readiness: Vec<AttachedTerminalFdReadiness>) -> Self {
        Self {
            inner,
            readiness: Some(readiness),
        }
    }
}

impl<I> AsyncAttachedTerminalIo for PrepolledAttachedTerminalIo<'_, I>
where
    I: AsyncAttachedTerminalIo,
{
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        if let Some(readiness) = self.readiness.take() {
            return Box::pin(async move { Ok(readiness) });
        }
        self.inner.poll_readiness()
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        self.inner.poll_input_readiness()
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
        self.inner.read_input(max_bytes)
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: super::AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize> {
        self.inner
            .write_styled_output_with_modes(lines, line_style_spans, modes)
    }

    fn pending_output_bytes(&self) -> usize {
        self.inner.pending_output_bytes()
    }

    fn flush_pending_output<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        self.inner.flush_pending_output(max_bytes)
    }

    fn write_styled_output_with_modes_bounded<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: super::AttachedTerminalOutputModes,
        max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        self.inner
            .write_styled_output_with_modes_bounded(lines, line_style_spans, modes, max_bytes)
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, Option<super::Size>> {
        self.inner.terminal_size()
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        self.inner.invalidate_output_frame()
    }

    /// Runs the enter presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enter_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        self.inner.enter_presentation()
    }

    /// Runs the restore presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn restore_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        self.inner.restore_presentation()
    }
}

/// Runs the build async attached terminal client service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_async_attached_terminal_client_service<I, S>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    mut io: I,
    request: AsyncAttachedTerminalLoopRequest,
    service_config: AsyncAttachedTerminalClientServiceConfig,
    mut status_provider: S,
) -> Result<AsyncRuntimeService>
where
    I: AsyncAttachedTerminalIo + Send + 'static,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>> + Send + 'static,
{
    service_config.validate()?;
    Ok(AsyncRuntimeService::new(name, async move {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            request,
            service_config,
            &mut status_provider,
        )
        .await?;
        let work_units = report.loop_report.iterations;
        if report.stopped_by_lifecycle && is_terminal_runtime_lifecycle_state(report.terminal_state)
        {
            Ok(AsyncRuntimeServiceExit::shutdown(work_units))
        } else {
            Ok(AsyncRuntimeServiceExit::completed(work_units))
        }
    }))
}

/// Runs the is attached terminal client stop state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_attached_terminal_client_stop_state(state: RuntimeLifecycleState) -> bool {
    state == RuntimeLifecycleState::Detached || is_terminal_runtime_lifecycle_state(state)
}

/// Runs the run async agent provider service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run_async_agent_provider_service<F>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncAgentProviderServiceConfig,
    mut should_stop: F,
) -> Result<AsyncAgentProviderPollReport>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: *lifecycle.borrow(),
    };

    loop {
        let state = *lifecycle.borrow();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(config.max_tasks_per_poll)
            .await?;
        if dispatches.is_empty() {
            handle
                .queue_provider_poll_timer_if_needed(report.polls.saturating_add(1), 1)
                .await?;
        }
        if dispatches.is_empty() {
            report.idle_polls = report.idle_polls.saturating_add(1);
            report.polls = report.polls.saturating_add(1);
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            tokio::select! {
                _ = handle.wait_for_event_delivery() => {}
                changed = side_effect_watcher.changed() => {
                    let _ = changed;
                }
                changed = lifecycle.changed() => {
                    if changed.is_err() {
                        return Ok(report);
                    }
                }
                _ = sleep(config.idle_interval) => {}
            }
        } else {
            let executions = dispatch_agent_provider_side_effects(handle, dispatches).await?;
            report.executions = report.executions.saturating_add(executions);
            report.polls = report.polls.saturating_add(1);
        }
    }
}

/// Runs the dispatch agent provider side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn dispatch_agent_provider_side_effects(
    handle: &AsyncRuntimeSessionHandle,
    dispatches: Vec<RuntimeSideEffect>,
) -> Result<u64> {
    let mut worker_tasks = Vec::new();
    let mut compaction_tasks = Vec::new();
    for dispatch in dispatches {
        match dispatch {
            RuntimeSideEffect::DispatchAgentProvider { agent_id, turn_id } => {
                let Some(dispatch) = handle
                    .claim_configured_agent_provider_task(agent_id.clone(), turn_id.clone())
                    .await?
                else {
                    continue;
                };
                let task =
                    tokio::spawn(
                        async move { execute_runtime_agent_provider_dispatch(dispatch).await },
                    );
                worker_tasks.push((agent_id, turn_id, task));
            }
            RuntimeSideEffect::DispatchAgentCompaction { pane_id } => {
                let dispatch = match handle.claim_agent_compaction_task(pane_id.clone()).await {
                    Ok(Some(dispatch)) => dispatch,
                    Ok(None) => continue,
                    Err(error) => {
                        let mut batch = RuntimeEventBatch::new();
                        batch.push(RuntimeEvent::AgentCompaction(
                            AgentCompactionEvent::Failed {
                                pane_id,
                                kind: provider_worker_error_kind(&error).to_string(),
                                message: error.message().to_string(),
                                provider_failure_json: error
                                    .provider_failure_json()
                                    .map(str::to_string),
                                provider_raw_text: error.provider_raw_text().map(str::to_string),
                            },
                        ));
                        handle.submit_runtime_events(batch).await?;
                        continue;
                    }
                };
                let task = tokio::spawn(async move {
                    execute_runtime_agent_compaction_dispatch(dispatch).await
                });
                compaction_tasks.push((pane_id, task));
            }
            _ => {}
        }
    }

    let mut executions = 0u64;
    for (agent_id, turn_id, task) in worker_tasks {
        let Some((event, completed)) =
            await_agent_provider_worker(handle, agent_id, turn_id, task).await?
        else {
            continue;
        };
        if completed {
            executions = executions.saturating_add(1);
        }
        let mut batch = RuntimeEventBatch::new();
        batch.push(event);
        handle.submit_runtime_events(batch).await?;
    }
    for (pane_id, task) in compaction_tasks {
        let Some((event, completed)) = await_agent_compaction_worker(handle, pane_id, task).await?
        else {
            continue;
        };
        if completed {
            executions = executions.saturating_add(1);
        }
        let mut batch = RuntimeEventBatch::new();
        batch.push(event);
        handle.submit_runtime_events(batch).await?;
    }
    Ok(executions)
}

/// Waits for one provider worker while honoring turn cancellation.
///
/// A provider task is claimed before it begins request serialization and
/// network work. Once claimed, `/stop` removes the turn from runtime state,
/// so the async side must actively abort the worker instead of waiting for
/// it to finish and continue allocating memory for a cancelled turn.
async fn await_agent_provider_worker(
    handle: &AsyncRuntimeSessionHandle,
    agent_id: AgentId,
    turn_id: String,
    mut task: tokio::task::JoinHandle<Result<super::AgentTurnExecution>>,
) -> Result<Option<(RuntimeEvent, bool)>> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    if !handle.agent_turn_is_running(&turn_id).await? {
        task.abort();
        let _ = task.await;
        return Ok(None);
    }
    loop {
        tokio::select! {
            result = &mut task => {
                return Ok(Some(provider_worker_event(agent_id, turn_id, result)));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    task.abort();
                    let _ = task.await;
                    return Ok(None);
                }
            }
        }
        let lifecycle_state = *lifecycle.borrow();
        if is_terminal_runtime_lifecycle_state(lifecycle_state)
            || !handle.agent_turn_is_running(&turn_id).await?
        {
            task.abort();
            let _ = task.await;
            return Ok(None);
        }
    }
}

/// Waits for one model-backed compaction worker while honoring shutdown.
async fn await_agent_compaction_worker(
    handle: &AsyncRuntimeSessionHandle,
    pane_id: String,
    mut task: tokio::task::JoinHandle<Result<crate::agent::ModelResponse>>,
) -> Result<Option<(RuntimeEvent, bool)>> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    loop {
        tokio::select! {
            result = &mut task => {
                return Ok(Some(compaction_worker_event(pane_id, result)));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    task.abort();
                    let _ = task.await;
                    return Ok(None);
                }
            }
        }
        if is_terminal_runtime_lifecycle_state(*lifecycle.borrow()) {
            task.abort();
            let _ = task.await;
            return Ok(None);
        }
    }
}

/// Runs the provider worker event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_worker_event(
    agent_id: AgentId,
    turn_id: String,
    result: std::result::Result<Result<super::AgentTurnExecution>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(execution)) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
                agent_id,
                turn_id,
                execution: Box::new(execution),
            }),
            true,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind: "invalid_state".to_string(),
                message: format!("provider worker join failed: {error}"),
                provider_failure_json: None,
                provider_raw_text: None,
            }),
            false,
        ),
    }
}

/// Converts a compaction worker result into a runtime event.
fn compaction_worker_event(
    pane_id: String,
    result: std::result::Result<Result<crate::agent::ModelResponse>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(response)) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed {
                pane_id,
                response: Box::new(response),
            }),
            true,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                pane_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                pane_id,
                kind: "invalid_state".to_string(),
                message: format!("provider worker join failed: {error}"),
                provider_failure_json: None,
                provider_raw_text: None,
            }),
            false,
        ),
    }
}

/// Runs the execute runtime agent provider dispatch operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn execute_runtime_agent_provider_dispatch(
    dispatch: RuntimeAgentProviderDispatch,
) -> Result<super::AgentTurnExecution> {
    let RuntimeAgentProviderDispatch {
        turn,
        context,
        mut model_profile,
        auto_sizing,
        provider,
        permission_policy,
        session_approvals,
        path_scopes,
        subagent_scope,
        available_mcp_servers,
        available_mcp_tools,
    } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            if let Some(auto_sizing) = auto_sizing.as_ref() {
                let (selected_profile, _decision, _fallback) =
                    runtime_execute_auto_sizing_with_async_provider(
                        &provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await;
                model_profile = selected_profile;
            }
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &permission_policy,
                approvals: &session_approvals,
                path_scopes: path_scopes.as_ref(),
                subagent_scope: subagent_scope.as_ref(),
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
            };
            runner.run_turn_async(&mut ledger, turn, context).await
        }
    }
}

/// Executes one model-backed conversation compaction request.
async fn execute_runtime_agent_compaction_dispatch(
    dispatch: RuntimeAgentCompactionDispatch,
) -> Result<crate::agent::ModelResponse> {
    let RuntimeAgentCompactionDispatch { task, provider } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            provider.send_request_async(&task.request).await
        }
    }
}

/// Runs the provider worker error kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_worker_error_kind(error: &MezError) -> &'static str {
    match error.kind() {
        MezErrorKind::InvalidArgs => "invalid_args",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config => "config",
        MezErrorKind::Io => "io",
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::NotImplemented => "not_implemented",
    }
}
