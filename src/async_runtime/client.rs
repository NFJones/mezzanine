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
    AttachedTerminalFdRole, ClientStatusLine, DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, MezError, MouseAction, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, TerminalClientLoopAction,
    empty_attached_terminal_loop_report, is_terminal_runtime_lifecycle_state,
    merge_attached_terminal_loop_report, run_async_attached_terminal_client_loop,
    run_async_attached_terminal_client_loop_deferred_pane_io, sleep,
};
use crate::agent::{
    ActionResult, ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnRecord,
    AgentTurnState, AsyncModelProvider, ContextSourceKind, ModelMessage, ModelMessageRole,
    ModelProfile, ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey,
    ProviderErrorRetryClass, ReqwestProviderHttpTransport,
    execute_network_action_with_transport_async, provider_error_retry_class,
};
use crate::async_runtime::RenderInvalidationReason;
use crate::error::MezErrorKind;
use crate::ids::AgentId;
use crate::runtime::runtime_execute_auto_sizing_with_async_provider;
use crate::terminal::{TerminalFdInterest, TerminalStyleSpan};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

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
    let mut render_limiter =
        AttachedTerminalRenderRateLimiter::new(request.terminal_config.render_rate_limit_fps);

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
        render_limiter.set_rate_limit(request.terminal_config.render_rate_limit_fps);

        let wake = wait_for_attached_terminal_batch_readiness(
            handle,
            io,
            &request.client_id,
            render_requested,
            &mut render_limiter,
            &mut lifecycle_watcher,
        )
        .await?;
        render_requested = false;
        let pending_output_flush = matches!(wake, AttachedTerminalBatchWake::PendingOutputFlush);
        let size_check_only = matches!(
            wake,
            AttachedTerminalBatchWake::TerminalSizeCheck | AttachedTerminalBatchWake::StateChanged
        );
        let (mut readiness, invalidate_output_frame) = match wake {
            AttachedTerminalBatchWake::Readiness {
                readiness,
                invalidate_output_frame,
            } => (readiness, invalidate_output_frame),
            AttachedTerminalBatchWake::PendingOutputFlush => (Vec::new(), false),
            AttachedTerminalBatchWake::TerminalSizeCheck
            | AttachedTerminalBatchWake::StateChanged => (Vec::new(), false),
        };
        if invalidate_output_frame {
            io.invalidate_output_frame().await?;
        }

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
        if size_check_only && !resized_this_batch {
            continue;
        }
        if pending_output_flush && !resized_this_batch {
            let flush = io
                .flush_pending_output(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES)
                .await?;
            report.batches = report.batches.saturating_add(1);
            report.loop_report.bytes_written = report
                .loop_report
                .bytes_written
                .saturating_add(flush.bytes_written);
            report.loop_report.pending_output_bytes = flush.pending_bytes;
            if flush.is_partial() {
                report.loop_report.partial_writes =
                    report.loop_report.partial_writes.saturating_add(1);
            } else if flush.bytes_written > 0 {
                report.loop_report.output_frames =
                    report.loop_report.output_frames.saturating_add(1);
                render_limiter.mark_flushed();
            }
            continue;
        }
        if resized_this_batch {
            if !invalidate_output_frame {
                io.invalidate_output_frame().await?;
            }
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
        let batch_output_frames = batch.output_frames;
        let should_finish =
            batch.input_hangups > 0 || batch.output_hangups > 0 || !batch.error_roles.is_empty();
        resized_this_batch |= attached_terminal_actions_include_resize(&batch.actions);
        request.terminal_config.host_bracketed_paste_active = batch.host_bracketed_paste_active;
        request.terminal_config.host_bracketed_paste_buffer =
            batch.host_bracketed_paste_buffer.clone();
        merge_attached_terminal_loop_report(&mut report.loop_report, batch);
        if batch_output_frames > 0 {
            render_limiter.mark_flushed();
        }
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

/// Coalesces foreground render invalidations behind a per-client frame cadence.
#[derive(Debug, Clone)]
struct AttachedTerminalRenderRateLimiter {
    /// Minimum time between ordinary render invalidation flushes.
    min_interval: Option<Duration>,
    /// Last time this client wrote a rendered frame.
    last_flush_at: Option<Instant>,
    /// Whether a render invalidation is waiting for the next allowed frame.
    pending_render: bool,
}

impl AttachedTerminalRenderRateLimiter {
    /// Builds a limiter from the configured maximum frames per second.
    fn new(rate_limit_fps: u64) -> Self {
        Self {
            min_interval: render_rate_limit_interval(rate_limit_fps),
            last_flush_at: None,
            pending_render: false,
        }
    }

    /// Applies a refreshed render-rate configuration.
    fn set_rate_limit(&mut self, rate_limit_fps: u64) {
        self.min_interval = render_rate_limit_interval(rate_limit_fps);
        if self.min_interval.is_none() {
            self.pending_render = false;
        }
    }

    /// Records that a render invalidation arrived.
    fn request_render(&mut self) {
        self.pending_render = true;
    }

    /// Clears a queued render invalidation after an immediate render is chosen.
    fn clear_pending(&mut self) {
        self.pending_render = false;
    }

    /// Records a completed frame flush.
    fn mark_flushed(&mut self) {
        self.last_flush_at = Some(Instant::now());
        self.pending_render = false;
    }

    /// Consumes a pending render when the rate gate permits it.
    fn consume_ready_render(&mut self) -> bool {
        if !self.pending_render {
            return false;
        }
        if self
            .pending_render_delay()
            .is_some_and(|delay| !delay.is_zero())
        {
            return false;
        }
        self.pending_render = false;
        true
    }

    /// Returns the remaining delay before a pending render may flush.
    fn pending_render_delay(&self) -> Option<Duration> {
        if !self.pending_render {
            return None;
        }
        let Some(min_interval) = self.min_interval else {
            return Some(Duration::ZERO);
        };
        let Some(last_flush_at) = self.last_flush_at else {
            return Some(Duration::ZERO);
        };
        let next_allowed = last_flush_at + min_interval;
        Some(next_allowed.saturating_duration_since(Instant::now()))
    }
}

/// Converts a frame-rate limit into a minimum interval between render flushes.
fn render_rate_limit_interval(rate_limit_fps: u64) -> Option<Duration> {
    if rate_limit_fps == 0 {
        return None;
    }
    Some(Duration::from_millis(
        1_000u64.saturating_add(rate_limit_fps.saturating_sub(1)) / rate_limit_fps,
    ))
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
    Readiness {
        /// File-descriptor readiness that should drive the next client batch.
        readiness: Vec<AttachedTerminalFdReadiness>,
        /// Whether retained differential output must be discarded first.
        invalidate_output_frame: bool,
    },
    /// Wake only to flush retained bytes from an already-composed output frame.
    ///
    /// This keeps terminal write readiness separate from render invalidation so
    /// slow clients can complete an unsuperseded partial frame without asking
    /// the runtime actor to compose a new view.
    PendingOutputFlush,
    /// Wake only to compare the attached terminal's current dimensions.
    ///
    /// This lets foreground clients notice terminal-emulator resize or zoom
    /// changes that do not arrive as input/runtime events without turning idle
    /// waits into repeated redraws.
    TerminalSizeCheck,
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
    render_limiter: &mut AttachedTerminalRenderRateLimiter,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
) -> Result<AttachedTerminalBatchWake>
where
    I: AsyncAttachedTerminalIo,
{
    if render_requested {
        return Ok(AttachedTerminalBatchWake::Readiness {
            readiness: vec![synthetic_output_readiness()],
            invalidate_output_frame: false,
        });
    }

    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    loop {
        let render = drain_render_request_for_client(handle, client_id, render_limiter).await?;
        if render.ready {
            return Ok(AttachedTerminalBatchWake::Readiness {
                readiness: vec![synthetic_output_readiness()],
                invalidate_output_frame: render.invalidate_output_frame,
            });
        }
        if let Some(render_delay) = render_limiter.pending_render_delay() {
            tokio::select! {
                biased;
                readiness = io.poll_input_readiness() => {
                    let mut readiness = readiness?;
                    if readiness.iter().any(attached_terminal_readiness_is_readable_input_or_control) {
                        ensure_output_readiness(&mut readiness);
                    }
                    return Ok(AttachedTerminalBatchWake::Readiness {
                        readiness,
                        invalidate_output_frame: false,
                    });
                }
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    return Ok(AttachedTerminalBatchWake::StateChanged);
                }
                _ = handle.wait_for_event_delivery() => {
                    let render = drain_render_request_for_client(handle, client_id, render_limiter).await?;
                    if render.ready {
                        return Ok(AttachedTerminalBatchWake::Readiness {
                            readiness: vec![synthetic_output_readiness()],
                            invalidate_output_frame: render.invalidate_output_frame,
                        });
                    }
                }
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    let render = drain_render_request_for_client(handle, client_id, render_limiter).await?;
                    if render.ready {
                        return Ok(AttachedTerminalBatchWake::Readiness {
                            readiness: vec![synthetic_output_readiness()],
                            invalidate_output_frame: render.invalidate_output_frame,
                        });
                    }
                }
                _ = sleep(render_delay) => {
                    if render_limiter.consume_ready_render() {
                        return Ok(AttachedTerminalBatchWake::Readiness {
                            readiness: vec![synthetic_output_readiness()],
                            invalidate_output_frame: false,
                        });
                    }
                }
                _ = sleep(DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT) => {
                    return Ok(AttachedTerminalBatchWake::TerminalSizeCheck);
                }
            }
            continue;
        }

        if io.pending_output_bytes() > 0 {
            let mut readiness = io.poll_readiness().await?;
            if readiness
                .iter()
                .any(attached_terminal_readiness_is_readable_input_or_control)
            {
                ensure_output_readiness(&mut readiness);
                return Ok(AttachedTerminalBatchWake::Readiness {
                    readiness,
                    invalidate_output_frame: false,
                });
            }
            if readiness.iter().any(|ready| {
                ready.role == AttachedTerminalFdRole::Output && (ready.hangup || ready.error)
            }) {
                return Ok(AttachedTerminalBatchWake::Readiness {
                    readiness,
                    invalidate_output_frame: false,
                });
            }
            if readiness
                .iter()
                .any(|ready| ready.role == AttachedTerminalFdRole::Output && ready.writable)
            {
                return Ok(AttachedTerminalBatchWake::PendingOutputFlush);
            }
        }

        tokio::select! {
            biased;
            readiness = io.poll_input_readiness() => {
                let mut readiness = readiness?;
                if readiness.iter().any(attached_terminal_readiness_is_readable_input_or_control) {
                    ensure_output_readiness(&mut readiness);
                }
                return Ok(AttachedTerminalBatchWake::Readiness {
                    readiness,
                    invalidate_output_frame: false,
                });
            }
            _ = handle.wait_for_event_delivery() => {
                let render = drain_render_request_for_client(handle, client_id, render_limiter).await?;
                if render.ready {
                    return Ok(AttachedTerminalBatchWake::Readiness {
                        readiness: vec![synthetic_output_readiness()],
                        invalidate_output_frame: render.invalidate_output_frame,
                    });
                }
                return Ok(AttachedTerminalBatchWake::StateChanged);
            }
            result = side_effect_watcher.changed() => {
                let _ = result;
                let render = drain_render_request_for_client(handle, client_id, render_limiter).await?;
                if render.ready {
                    return Ok(AttachedTerminalBatchWake::Readiness {
                        readiness: vec![synthetic_output_readiness()],
                        invalidate_output_frame: render.invalidate_output_frame,
                    });
                }
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
                return Ok(AttachedTerminalBatchWake::StateChanged);
            }
            _ = sleep(DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT) => {
                return Ok(AttachedTerminalBatchWake::TerminalSizeCheck);
            }
        }
    }
}

/// Drains queued render invalidations for one client into the rate limiter.
async fn drain_render_request_for_client(
    handle: &AsyncRuntimeSessionHandle,
    client_id: &super::ClientId,
    render_limiter: &mut AttachedTerminalRenderRateLimiter,
) -> Result<AttachedTerminalRenderDrain> {
    let render_effects = handle
        .drain_render_side_effects_for_client(client_id.clone(), 8)
        .await?;
    let Some(reason) = strongest_render_invalidation_reason(&render_effects) else {
        return Ok(AttachedTerminalRenderDrain::default());
    };
    if render_invalidation_reason_bypasses_rate_limit(reason) {
        render_limiter.clear_pending();
        return Ok(AttachedTerminalRenderDrain {
            ready: true,
            invalidate_output_frame: render_invalidation_reason_invalidates_frame(reason),
        });
    }
    render_limiter.request_render();
    Ok(AttachedTerminalRenderDrain {
        ready: render_limiter.consume_ready_render(),
        invalidate_output_frame: false,
    })
}

/// Render invalidation drained for one attached client.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AttachedTerminalRenderDrain {
    /// Whether the render should run in this batch.
    ready: bool,
    /// Whether retained differential output must be discarded first.
    invalidate_output_frame: bool,
}

/// Returns the strongest render invalidation reason from drained side effects.
fn strongest_render_invalidation_reason(
    effects: &[RuntimeSideEffect],
) -> Option<RenderInvalidationReason> {
    effects
        .iter()
        .filter_map(render_invalidation_reason)
        .fold(None, |current, incoming| {
            Some(match current {
                Some(current) => strongest_client_render_invalidation_reason(current, incoming),
                None => incoming,
            })
        })
}

/// Extracts the render invalidation reason from one side effect.
fn render_invalidation_reason(effect: &RuntimeSideEffect) -> Option<RenderInvalidationReason> {
    match effect {
        RuntimeSideEffect::RenderClient { reason, .. } => Some(*reason),
        _ => None,
    }
}

/// Chooses the stronger client render invalidation reason.
fn strongest_client_render_invalidation_reason(
    current: RenderInvalidationReason,
    incoming: RenderInvalidationReason,
) -> RenderInvalidationReason {
    if client_render_invalidation_priority(current) >= client_render_invalidation_priority(incoming)
    {
        current
    } else {
        incoming
    }
}

/// Priority used by the attached client when coalescing drained render requests.
fn client_render_invalidation_priority(reason: RenderInvalidationReason) -> u8 {
    match reason {
        RenderInvalidationReason::CursorBlink => 0,
        RenderInvalidationReason::StatusLine => 1,
        RenderInvalidationReason::PaneOutput => 2,
        RenderInvalidationReason::AgentPrompt => 3,
        RenderInvalidationReason::Overlay => 4,
        RenderInvalidationReason::Configuration => 5,
        RenderInvalidationReason::Resize => 6,
        RenderInvalidationReason::Layout => 7,
        RenderInvalidationReason::FullRedraw => 8,
    }
}

/// Returns whether a render invalidation should bypass the output rate gate.
fn render_invalidation_reason_bypasses_rate_limit(reason: RenderInvalidationReason) -> bool {
    matches!(
        reason,
        RenderInvalidationReason::Resize
            | RenderInvalidationReason::Layout
            | RenderInvalidationReason::FullRedraw
    )
}

/// Returns whether a render invalidation invalidates retained output state.
fn render_invalidation_reason_invalidates_frame(reason: RenderInvalidationReason) -> bool {
    matches!(
        reason,
        RenderInvalidationReason::Resize
            | RenderInvalidationReason::Layout
            | RenderInvalidationReason::FullRedraw
    )
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
                .queue_provider_poll_timer_if_needed(
                    report.polls.saturating_add(1),
                    config.provider_poll_fallback_delay_ms(),
                )
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
        auto_sizing_provider,
        auto_sizing_target_providers,
        provider,
        permission_policy,
        session_approvals,
        path_scopes,
        subagent_scope,
        available_mcp_servers,
        available_mcp_tools,
        loop_turn: _,
    } = dispatch;
    let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
    let mut selected_auto_sizing_profile: Option<ModelProfile> = None;
    let loop_allowed_actions = None;
    if let Some(auto_sizing) = auto_sizing.as_ref()
        && let Some(auto_sizing_provider) = auto_sizing_provider.as_ref()
    {
        match auto_sizing_provider {
            RuntimeAgentProviderDispatchProvider::OpenAi(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                selected_auto_sizing_profile = Some(auto_sizing_execution.selected_profile);
            }
            RuntimeAgentProviderDispatchProvider::DeepSeek(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                selected_auto_sizing_profile = Some(auto_sizing_execution.selected_profile);
            }
            RuntimeAgentProviderDispatchProvider::OpenAiCompatible(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                selected_auto_sizing_profile = Some(auto_sizing_execution.selected_profile);
            }
        }
    }
    let mut provider = provider;
    if selected_auto_sizing_profile.is_none()
        && let Some(auto_sizing) = auto_sizing.as_ref()
        && auto_sizing.router_profile.provider == provider.provider_id()
    {
        selected_auto_sizing_profile = match &provider {
            RuntimeAgentProviderDispatchProvider::OpenAi(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                Some(auto_sizing_execution.selected_profile)
            }
            RuntimeAgentProviderDispatchProvider::DeepSeek(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                Some(auto_sizing_execution.selected_profile)
            }
            RuntimeAgentProviderDispatchProvider::OpenAiCompatible(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                Some(auto_sizing_execution.selected_profile)
            }
        };
    }
    (provider, model_profile) = runtime_provider_dispatch_after_auto_sizing(
        provider,
        model_profile,
        selected_auto_sizing_profile,
        &auto_sizing_target_providers,
    );
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
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
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    loop_allowed_actions.clone(),
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(execution)
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
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
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    loop_allowed_actions.clone(),
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(execution)
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
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
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    loop_allowed_actions.clone(),
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(execution)
        }
    }
}

/// Applies a router-selected profile to the async provider dispatch.
///
/// Async provider workers run after the runtime actor has already constructed
/// concrete provider clients. When automatic sizing selects a profile owned by
/// a different configured provider, the worker must switch to the carried
/// target provider instead of silently keeping the originally active provider.
fn runtime_provider_dispatch_after_auto_sizing(
    provider: RuntimeAgentProviderDispatchProvider,
    current_profile: ModelProfile,
    selected_profile: Option<ModelProfile>,
    target_providers: &std::collections::BTreeMap<String, RuntimeAgentProviderDispatchProvider>,
) -> (RuntimeAgentProviderDispatchProvider, ModelProfile) {
    let Some(selected_profile) = selected_profile else {
        return (provider, current_profile);
    };
    if selected_profile.provider == provider.provider_id() {
        return (provider, selected_profile);
    }
    if let Some(target_provider) = target_providers.get(&selected_profile.provider).cloned() {
        return (target_provider, selected_profile);
    }
    (provider, current_profile)
}

/// Executes runtime-owned network actions before returning provider work to the
/// actor.
///
/// Provider workers already run outside the single-owner session actor. Keeping
/// `fetch_url` and `web_search` HTTP there prevents a large research batch from
/// monopolizing the actor while still returning ordinary action results for the
/// actor to present, audit, persist, and feed into any continuation request.
pub(super) async fn execute_provider_worker_network_actions(
    turn: &AgentTurnRecord,
    mut execution: AgentTurnExecution,
) -> Result<AgentTurnExecution> {
    if execution.terminal_state != AgentTurnState::Running {
        return Ok(execution);
    }
    let Some(batch) = execution.response.action_batch.clone() else {
        return Ok(execution);
    };
    let transport = ReqwestProviderHttpTransport;
    for index in 0..execution.action_results.len() {
        if execution.action_results[index].status != ActionStatus::Running
            || !matches!(
                execution.action_results[index].action_type,
                "web_search" | "fetch_url"
            )
        {
            continue;
        }
        let action_id = execution.action_results[index].action_id.clone();
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("running network result does not match an action")
            })?;
        if !matches!(
            action.payload,
            AgentActionPayload::WebSearch { .. } | AgentActionPayload::FetchUrl { .. }
        ) {
            continue;
        }
        execution.action_results[index] =
            execute_network_action_with_transport_async(turn, &action, &transport).await?;
    }
    execution.terminal_state =
        async_agent_turn_state_from_action_results(&execution.action_results, execution.final_turn);
    Ok(execution)
}

/// Computes the provider-worker terminal state after network actions settle.
fn async_agent_turn_state_from_action_results(
    results: &[ActionResult],
    final_turn: bool,
) -> AgentTurnState {
    if results
        .iter()
        .any(|result| result.status == ActionStatus::Blocked)
    {
        AgentTurnState::Blocked
    } else if results.iter().any(|result| result.is_error) {
        AgentTurnState::Failed
    } else if results
        .iter()
        .any(|result| result.status == ActionStatus::Running)
    {
        AgentTurnState::Running
    } else if final_turn
        || (!results.is_empty()
            && results
                .iter()
                .all(|result| matches!(result.action_type, "complete")))
    {
        AgentTurnState::Completed
    } else {
        AgentTurnState::Running
    }
}

/// Applies auto-sizing with the main provider when no separate router provider
/// was constructed for the turn.
#[cfg(test)]
async fn runtime_apply_same_provider_auto_sizing_if_needed<P: AsyncModelProvider>(
    provider: &P,
    current_profile: ModelProfile,
    has_separate_router_provider: bool,
    auto_sizing: Option<&crate::runtime::RuntimeAutoSizingDispatch>,
    turn: &AgentTurnRecord,
    context: &crate::agent::AgentContext,
) -> (
    ModelProfile,
    std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) {
    if has_separate_router_provider {
        return (current_profile, std::collections::BTreeMap::new());
    }
    let Some(auto_sizing) = auto_sizing else {
        return (current_profile, std::collections::BTreeMap::new());
    };
    let current_provider = current_profile.provider.clone();
    if auto_sizing.router_profile.provider != current_provider {
        return (current_profile, std::collections::BTreeMap::new());
    }
    let auto_sizing_execution =
        runtime_execute_auto_sizing_with_async_provider(provider, auto_sizing, turn, context).await;
    let usage_by_model = auto_sizing_execution.token_usage_by_model();
    if auto_sizing_execution.selected_profile.provider == current_provider {
        (auto_sizing_execution.selected_profile, usage_by_model)
    } else {
        (current_profile, usage_by_model)
    }
}

/// Merges per-model token usage maps with saturating counters.
fn merge_model_token_usage_by_model(
    target: &mut std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    source: std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) {
    for (key, usage) in source {
        target.entry(key).or_default().add_assign(usage);
    }
}

/// Executes one model-backed conversation compaction request.
async fn execute_runtime_agent_compaction_dispatch(
    dispatch: RuntimeAgentCompactionDispatch,
) -> Result<crate::agent::ModelResponse> {
    let RuntimeAgentCompactionDispatch { task, provider } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
    }
}

/// Sends a model compaction request and retries once with stricter output
/// guidance when the provider cuts off generation at its output-token limit.
async fn runtime_send_compaction_request_with_output_limit_retry<P: AsyncModelProvider>(
    provider: &P,
    mut request: ModelRequest,
    model_profile: &ModelProfile,
) -> Result<ModelResponse> {
    match provider.send_request_async(&request).await {
        Ok(response) => Ok(response),
        Err(error)
            if matches!(
                provider_error_retry_class(&error),
                ProviderErrorRetryClass::OutputLimit
            ) =>
        {
            request =
                runtime_agent_compaction_request_with_output_limit_retry(request, model_profile);
            request.messages.push(ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::Configuration,
                content: "[ephemeral compaction output-limit retry]\n\
                    The previous compaction response was incomplete because generation hit max_output_tokens. \
                    Return exactly one final say action containing a compact durable summary. \
                    Keep the summary brief: preserve only active goals, decisions, changed files, validations, blockers, and next steps. \
                    Do not include full logs, transcript excerpts, plans, or explanations. \
                    This retry instruction is not durable transcript or future-turn context."
                    .to_string(),
            });
            provider.send_request_async(&request).await
        }
        Err(error) => Err(error),
    }
}

/// Returns a compaction request with an escalated output cap for any retry.
fn runtime_agent_compaction_request_with_output_limit_retry(
    mut request: ModelRequest,
    model_profile: &ModelProfile,
) -> ModelRequest {
    request.max_output_tokens = Some(model_profile.output_limit_retry_tokens());
    request
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentContext, AgentTurnRecord, AgentTurnState, AgentTurnTrigger, ContextBlock,
        ContextSourceKind, ModelInteractionKind, ModelRequest, ModelResponse,
    };
    use crate::runtime::{
        RuntimeAutoSizingDispatch, RuntimeAutoSizingFallbackPolicy, RuntimeAutoSizingTargetProfile,
    };
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    /// Records async provider requests and returns a valid auto-sizing router
    /// decision. This isolates same-provider routing from network transport so
    /// the DeepSeek dispatch regression is covered without live provider I/O.
    #[derive(Clone, Default)]
    struct SameProviderAutoSizingProvider {
        /// Provider requests observed by the test.
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl AsyncModelProvider for SameProviderAutoSizingProvider {
        /// Returns the test provider id used by the dispatch profile.
        fn provider_id(&self) -> &str {
            "deepseek"
        }

        /// Records the request and returns an internal router decision.
        fn send_request_async<'a>(
            &'a self,
            request: &'a ModelRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                Ok(ModelResponse {
                    provider: self.provider_id().to_string(),
                    model: request.model.clone(),
                    raw_text: r#"{"version":1,"size":"large","reasoning_effort":"xhigh","confidence":0.91,"rationale":"deep review"}"#.to_string(),
                    usage: ModelTokenUsage {
                        input_tokens: 31,
                        output_tokens: 7,
                        reasoning_tokens: 3,
                        cached_input_tokens: Some(11),
                    },
                    latest_request_usage: None,
                    quota_usage: Vec::new(),
                    action_batch: None,
                    provider_transcript_events: Vec::new(),
                })
            })
        }
    }

    /// Records compaction provider requests while failing the first one with an
    /// output-token-limit error. This keeps compaction retry coverage local to
    /// the async worker helper instead of depending on live provider I/O.
    #[derive(Clone, Default)]
    struct CompactionOutputLimitThenSuccessProvider {
        /// Provider requests observed by the retry helper.
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl AsyncModelProvider for CompactionOutputLimitThenSuccessProvider {
        /// Returns the test provider id used by the compaction request.
        fn provider_id(&self) -> &str {
            "openai"
        }

        /// Fails once with `max_output_tokens` and succeeds after retry.
        fn send_request_async<'a>(
            &'a self,
            request: &'a ModelRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
            Box::pin(async move {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.clone());
                if requests.len() == 1 {
                    return Err(MezError::invalid_state(
                        "OpenAI stream returned an incomplete response: max_output_tokens",
                    )
                    .with_provider_failure_json(
                        r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#,
                    ));
                }
                Ok(ModelResponse {
                    provider: self.provider_id().to_string(),
                    model: request.model.clone(),
                    raw_text: "compacted after output-limit retry".to_string(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Vec::new(),
                    action_batch: None,
                    provider_transcript_events: Vec::new(),
                })
            })
        }
    }

    /// Builds one model profile for same-provider auto-sizing tests.
    fn test_model_profile(model: &str, reasoning: Option<&str>) -> ModelProfile {
        ModelProfile {
            provider: "deepseek".to_string(),
            model: model.to_string(),
            reasoning_profile: reasoning.map(str::to_string),
            latency_preference: None,
            multimodal_required: false,
            provider_options: BTreeMap::new(),
            safety_tier: None,
        }
    }

    /// Builds one auto-sizing target profile for the synthetic dispatch.
    fn test_auto_sizing_target(
        size: &str,
        profile_name: &str,
        model: &str,
    ) -> RuntimeAutoSizingTargetProfile {
        RuntimeAutoSizingTargetProfile {
            size: size.to_string(),
            profile_name: profile_name.to_string(),
            profile: test_model_profile(model, Some("high")),
            supported_reasoning_efforts: vec!["high".to_string(), "xhigh".to_string()],
        }
    }

    /// Builds the auto-sizing dispatch used by same-provider routing tests.
    fn test_auto_sizing_dispatch() -> RuntimeAutoSizingDispatch {
        RuntimeAutoSizingDispatch {
            router_profile_name: "deepseek-fast".to_string(),
            router_profile: test_model_profile("deepseek-v4-flash", Some("high")),
            default_profile_name: "deepseek-default".to_string(),
            default_profile: test_model_profile("deepseek-v4-pro", Some("high")),
            small: test_auto_sizing_target("small", "deepseek-fast", "deepseek-v4-flash"),
            medium: test_auto_sizing_target("medium", "deepseek-default", "deepseek-v4-pro"),
            large: test_auto_sizing_target("large", "deepseek-default", "deepseek-v4-pro"),
            turn_metadata: None,
            allowed_reasoning_efforts: vec!["high".to_string(), "xhigh".to_string()],
            fallback_policy: RuntimeAutoSizingFallbackPolicy::UseDefaultProfile,
        }
    }

    /// Verifies async provider dispatch switches to a carried target provider
    /// when routing selects a profile owned by a different provider id.
    ///
    /// The async worker runs after the runtime actor has already built concrete
    /// provider clients. Cross-provider auto-sizing decisions therefore need to
    /// replace both the effective model profile and provider client before the
    /// normal provider request is sent, otherwise routing silently falls back to
    /// the previously active provider.
    #[test]
    fn auto_sizing_dispatch_switches_to_selected_target_provider() {
        let current_provider = crate::agent::OpenAiResponsesProvider::without_auth(
            "http://127.0.0.1/current",
            1_000,
            BTreeMap::new(),
            false,
            ReqwestProviderHttpTransport,
        )
        .unwrap()
        .with_provider_id("current")
        .unwrap();
        let target_provider = crate::agent::OpenAiResponsesProvider::without_auth(
            "http://127.0.0.1/target",
            1_000,
            BTreeMap::new(),
            false,
            ReqwestProviderHttpTransport,
        )
        .unwrap()
        .with_provider_id("target")
        .unwrap();
        let mut target_providers = BTreeMap::new();
        target_providers.insert(
            "target".to_string(),
            RuntimeAgentProviderDispatchProvider::OpenAi(target_provider),
        );
        let current_profile = ModelProfile {
            provider: "current".to_string(),
            model: "current-model".to_string(),
            reasoning_profile: Some("medium".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: BTreeMap::new(),
            safety_tier: None,
        };
        let selected_profile = ModelProfile {
            provider: "target".to_string(),
            model: "target-model".to_string(),
            reasoning_profile: Some("high".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: BTreeMap::new(),
            safety_tier: None,
        };

        let (provider, profile) = runtime_provider_dispatch_after_auto_sizing(
            RuntimeAgentProviderDispatchProvider::OpenAi(current_provider),
            current_profile,
            Some(selected_profile),
            &target_providers,
        );

        assert_eq!(provider.provider_id(), "target");
        assert_eq!(profile.provider, "target");
        assert_eq!(profile.model, "target-model");
        assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    }

    /// Verifies model-backed compaction retries output-limit provider failures
    /// with stricter summary guidance and an escalated output cap.
    ///
    /// Auto compaction runs through a standalone async worker rather than the
    /// active-turn provider retry path. This regression protects OpenAI
    /// `response.incomplete/max_output_tokens` failures from becoming terminal
    /// compaction failures before a compact second request can succeed.
    #[tokio::test]
    async fn compaction_output_limit_failure_retries_with_compact_guidance() {
        let provider = CompactionOutputLimitThenSuccessProvider::default();
        let mut model_profile = ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-compact-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: BTreeMap::new(),
            safety_tier: None,
        };
        model_profile
            .provider_options
            .insert("max_output_tokens".to_string(), "4096".to_string());
        let request = ModelRequest {
            provider: model_profile.provider.clone(),
            model: model_profile.model.clone(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: model_profile.max_output_tokens(),
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "compact-conversation".to_string(),
            agent_id: "agent-%1".to_string(),
            available_mcp_tools: Vec::new(),
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::say_only(),
            messages: Vec::new(),
        };

        let response = runtime_send_compaction_request_with_output_limit_retry(
            &provider,
            request,
            &model_profile,
        )
        .await
        .unwrap();

        assert_eq!(response.raw_text, "compacted after output-limit retry");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].max_output_tokens, Some(4096));
        assert_eq!(requests[0].messages.len(), 0);
        assert_eq!(requests[1].max_output_tokens, Some(16_384));
        assert!(requests[1].messages.iter().any(|message| {
            message.content.contains("compaction output-limit retry")
                && message
                    .content
                    .contains("Return exactly one final say action")
        }));
    }

    /// Verifies that same-provider automatic sizing is not OpenAI-specific.
    ///
    /// The async dispatch path used to execute same-provider routing only for
    /// the OpenAI branch. This regression covers the factored helper used by
    /// both OpenAI and DeepSeek provider branches and proves a DeepSeek-shaped
    /// provider receives the internal router request and applies the selected
    /// canonical `xhigh` reasoning effort.
    #[tokio::test]
    async fn same_provider_auto_sizing_applies_to_deepseek_provider() {
        let provider = SameProviderAutoSizingProvider::default();
        let turn = AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "deepseek-default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
        };
        let context = AgentContext {
            blocks: vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "latest user prompt".to_string(),
                content: "deeply inspect the codebase".to_string(),
            }],
        };
        let (selected, routing_token_usage_by_model) =
            runtime_apply_same_provider_auto_sizing_if_needed(
                &provider,
                test_model_profile("deepseek-v4-pro", Some("high")),
                false,
                Some(&test_auto_sizing_dispatch()),
                &turn,
                &context,
            )
            .await;

        assert!(!routing_token_usage_by_model.is_empty());
        assert_eq!(selected.provider, "deepseek");
        assert_eq!(selected.model, "deepseek-v4-pro");
        assert_eq!(selected.reasoning_profile.as_deref(), Some("xhigh"));
        assert_eq!(
            selected
                .provider_options
                .get("reasoning_effort")
                .map(String::as_str),
            Some("xhigh")
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].interaction_kind,
            ModelInteractionKind::AutoSizing
        );
        assert_eq!(requests[0].provider, "deepseek");
        assert!(requests[0].turn_id.ends_with(":auto-sizing"));
    }

    /// Verifies that same-provider auto-sizing does not send a router request
    /// through the active provider when the router profile belongs to another
    /// provider.
    ///
    /// Subagent panes can combine an inherited DeepSeek model profile with a
    /// stale or default OpenAI auto-sizing configuration when pane-local sizing
    /// state is not inherited correctly. This regression keeps that mismatch
    /// from being sent through the DeepSeek provider, which would fail before
    /// the actual child-agent turn can run.
    #[tokio::test]
    async fn same_provider_auto_sizing_skips_mismatched_router_provider() {
        let provider = SameProviderAutoSizingProvider::default();
        let mut auto_sizing = test_auto_sizing_dispatch();
        auto_sizing.router_profile.provider = "openai".to_string();
        auto_sizing.small.profile.provider = "openai".to_string();
        auto_sizing.medium.profile.provider = "openai".to_string();
        auto_sizing.large.profile.provider = "openai".to_string();
        let turn = AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "deepseek-default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
        };
        let context = AgentContext {
            blocks: vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "latest user prompt".to_string(),
                content: "generate a random number".to_string(),
            }],
        };
        let (selected, routing_token_usage_by_model) =
            runtime_apply_same_provider_auto_sizing_if_needed(
                &provider,
                test_model_profile("deepseek-v4-pro", Some("high")),
                false,
                Some(&auto_sizing),
                &turn,
                &context,
            )
            .await;

        assert!(routing_token_usage_by_model.is_empty());
        assert_eq!(selected.provider, "deepseek");
        assert_eq!(selected.model, "deepseek-v4-pro");
        assert!(
            provider.requests.lock().unwrap().is_empty(),
            "mismatched router requests must not be sent to the active provider"
        );
    }
}
