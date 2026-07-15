//! Async Runtime Client implementation.
//!
//! This module owns the async runtime client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentCompactionEvent, AgentProviderEvent, AgentRememberEvent, AgentTurnLedger, AgentTurnRunner,
    AsyncAgentProviderPollReport, AsyncAgentProviderServiceConfig, AsyncAttachedTerminalIo,
    AsyncAttachedTerminalLoopRequest, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle, AsyncTerminalIoFuture, AsyncTerminalOutputWriteReport,
    AttachedTerminalClientLoopReport, AttachedTerminalFdReadiness, AttachedTerminalFdRole,
    ClientStatusLine, DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, MezError, MouseAction, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentRememberDispatch, RuntimeEvent,
    RuntimeEventBatch, RuntimeLifecycleState, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    TerminalClientLoopAction, empty_attached_terminal_loop_report,
    is_terminal_runtime_lifecycle_state, merge_attached_terminal_loop_report,
    run_async_attached_terminal_client_loop, sleep,
};
use crate::agent::{
    ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    AsyncModelProvider, ContextSourceKind, ModelMessage, ModelMessageRole, ModelProfile,
    ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey, ProviderErrorRetryClass,
    ReqwestProviderHttpTransport, execute_network_action_with_transport_async,
    provider_error_retry_class,
};
use crate::async_runtime::RenderInvalidationReason;
use crate::error::MezErrorKind;
use crate::runtime::runtime_execute_auto_sizing_with_async_provider;
use crate::terminal::TerminalFdInterest;
use mez_core::ids::AgentId;
use mez_terminal::TerminalStyleSpan;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinSet;
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
    let mut request = request;
    let mut status_provider = status_provider;
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
            request.role == super::ClientViewRole::Primary,
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
        let batch = run_async_attached_terminal_client_loop(
            handle,
            &mut prepolled_io,
            request.clone(),
            |iteration| status_provider(iteration_offset.saturating_add(iteration)),
        )
        .await?;
        report.batches = report.batches.saturating_add(1);
        let batch_output_frames = batch.output_frames;
        let should_finish =
            batch.input_hangups > 0 || batch.output_hangups > 0 || !batch.error_roles.is_empty();
        resized_this_batch |= attached_terminal_actions_include_resize(&batch.actions);
        request.terminal_config.host_bracketed_paste_active = batch.host_bracketed_paste_active;
        request.terminal_config.host_bracketed_paste_buffer =
            batch.host_bracketed_paste_buffer.clone();
        request.terminal_config.host_bracketed_paste_started_at =
            batch.host_bracketed_paste_started_at;
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
    poll_terminal_size: bool,
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
                _ = sleep(DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT), if poll_terminal_size => {
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
            _ = sleep(DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT), if poll_terminal_size => {
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
    let mut workers = JoinSet::new();
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: *lifecycle.borrow(),
    };

    loop {
        let state = *lifecycle.borrow();
        report.terminal_state = state;
        drain_completed_agent_provider_workers(&mut workers, handle, &mut report).await?;
        if should_stop(report.polls, state) {
            abort_agent_provider_workers(&mut workers).await;
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
                abort_agent_provider_workers(&mut workers).await;
                return Ok(report);
            }
            if let Some(joined) = wait_for_agent_provider_worker_wakeup(
                handle,
                &mut workers,
                &mut lifecycle,
                &mut side_effect_watcher,
                config.idle_interval,
            )
            .await
            {
                record_joined_agent_provider_worker(joined, handle, &mut report).await?;
            }
        } else {
            dispatch_agent_provider_side_effects(handle, dispatches, &mut workers).await?;
            drain_completed_agent_provider_workers(&mut workers, handle, &mut report).await?;
            report.polls = report.polls.saturating_add(1);
        }
    }
}

type AsyncAgentProviderWorkerResult = Option<(RuntimeEvent, bool)>;

/// Drains provider workers that completed without blocking new dispatch claims.
async fn drain_completed_agent_provider_workers(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    handle: &AsyncRuntimeSessionHandle,
    report: &mut AsyncAgentProviderPollReport,
) -> Result<()> {
    while let Some(joined) = workers.try_join_next() {
        record_joined_agent_provider_worker(joined, handle, report).await?;
    }
    Ok(())
}

/// Records one completed provider worker and applies its runtime event.
async fn record_joined_agent_provider_worker(
    joined: std::result::Result<Result<AsyncAgentProviderWorkerResult>, tokio::task::JoinError>,
    handle: &AsyncRuntimeSessionHandle,
    report: &mut AsyncAgentProviderPollReport,
) -> Result<()> {
    match joined {
        Ok(Ok(Some((event, completed)))) => {
            if completed {
                report.executions = report.executions.saturating_add(1);
            }
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            handle.submit_runtime_events(batch).await?;
            Ok(())
        }
        Ok(Ok(None)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(MezError::invalid_state(format!(
            "async agent provider worker task failed: {error}"
        ))),
    }
}

/// Waits until provider work, actor events, lifecycle changes, or idle probing
/// should return control to the provider service loop.
async fn wait_for_agent_provider_worker_wakeup(
    handle: &AsyncRuntimeSessionHandle,
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    idle_interval: Duration,
) -> Option<std::result::Result<Result<AsyncAgentProviderWorkerResult>, tokio::task::JoinError>> {
    if workers.is_empty() {
        tokio::select! {
            _ = handle.wait_for_event_delivery() => None,
            changed = side_effect_watcher.changed() => {
                let _ = changed;
                None
            }
            changed = lifecycle_watcher.changed() => {
                let _ = changed;
                None
            }
            _ = sleep(idle_interval) => None,
        }
    } else {
        tokio::select! {
            biased;
            joined = workers.join_next() => joined,
            _ = handle.wait_for_event_delivery() => None,
            changed = side_effect_watcher.changed() => {
                let _ = changed;
                None
            }
            changed = lifecycle_watcher.changed() => {
                let _ = changed;
                None
            }
            _ = sleep(idle_interval) => None,
        }
    }
}

/// Aborts provider workers before the provider service exits.
async fn abort_agent_provider_workers(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
) {
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}

/// Runs the dispatch agent provider side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn dispatch_agent_provider_side_effects(
    handle: &AsyncRuntimeSessionHandle,
    dispatches: Vec<RuntimeSideEffect>,
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
) -> Result<()> {
    for dispatch in dispatches {
        match dispatch {
            RuntimeSideEffect::DispatchAgentProvider { agent_id, turn_id } => {
                let Some(dispatch) = handle
                    .claim_configured_agent_provider_task(agent_id.clone(), turn_id.clone())
                    .await?
                else {
                    continue;
                };
                workers.spawn(monitor_runtime_agent_provider_dispatch(
                    handle.clone(),
                    agent_id,
                    turn_id,
                    dispatch,
                ));
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
                workers.spawn(monitor_runtime_agent_compaction_dispatch(
                    handle.clone(),
                    pane_id,
                    dispatch,
                ));
            }
            RuntimeSideEffect::DispatchAgentRemember { pane_id } => {
                let dispatch = match handle.claim_agent_remember_task(pane_id.clone()).await {
                    Ok(Some(dispatch)) => dispatch,
                    Ok(None) => continue,
                    Err(error) => {
                        let mut batch = RuntimeEventBatch::new();
                        batch.push(RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                            pane_id,
                            kind: provider_worker_error_kind(&error).to_string(),
                            message: error.message().to_string(),
                            provider_failure_json: error
                                .provider_failure_json()
                                .map(str::to_string),
                            provider_raw_text: error.provider_raw_text().map(str::to_string),
                        }));
                        handle.submit_runtime_events(batch).await?;
                        continue;
                    }
                };
                workers.spawn(monitor_runtime_agent_remember_dispatch(
                    handle.clone(),
                    pane_id,
                    dispatch,
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Runs one provider request while honoring turn cancellation.
///
/// A provider task is claimed before it begins request serialization and
/// network work. Once claimed, `/stop` removes the turn from runtime state,
/// so the async side must drop the provider future instead of waiting for it
/// to finish and continue allocating memory for a cancelled turn.
async fn monitor_runtime_agent_provider_dispatch(
    handle: AsyncRuntimeSessionHandle,
    agent_id: AgentId,
    turn_id: String,
    dispatch: RuntimeAgentProviderDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    if !handle.agent_turn_is_running(&turn_id).await? {
        return Ok(None);
    }
    let worker = execute_runtime_agent_provider_dispatch(dispatch, None);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(provider_worker_event(agent_id, turn_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        let lifecycle_state = *lifecycle.borrow();
        if is_terminal_runtime_lifecycle_state(lifecycle_state)
            || !handle.agent_turn_is_running(&turn_id).await?
        {
            return Ok(None);
        }
    }
}

/// Runs one model-backed compaction worker while honoring shutdown.
async fn monitor_runtime_agent_compaction_dispatch(
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    dispatch: RuntimeAgentCompactionDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let worker = execute_runtime_agent_compaction_dispatch(dispatch);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(compaction_worker_event(pane_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        if is_terminal_runtime_lifecycle_state(*lifecycle.borrow()) {
            return Ok(None);
        }
    }
}

/// Runs one model-backed durable memory worker while honoring shutdown.
async fn monitor_runtime_agent_remember_dispatch(
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    dispatch: RuntimeAgentRememberDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let worker = execute_runtime_agent_remember_dispatch(dispatch);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(remember_worker_event(pane_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        if is_terminal_runtime_lifecycle_state(*lifecycle.borrow()) {
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

/// Converts a durable memory worker result into a runtime event.
fn remember_worker_event(
    pane_id: String,
    result: std::result::Result<Result<crate::agent::ModelResponse>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(response)) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Completed {
                pane_id,
                response: Box::new(response),
            }),
            true,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                pane_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
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
    _output_progress_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentProviderEvent>>,
) -> Result<super::AgentTurnExecution> {
    let RuntimeAgentProviderDispatch {
        turn,
        context,
        mut model_profile,
        macro_judge_request,
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
        memory_actions_enabled,
        issue_actions_enabled,
        loop_turn: _,
    } = dispatch;
    let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
    let mut selected_auto_sizing_profile: Option<ModelProfile> = None;
    let loop_allowed_actions = None;
    if let Some(request) = macro_judge_request {
        let response = match provider {
            RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::ClaudeCode(provider) => {
                provider.send_request_async(&request).await?
            }
        };
        return Ok(super::AgentTurnExecution {
            request,
            response,
            latest_response_usage: Default::default(),
            routing_token_usage_by_model,
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        });
    }
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
                .await?;
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
                .await?;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                selected_auto_sizing_profile = Some(auto_sizing_execution.selected_profile);
            }
            RuntimeAgentProviderDispatchProvider::Anthropic(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await?;
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
                .await?;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                selected_auto_sizing_profile = Some(auto_sizing_execution.selected_profile);
            }
            RuntimeAgentProviderDispatchProvider::ClaudeCode(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await?;
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
                .await?;
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
                .await?;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                Some(auto_sizing_execution.selected_profile)
            }
            RuntimeAgentProviderDispatchProvider::Anthropic(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await?;
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
                .await?;
                merge_model_token_usage_by_model(
                    &mut routing_token_usage_by_model,
                    auto_sizing_execution.token_usage_by_model(),
                );
                Some(auto_sizing_execution.selected_profile)
            }
            RuntimeAgentProviderDispatchProvider::ClaudeCode(router_provider) => {
                let auto_sizing_execution = runtime_execute_auto_sizing_with_async_provider(
                    router_provider,
                    auto_sizing,
                    &turn,
                    &context,
                )
                .await?;
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
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                ),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
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
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                ),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
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
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                ),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
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
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                ),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
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
        RuntimeAgentProviderDispatchProvider::ClaudeCode(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                ),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
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
        mez_agent::turn_state_from_action_results(&execution.action_results, execution.final_turn);
    Ok(execution)
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
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
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
        RuntimeAgentProviderDispatchProvider::ClaudeCode(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
    }
}

/// Executes one model-backed durable memory generation request.
async fn execute_runtime_agent_remember_dispatch(
    dispatch: RuntimeAgentRememberDispatch,
) -> Result<crate::agent::ModelResponse> {
    let RuntimeAgentRememberDispatch { task, provider } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::ClaudeCode(provider) => {
            provider.send_request_async(&task.request).await
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
