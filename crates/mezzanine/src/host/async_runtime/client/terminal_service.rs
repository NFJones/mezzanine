//! Attached-terminal client service construction, wakeups, and render rate limiting.

use super::{
    AsyncAttachedTerminalIo, AsyncAttachedTerminalLoopRequest, AsyncRuntimeSessionHandle,
    AsyncTerminalIoFuture, AsyncTerminalOutputWriteReport, AttachedTerminalClientLoopReport,
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, ClientStatusLine,
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, MezError, MouseAction,
    RenderInvalidationReason, Result, RuntimeLifecycleState, RuntimeSideEffect, RuntimeTimerKey,
    RuntimeTimerKind, TerminalClientLoopAction, TerminalFdInterest, TerminalStyleSpan,
    empty_attached_terminal_loop_report, is_terminal_runtime_lifecycle_state,
    merge_attached_terminal_loop_report, run_async_attached_terminal_client_loop, sleep, watch,
};
#[cfg(test)]
use super::{AsyncRuntimeService, AsyncRuntimeServiceExit};
use std::time::Duration;
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
#[cfg(test)]
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
