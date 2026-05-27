//! Async Runtime Terminal implementation.
//!
//! This module owns the async runtime terminal boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsyncAttachedTerminalIo, AsyncRenderedClientFrame, AsyncRuntimeSessionHandle,
    AsyncRuntimeSideEffectServiceConfig, AttachedTerminalClientLoopConfig,
    AttachedTerminalClientLoopReport, AttachedTerminalFdRole, AttachedTerminalOutputModes,
    ClientId, ClientStatusLine, ClientViewRole, MezError, Result, RuntimeSideEffect, Size,
    TerminalClientLoopConfig, plan_attached_terminal_client_step_with_host_paste_buffer,
    run_async_client_output_flush_service,
};
use crate::terminal::{
    TerminalClientLoopAction, compose_client_presentation_with_styles,
    compose_terminal_output_style_spans,
};

// Attached terminal loop handling.

/// Carries Async Attached Terminal Loop Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncAttachedTerminalLoopRequest {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ClientViewRole,
    /// Stores the client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_id: ClientId,
    /// Stores the primary client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_client_id: Option<ClientId>,
    /// Stores the client size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_size: Size,
    /// Stores the terminal config value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_config: TerminalClientLoopConfig,
    /// Stores the loop config value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub loop_config: AttachedTerminalClientLoopConfig,
}

/// Pane I/O application mode for attached terminal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncAttachedTerminalPaneIoMode {
    /// Apply pane input directly through the synchronous process manager.
    Inline,
    /// Queue pane input as side effects for async pane process workers.
    Deferred,
}

/// Carries Async Attached Terminal Error Recovery state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct AsyncAttachedTerminalErrorRecovery {
    /// Stores the client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    client_id: ClientId,
    /// Stores the error value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    error: MezError,
    /// Stores the client size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    client_size: Size,
    /// Stores the terminal config value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    terminal_config: TerminalClientLoopConfig,
    /// Stores the cursor blink epoch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_blink_epoch: std::time::Instant,
    /// Stores the output writable value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output_writable: bool,
}

/// Runs the recover attached terminal error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn recover_attached_terminal_error<I>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    recovery: AsyncAttachedTerminalErrorRecovery,
    report: &mut AttachedTerminalClientLoopReport,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
{
    let display_lines = vec![format!("mez error: {}", recovery.error)];
    handle.show_primary_error_overlay(display_lines).await?;
    if !recovery.output_writable {
        return Ok(());
    }
    let refreshed = handle
        .render_client_frame(
            ClientViewRole::Primary,
            recovery.client_size,
            recovery.terminal_config,
            true,
        )
        .await?;
    let Some(view) = refreshed.view.as_ref() else {
        return Ok(());
    };
    let (lines, spans) = compose_client_presentation_with_styles(view, None);
    let output_modes = AttachedTerminalOutputModes {
        application_keypad: refreshed.config.mouse_policy.pane_application_keypad_mode,
        bracketed_paste: refreshed.config.pane_bracketed_paste_mode,
        cursor_style: refreshed.config.cursor_style,
        cursor_blink: false,
        cursor_blink_interval_ms: refreshed.config.cursor_blink_interval_ms,
        cursor_blink_elapsed_ms: cursor_blink_elapsed_ms(recovery.cursor_blink_epoch),
        animation_refresh_interval_ms: view.animation_refresh_interval_ms,
        cursor_visible: view.cursor_visible,
        cursor_row: view.cursor_row,
        cursor_column: view.cursor_column,
    };
    let flush = queue_and_flush_async_attached_terminal_output(
        handle,
        io,
        recovery.client_id,
        lines,
        spans,
        output_modes,
    )
    .await?;
    merge_attached_terminal_flush_report(report, &flush);
    Ok(())
}

/// Runs the cursor blink elapsed ms operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_blink_elapsed_ms(epoch: std::time::Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Runs the queue and flush async attached terminal output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn queue_and_flush_async_attached_terminal_output<I>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    client_id: ClientId,
    lines: Vec<String>,
    line_style_spans: Vec<Vec<crate::terminal::TerminalStyleSpan>>,
    modes: AttachedTerminalOutputModes,
) -> Result<super::AsyncClientOutputFlushServiceReport>
where
    I: AsyncAttachedTerminalIo,
{
    let timer_client_id = client_id.clone();
    handle
        .queue_runtime_side_effects(vec![RuntimeSideEffect::FlushClientOutput {
            client_id: client_id.clone(),
            lines,
            line_style_spans,
            modes,
        }])
        .await?;
    let report = run_async_client_output_flush_service(
        handle,
        client_id,
        io,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 1,
            drain_limit: 8,
            idle_interval: std::time::Duration::from_millis(1),
        },
        |_, _| false,
    )
    .await?;
    if report.flushed > 0 {
        handle.ensure_client_render_timers(timer_client_id).await?;
    }
    Ok(report)
}

/// Merges an output-flush worker report into the attached-terminal loop report.
fn merge_attached_terminal_flush_report(
    loop_report: &mut AttachedTerminalClientLoopReport,
    flush: &super::AsyncClientOutputFlushServiceReport,
) {
    loop_report.bytes_written = loop_report
        .bytes_written
        .saturating_add(flush.bytes_written);
    loop_report.output_frames = loop_report.output_frames.saturating_add(flush.flushed);
    loop_report.output_hangups = loop_report
        .output_hangups
        .saturating_add(flush.output_hangups);
    loop_report.partial_writes = loop_report
        .partial_writes
        .saturating_add(flush.partial_writes);
    loop_report.pending_output_bytes = flush.pending_output_bytes;
}

/// Runs the run async attached terminal client loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run_async_attached_terminal_client_loop<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    request: AsyncAttachedTerminalLoopRequest,
    status_provider: S,
) -> Result<AttachedTerminalClientLoopReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    run_async_attached_terminal_client_loop_with_pane_io_mode(
        handle,
        io,
        request,
        AsyncAttachedTerminalPaneIoMode::Inline,
        status_provider,
    )
    .await
}

/// Runs an attached terminal client loop that queues pane input for async pane
/// process workers instead of writing through the compatibility manager path.
pub async fn run_async_attached_terminal_client_loop_deferred_pane_io<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    request: AsyncAttachedTerminalLoopRequest,
    status_provider: S,
) -> Result<AttachedTerminalClientLoopReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    run_async_attached_terminal_client_loop_with_pane_io_mode(
        handle,
        io,
        request,
        AsyncAttachedTerminalPaneIoMode::Deferred,
        status_provider,
    )
    .await
}

/// Runs the run async attached terminal client loop with pane io mode operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn run_async_attached_terminal_client_loop_with_pane_io_mode<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    io: &mut I,
    request: AsyncAttachedTerminalLoopRequest,
    pane_io_mode: AsyncAttachedTerminalPaneIoMode,
    mut status_provider: S,
) -> Result<AttachedTerminalClientLoopReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64) -> Result<Option<ClientStatusLine>>,
{
    if request.loop_config.max_iterations == 0 {
        return Err(MezError::invalid_args(
            "attached terminal client loop max_iterations must be greater than zero",
        ));
    }
    if request.loop_config.max_input_bytes == 0 {
        return Err(MezError::invalid_args(
            "attached terminal client loop max_input_bytes must be greater than zero",
        ));
    }
    if request.role == ClientViewRole::Primary && request.primary_client_id.is_none() {
        return Err(MezError::invalid_args(
            "primary attached terminal loop requires a primary client id",
        ));
    }

    let mut report = AttachedTerminalClientLoopReport {
        iterations: 0,
        actions: Vec::new(),
        output_frames: 0,
        bytes_written: 0,
        partial_writes: 0,
        pending_output_bytes: 0,
        input_hangups: 0,
        output_hangups: 0,
        error_roles: Vec::new(),
        host_bracketed_paste_active: request.terminal_config.host_bracketed_paste_active,
        host_bracketed_paste_buffer: request.terminal_config.host_bracketed_paste_buffer.clone(),
    };
    let cursor_blink_epoch = std::time::Instant::now();
    let mut host_bracketed_paste_active = request.terminal_config.host_bracketed_paste_active;
    let mut host_bracketed_paste_buffer =
        request.terminal_config.host_bracketed_paste_buffer.clone();

    for _ in 0..request.loop_config.max_iterations {
        let readiness = io.poll_readiness().await?;
        let input_readable = readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable);
        let input = if input_readable {
            let bytes = io.read_input(request.loop_config.max_input_bytes).await?;
            if request.role == ClientViewRole::Primary {
                Some(bytes)
            } else {
                None
            }
        } else {
            None
        };
        let output_writable = readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Output && ready.writable);
        let frame = if input_readable || output_writable {
            handle
                .render_client_frame(
                    request.role,
                    request.client_size,
                    request.terminal_config.clone(),
                    output_writable,
                )
                .await?
        } else {
            AsyncRenderedClientFrame {
                config: request.terminal_config.clone(),
                view: None,
            }
        };
        let status = if output_writable {
            status_provider(report.iterations)?
        } else {
            None
        };
        let step = plan_attached_terminal_client_step_with_host_paste_buffer(
            &readiness,
            input.as_deref(),
            frame.view.as_ref(),
            status.as_ref(),
            &frame.config,
            &mut host_bracketed_paste_active,
            &mut host_bracketed_paste_buffer,
        )?;
        report.host_bracketed_paste_active = host_bracketed_paste_active;
        report.host_bracketed_paste_buffer = host_bracketed_paste_buffer.clone();

        let agent_prompt_input_action = request.role == ClientViewRole::Primary
            && frame
                .view
                .as_ref()
                .and_then(|view| view.agent_prompt_region)
                .is_some()
            && step
                .actions
                .iter()
                .any(|action| matches!(action, TerminalClientLoopAction::ForwardToPane(_)));
        if !step.output_lines.is_empty() && !agent_prompt_input_action {
            let rendered = frame
                .view
                .as_ref()
                .map(|view| (view.clone(), status.clone()));
            let output_line_style_spans =
                compose_terminal_output_style_spans(&step.output_lines, rendered.as_ref());
            let output_modes = AttachedTerminalOutputModes {
                application_keypad: frame.config.mouse_policy.pane_application_keypad_mode,
                bracketed_paste: frame.config.pane_bracketed_paste_mode,
                cursor_style: frame.config.cursor_style,
                cursor_blink: frame.config.cursor_blink,
                cursor_blink_interval_ms: frame.config.cursor_blink_interval_ms,
                cursor_blink_elapsed_ms: cursor_blink_elapsed_ms(cursor_blink_epoch),
                animation_refresh_interval_ms: frame
                    .view
                    .as_ref()
                    .map(|view| view.animation_refresh_interval_ms)
                    .unwrap_or(0),
                cursor_visible: frame.view.as_ref().is_some_and(|view| view.cursor_visible),
                cursor_row: frame.view.as_ref().map(|view| view.cursor_row).unwrap_or(0),
                cursor_column: frame
                    .view
                    .as_ref()
                    .map(|view| view.cursor_column)
                    .unwrap_or(0),
            };
            let flush = queue_and_flush_async_attached_terminal_output(
                handle,
                io,
                request.client_id.clone(),
                step.output_lines.clone(),
                output_line_style_spans.clone(),
                output_modes,
            )
            .await?;
            merge_attached_terminal_flush_report(&mut report, &flush);
            if flush.output_hangups > 0 {
                break;
            }
        }
        if request.role == ClientViewRole::Primary
            && !step.actions.is_empty()
            && let Some(primary_client_id) = request.primary_client_id.as_ref()
        {
            let application_result = match pane_io_mode {
                AsyncAttachedTerminalPaneIoMode::Inline => {
                    handle
                        .apply_attached_terminal_step_plan_inline_pane_io(
                            primary_client_id.clone(),
                            step.clone(),
                        )
                        .await
                }
                AsyncAttachedTerminalPaneIoMode::Deferred => {
                    handle
                        .apply_attached_terminal_step_plan(primary_client_id.clone(), step.clone())
                        .await
                }
            };
            let application = match application_result {
                Ok(application) => application,
                Err(error) => {
                    recover_attached_terminal_error(
                        handle,
                        io,
                        AsyncAttachedTerminalErrorRecovery {
                            client_id: request.client_id.clone(),
                            error,
                            client_size: request.client_size,
                            terminal_config: frame.config.clone(),
                            cursor_blink_epoch,
                            output_writable,
                        },
                        &mut report,
                    )
                    .await?;
                    return Ok(report);
                }
            };
            if application.full_redraw_required {
                io.invalidate_output_frame().await?;
            }
            if application.view_refresh_required && output_writable {
                let refreshed = handle
                    .render_client_frame(
                        request.role,
                        request.client_size,
                        frame.config.clone(),
                        true,
                    )
                    .await?;
                if let Some(view) = refreshed.view.as_ref() {
                    let (lines, spans) =
                        compose_client_presentation_with_styles(view, status.as_ref());
                    let output_modes = AttachedTerminalOutputModes {
                        application_keypad: refreshed
                            .config
                            .mouse_policy
                            .pane_application_keypad_mode,
                        bracketed_paste: refreshed.config.pane_bracketed_paste_mode,
                        cursor_style: refreshed.config.cursor_style,
                        cursor_blink: refreshed.config.cursor_blink,
                        cursor_blink_interval_ms: refreshed.config.cursor_blink_interval_ms,
                        cursor_blink_elapsed_ms: cursor_blink_elapsed_ms(cursor_blink_epoch),
                        animation_refresh_interval_ms: view.animation_refresh_interval_ms,
                        cursor_visible: view.cursor_visible,
                        cursor_row: view.cursor_row,
                        cursor_column: view.cursor_column,
                    };
                    let flush = queue_and_flush_async_attached_terminal_output(
                        handle,
                        io,
                        request.client_id.clone(),
                        lines,
                        spans,
                        output_modes,
                    )
                    .await?;
                    merge_attached_terminal_flush_report(&mut report, &flush);
                    if flush.output_hangups > 0 {
                        break;
                    }
                }
            }
        }
        report.actions.extend(step.actions);
        if step.input_hangup {
            report.input_hangups = report.input_hangups.saturating_add(1);
        }
        if step.output_hangup {
            report.output_hangups = report.output_hangups.saturating_add(1);
        }
        report.error_roles.extend(step.error_roles);
        report.iterations = report.iterations.saturating_add(1);

        if report.input_hangups > 0 || report.output_hangups > 0 || !report.error_roles.is_empty() {
            break;
        }
    }

    report.pending_output_bytes = io.pending_output_bytes();
    Ok(report)
}
