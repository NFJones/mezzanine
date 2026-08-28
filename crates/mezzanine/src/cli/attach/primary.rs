//! Primary control-socket attach setup and interactive loop.

use super::event_stream::read_attached_client_input_or_deadline;
use super::event_stream::{
    AttachRenderAction, AttachedClientInputPoll, AttachedRuntimeEventStream,
    IrohAttachRenderWakeup, coalesce_ready_iroh_render_actions,
    control_socket_disconnected_without_pending_response, optional_control_socket_event_stream,
    read_attached_client_input_or_iroh_event, read_attached_client_input_or_runtime_event,
};
use super::requests::{
    read_async_control_response_frames, read_async_control_response_frames_or_disconnected,
    refresh_attached_client_size_async, render_iroh_attach_client_frame_async,
    render_iroh_attach_client_frame_bounded_async, request_and_render_primary_view_async,
    request_primary_resize_async, request_primary_view_frame_async, terminal_step_control_request,
    terminal_step_if_changed_control_request, write_async_control_body_or_disconnected,
};
use super::responses::{
    control_response_forbidden, terminal_step_response_client_frame,
    terminal_step_response_refresh_requirement,
};
use super::{
    AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachAnimationRefresh, AttachTerminalSizeRefresh, ClientId, MezError, Result, Size,
    UnixStream, decode_control_frame, io,
};

/// Runs the run control socket attached primary client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::cli) async fn run_control_socket_attached_primary_client(
    stream: &mut UnixStream,
    control_socket_path: &std::path::Path,
    primary_client_id: ClientId,
    client_size: Size,
    event_binding_token: String,
) -> Result<()> {
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let control_stream = stream.try_clone()?;
    control_stream.set_nonblocking(true)?;
    let mut control_stream = tokio::net::UnixStream::from_std(control_stream)?;
    let event_stream =
        optional_control_socket_event_stream(control_socket_path, event_binding_token.as_str())?;
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
        &mut control_stream,
        terminal_guard.io_mut(),
        primary_client_id,
        client_size,
        event_stream,
    )
    .await;
    let restore_result = terminal_guard.restore().await;
    match run_result {
        Ok(()) => restore_result,
        Err(error) => {
            let _ = restore_result;
            Err(error)
        }
    }
}

/// Runs a primary attach over one persistent Iroh control stream.
pub(in crate::cli) async fn run_iroh_attached_primary_client<S>(
    stream: &mut S,
    connection: &iroh::endpoint::Connection,
    primary_client_id: ClientId,
    client_size: Size,
    request_timeout: std::time::Duration,
    mut event_receiver: tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>,
    pushed_render_owner: bool,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_iroh_attached_primary_client_loop_async_with_events(
        stream,
        terminal_guard.io_mut(),
        Some(connection),
        primary_client_id,
        client_size,
        request_timeout,
        Some(&mut event_receiver),
        pushed_render_owner,
    )
    .await;
    let restore_result = terminal_guard.restore().await;
    match run_result {
        Ok(()) => restore_result,
        Err(error) => {
            let _ = restore_result;
            Err(error)
        }
    }
}

/// Runs remote primary attach without replaying ambiguous input.
#[cfg(test)]
pub(in crate::cli) async fn run_iroh_attached_primary_client_loop_async<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    primary_client_id: ClientId,
    client_size: Size,
    request_timeout: std::time::Duration,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    run_iroh_attached_primary_client_loop_async_with_events(
        stream,
        terminal_io,
        None,
        primary_client_id,
        client_size,
        request_timeout,
        None,
        false,
    )
    .await
}

/// Tracks animation cadence for terminal output retained across bounded passes.
#[derive(Debug, Default)]
struct PendingIrohOutput {
    /// Cadence belonging to the newest frame queued in the terminal adapter.
    animation_refresh_interval_ms: Option<u64>,
}

/// Queues one frame and performs at most one bounded terminal-output pass.
async fn queue_iroh_frame_bounded<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    frame: &super::AttachClientFrame,
    connected: bool,
    quality: crate::host::terminal::TerminalIrohStatusQuality,
    cursor_blink_epoch: std::time::Instant,
    pending_output: &mut PendingIrohOutput,
    animation_refresh: &mut AttachAnimationRefresh,
) -> Result<bool> {
    let (outcome, report) = render_iroh_attach_client_frame_bounded_async(
        terminal_io,
        frame,
        connected,
        quality,
        cursor_blink_epoch,
    )
    .await?;
    if !outcome.connected {
        return Ok(false);
    }
    pending_output.animation_refresh_interval_ms = Some(outcome.animation_refresh_interval_ms);
    if !report.is_partial()
        && terminal_io.pending_output_bytes() == 0
        && let Some(interval_ms) = pending_output.animation_refresh_interval_ms.take()
    {
        animation_refresh.update_from_rendered_view(interval_ms);
    }
    Ok(true)
}

/// Advances retained terminal output by at most one bounded flush pass.
async fn flush_pending_iroh_output<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    pending_output: &mut PendingIrohOutput,
    animation_refresh: &mut AttachAnimationRefresh,
) -> Result<bool> {
    if terminal_io.pending_output_bytes() == 0 {
        return Ok(true);
    }
    let report = match terminal_io
        .flush_pending_output(
            crate::host::async_runtime::DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
        )
        .await
    {
        Ok(report) => report,
        Err(error) if super::attached_terminal_output_disconnected(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    if !report.is_partial()
        && terminal_io.pending_output_bytes() == 0
        && let Some(interval_ms) = pending_output.animation_refresh_interval_ms.take()
    {
        animation_refresh.update_from_rendered_view(interval_ms);
    }
    Ok(true)
}

/// Presents one authoritative v3 wakeup in a bounded output pass.
///
/// The response wait regains control after each pass, so pushed rendering can
/// start before the acknowledgement without starving it or follow-on input.
#[allow(
    clippy::too_many_arguments,
    reason = "terminal presentation, queued wakeups, retained frame state, output scheduling, animation cadence, path quality, and cursor timing are independent attach-loop state"
)]
async fn present_iroh_wakeup_while_step_pending<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    event_receiver: &mut tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>,
    wakeup: IrohAttachRenderWakeup,
    cached_frame: &mut Option<super::AttachClientFrame>,
    pending_output: &mut PendingIrohOutput,
    animation_refresh: &mut AttachAnimationRefresh,
    quality: crate::host::terminal::TerminalIrohStatusQuality,
    cursor_blink_epoch: std::time::Instant,
) -> Result<()> {
    let wakeup = coalesce_ready_iroh_render_actions(
        event_receiver,
        wakeup,
        cached_frame.as_ref().and_then(|frame| frame.event_cutoff),
    )?;
    let received_pushed_snapshot = wakeup.pushed_snapshot.is_some();
    if let Some(snapshot) = wakeup.pushed_snapshot {
        if snapshot.invalidate_output {
            terminal_io.invalidate_output_frame().await?;
        }
        let connected = queue_iroh_frame_bounded(
            terminal_io,
            &snapshot.frame,
            true,
            quality,
            cursor_blink_epoch,
            pending_output,
            animation_refresh,
        )
        .await?;
        if !connected {
            return Err(MezError::invalid_state(
                "Iroh attach disconnected while rendering a pushed snapshot",
            ));
        }
        *cached_frame = Some(snapshot.frame);
    }
    match wakeup.action {
        AttachRenderAction::None | AttachRenderAction::View if received_pushed_snapshot => Ok(()),
        AttachRenderAction::View => {
            if let Some(frame) = cached_frame.as_ref() {
                let connected = queue_iroh_frame_bounded(
                    terminal_io,
                    frame,
                    true,
                    quality,
                    cursor_blink_epoch,
                    pending_output,
                    animation_refresh,
                )
                .await?;
                if !connected {
                    return Err(MezError::invalid_state(
                        "Iroh attach terminal disconnected during local repaint",
                    ));
                }
            }
            Ok(())
        }
        AttachRenderAction::InvalidateAndView => terminal_io.invalidate_output_frame().await,
        AttachRenderAction::Disconnect => {
            if let Some(frame) = cached_frame.as_ref() {
                let _ = queue_iroh_frame_bounded(
                    terminal_io,
                    frame,
                    false,
                    quality,
                    cursor_blink_epoch,
                    pending_output,
                    animation_refresh,
                )
                .await;
            }
            Err(MezError::invalid_state(
                "Iroh event stream disconnected; reattach required",
            ))
        }
        AttachRenderAction::None => Ok(()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "control stream, terminal I/O, connection health, client identity, geometry, timeout, event delivery, and render ownership are independent attach inputs"
)]
async fn run_iroh_attached_primary_client_loop_async_with_events<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    connection: Option<&iroh::endpoint::Connection>,
    primary_client_id: ClientId,
    mut client_size: Size,
    request_timeout: std::time::Duration,
    mut event_receiver: Option<&mut tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>>,
    pushed_render_owner: bool,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut render_requested = !pushed_render_owner;
    let mut size_refresh = AttachTerminalSizeRefresh::default();
    let mut animation_refresh = AttachAnimationRefresh::default();
    let mut health = super::AttachIrohHealthTracker::default();
    let mut cached_frame: Option<super::AttachClientFrame> = None;
    let mut pending_output = PendingIrohOutput::default();
    let mut buffered_input = Vec::new();
    let mut buffered_eof = false;
    loop {
        if buffered_eof {
            return Ok(());
        }
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
            let outcome = tokio::time::timeout(
                request_timeout,
                request_primary_resize_async(stream, &primary_client_id, client_size, iteration),
            )
            .await
            .map_err(|_| {
                MezError::invalid_state(
                    "Iroh terminal resize acknowledgement timed out; reattach required",
                )
            })??;
            if !outcome.connected {
                return Err(MezError::invalid_state(
                    "Iroh attach disconnected during terminal resize; reattach required",
                ));
            }
            iteration = iteration.saturating_add(1);
            render_requested = !pushed_render_owner;
        }
        let wake_deadline = connection
            .map(|_| health.deadline().min(size_refresh.deadline()))
            .unwrap_or_else(|| size_refresh.deadline());
        let wake_deadline = if pushed_render_owner && terminal_io.pending_output_bytes() > 0 {
            tokio::time::Instant::now() + std::time::Duration::from_millis(1)
        } else {
            wake_deadline
        };
        let input = if !buffered_input.is_empty() {
            AttachedClientInputPoll {
                bytes: std::mem::take(&mut buffered_input),
                eof: false,
                render_action: AttachRenderAction::None,
                pushed_snapshot: None,
            }
        } else {
            match event_receiver.as_deref_mut() {
                Some(event_receiver) => {
                    read_attached_client_input_or_iroh_event(
                        terminal_io,
                        event_receiver,
                        4096,
                        animation_refresh.deadline(),
                        wake_deadline,
                        cached_frame.as_ref().and_then(|frame| frame.event_cutoff),
                    )
                    .await?
                }
                None => {
                    read_attached_client_input_or_deadline(
                        terminal_io,
                        4096,
                        animation_refresh.deadline(),
                        animation_refresh
                            .deadline()
                            .filter(|deadline| *deadline <= wake_deadline)
                            .unwrap_or(wake_deadline),
                    )
                    .await?
                }
            }
        };
        size_refresh.reschedule();
        let received_pushed_snapshot = input.pushed_snapshot.is_some();
        if let Some(snapshot) = input.pushed_snapshot {
            if snapshot.invalidate_output {
                terminal_io.invalidate_output_frame().await?;
            }
            if pushed_render_owner {
                if !queue_iroh_frame_bounded(
                    terminal_io,
                    &snapshot.frame,
                    true,
                    health.quality(),
                    cursor_blink_epoch,
                    &mut pending_output,
                    &mut animation_refresh,
                )
                .await?
                {
                    return Err(MezError::invalid_state(
                        "Iroh attach disconnected while rendering a pushed snapshot",
                    ));
                }
            } else {
                let outcome = render_iroh_attach_client_frame_async(
                    terminal_io,
                    &snapshot.frame,
                    true,
                    health.quality(),
                    cursor_blink_epoch,
                )
                .await?;
                if !outcome.connected {
                    return Err(MezError::invalid_state(
                        "Iroh attach disconnected while rendering a pushed snapshot",
                    ));
                }
                animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
            }
            cached_frame = Some(snapshot.frame);
            render_requested = false;
        }
        match input.render_action {
            AttachRenderAction::None => {}
            AttachRenderAction::View if pushed_render_owner && !received_pushed_snapshot => {
                if let Some(frame) = cached_frame.as_ref() {
                    let connected = queue_iroh_frame_bounded(
                        terminal_io,
                        frame,
                        true,
                        health.quality(),
                        cursor_blink_epoch,
                        &mut pending_output,
                        &mut animation_refresh,
                    )
                    .await?;
                    if !connected {
                        return Err(MezError::invalid_state(
                            "Iroh attach terminal disconnected during local animation repaint",
                        ));
                    }
                }
            }
            AttachRenderAction::View => render_requested = !pushed_render_owner,
            AttachRenderAction::InvalidateAndView => {
                terminal_io.invalidate_output_frame().await?;
                render_requested = !pushed_render_owner;
            }
            AttachRenderAction::Disconnect => {
                if let Some(frame) = cached_frame.as_ref() {
                    if pushed_render_owner {
                        let _ = queue_iroh_frame_bounded(
                            terminal_io,
                            frame,
                            false,
                            health.quality(),
                            cursor_blink_epoch,
                            &mut pending_output,
                            &mut animation_refresh,
                        )
                        .await;
                    } else {
                        let _ = render_iroh_attach_client_frame_async(
                            terminal_io,
                            frame,
                            false,
                            health.quality(),
                            cursor_blink_epoch,
                        )
                        .await;
                    }
                }
                return Err(MezError::invalid_state(
                    "Iroh event stream disconnected; reattach required",
                ));
            }
        }
        if input.eof {
            return Ok(());
        }
        if input.bytes.is_empty() {
            if pushed_render_owner
                && terminal_io.pending_output_bytes() > 0
                && !flush_pending_iroh_output(
                    terminal_io,
                    &mut pending_output,
                    &mut animation_refresh,
                )
                .await?
            {
                return Err(MezError::invalid_state(
                    "Iroh attach terminal disconnected while flushing pushed output",
                ));
            }
            let quality_changed = connection.is_some_and(|connection| {
                health.deadline() <= tokio::time::Instant::now() && health.sample(connection)
            });
            if !render_requested && quality_changed {
                if let Some(frame) = cached_frame.as_ref() {
                    if pushed_render_owner {
                        if !queue_iroh_frame_bounded(
                            terminal_io,
                            frame,
                            true,
                            health.quality(),
                            cursor_blink_epoch,
                            &mut pending_output,
                            &mut animation_refresh,
                        )
                        .await?
                        {
                            return Err(MezError::invalid_state(
                                "Iroh attach terminal disconnected during local status repaint",
                            ));
                        }
                    } else {
                        let outcome = render_iroh_attach_client_frame_async(
                            terminal_io,
                            frame,
                            true,
                            health.quality(),
                            cursor_blink_epoch,
                        )
                        .await?;
                        if !outcome.connected {
                            return Err(MezError::invalid_state(
                                "Iroh attach terminal disconnected during local status repaint",
                            ));
                        }
                    }
                }
                continue;
            }
            if !render_requested {
                continue;
            }
            let frame = tokio::time::timeout(
                request_timeout,
                request_primary_view_frame_async(stream, client_size, iteration),
            )
            .await
            .map_err(|_| {
                MezError::invalid_state(
                    "Iroh terminal view acknowledgement timed out; reattach required",
                )
            })??
            .ok_or_else(|| {
                MezError::invalid_state(
                    "Iroh attach disconnected while reading a terminal view; reattach required",
                )
            })?;
            if let Some(connection) = connection {
                health.sample(connection);
            }
            let outcome = render_iroh_attach_client_frame_async(
                terminal_io,
                &frame,
                true,
                health.quality(),
                cursor_blink_epoch,
            )
            .await?;
            if !outcome.connected {
                return Err(MezError::invalid_state(
                    "Iroh attach disconnected while reading a terminal view; reattach required",
                ));
            }
            cached_frame = Some(frame);
            animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
            render_requested = false;
            iteration = iteration.saturating_add(1);
            continue;
        }

        let request = if pushed_render_owner {
            terminal_step_control_request(
                iteration,
                &primary_client_id,
                client_size,
                input.bytes.as_slice(),
                false,
            )
        } else {
            terminal_step_if_changed_control_request(
                iteration,
                &primary_client_id,
                client_size,
                input.bytes.as_slice(),
            )
        };
        let write_result = tokio::time::timeout(request_timeout, async {
            tokio::io::AsyncWriteExt::write_all(stream, &super::encode_control_body(&request))
                .await?;
            tokio::io::AsyncWriteExt::flush(stream).await
        })
        .await;
        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => {
                return Err(MezError::invalid_state(
                    "Iroh terminal input outcome is unknown; reattach required; input was not replayed",
                ));
            }
        }
        let response = {
            let response_deadline = tokio::time::Instant::now() + request_timeout;
            let response = tokio::time::timeout(
                request_timeout,
                read_async_control_response_frames(stream, 1024 * 1024, 1),
            );
            tokio::pin!(response);
            if pushed_render_owner {
                if let Some(event_receiver) = event_receiver.as_deref_mut() {
                    loop {
                        if terminal_io.pending_output_bytes() > 0
                            && !flush_pending_iroh_output(
                                terminal_io,
                                &mut pending_output,
                                &mut animation_refresh,
                            )
                            .await?
                        {
                            return Err(MezError::invalid_state(
                                "Iroh attach terminal disconnected while flushing pushed output",
                            ));
                        }
                        let event = {
                            let input = read_attached_client_input_or_deadline(
                                terminal_io,
                                4096usize.saturating_sub(buffered_input.len()),
                                None,
                                response_deadline,
                            );
                            tokio::pin!(input);
                            tokio::select! {
                                biased;
                                response = response.as_mut() => break response,
                                input = &mut input, if !buffered_eof && buffered_input.len() < 4096 => {
                                    let input = input?;
                                    if input.eof {
                                        buffered_input.clear();
                                        buffered_eof = true;
                                    } else {
                                        buffered_input.extend_from_slice(&input.bytes);
                                    }
                                    None
                                }
                                event = event_receiver.recv() => Some(event),
                            }
                        };
                        if let Some(event) = event {
                            match event {
                                Some(Ok(wakeup)) => {
                                    present_iroh_wakeup_while_step_pending(
                                        terminal_io,
                                        event_receiver,
                                        wakeup,
                                        &mut cached_frame,
                                        &mut pending_output,
                                        &mut animation_refresh,
                                        health.quality(),
                                        cursor_blink_epoch,
                                    )
                                    .await?;
                                }
                                Some(Err(error)) => return Err(error),
                                None => {
                                    return Err(MezError::invalid_state(
                                        "Iroh event stream disconnected; reattach required",
                                    ));
                                }
                            }
                        }
                    }
                } else {
                    response.as_mut().await
                }
            } else {
                response.as_mut().await
            }
        };
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => {
                return Err(MezError::invalid_state(
                    "Iroh terminal input outcome is unknown; reattach required; input was not replayed",
                ));
            }
        };
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        if control_response_forbidden(body.as_str())? {
            return Err(MezError::forbidden("Iroh terminal input was rejected"));
        }
        let refresh_requirement = terminal_step_response_refresh_requirement(body.as_str())?;
        let inline_frame = if pushed_render_owner {
            None
        } else {
            terminal_step_response_client_frame(body.as_str())?
        };
        if refresh_requirement.client_detached || refresh_requirement.session_terminated {
            return Ok(());
        }
        if refresh_requirement.full_redraw_required && !pushed_render_owner {
            terminal_io.invalidate_output_frame().await?;
        }
        if !pushed_render_owner
            && (inline_frame.is_some()
                || render_requested
                || refresh_requirement.view_refresh_required)
        {
            let frame = match inline_frame {
                Some(frame) => frame,
                None => tokio::time::timeout(
                    request_timeout,
                    request_primary_view_frame_async(stream, client_size, iteration),
                )
                .await
                .map_err(|_| {
                    MezError::invalid_state(
                        "Iroh terminal view acknowledgement timed out; reattach required",
                    )
                })??
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "Iroh attach disconnected while reading a terminal view; reattach required",
                    )
                })?,
            };
            if let Some(connection) = connection {
                health.sample(connection);
            }
            let outcome = render_iroh_attach_client_frame_async(
                terminal_io,
                &frame,
                true,
                health.quality(),
                cursor_blink_epoch,
            )
            .await?;
            if !outcome.connected {
                return Err(MezError::invalid_state(
                    "Iroh attach disconnected while reading a terminal view; reattach required",
                ));
            }
            cached_frame = Some(frame);
            animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
        }
        render_requested = false;
        iteration = iteration.saturating_add(1);
    }
}

#[cfg(test)]
#[allow(
    clippy::items_after_test_module,
    reason = "focused private v3 snapshot coverage stays beside its owning Iroh loop while public test-only control-socket adapters remain below"
)]
mod pushed_snapshot_tests {
    use super::*;

    /// Shared synchronization and observation state for slow terminal output.
    #[derive(Debug, Default)]
    struct SlowOutputState {
        input: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
        input_ready: tokio::sync::Notify,
        first_output_pass: tokio::sync::Notify,
        pending_output_bytes: std::sync::atomic::AtomicUsize,
    }

    /// Terminal fake that retains a pushed frame across many bounded passes.
    #[derive(Debug)]
    struct SlowBoundedTerminalIo {
        state: std::sync::Arc<SlowOutputState>,
    }

    impl SlowBoundedTerminalIo {
        /// Creates a slow-output terminal and a handle for deterministic gates.
        fn new() -> (Self, std::sync::Arc<SlowOutputState>) {
            let state = std::sync::Arc::new(SlowOutputState::default());
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl AsyncAttachedTerminalIo for SlowBoundedTerminalIo {
        fn poll_readiness<'a>(
            &'a mut self,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<
            'a,
            Vec<crate::host::terminal::AttachedTerminalFdReadiness>,
        > {
            self.poll_input_readiness()
        }

        fn poll_input_readiness<'a>(
            &'a mut self,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<
            'a,
            Vec<crate::host::terminal::AttachedTerminalFdReadiness>,
        > {
            Box::pin(async move {
                loop {
                    let notified = self.state.input_ready.notified();
                    if !self.state.input.lock().unwrap().is_empty() {
                        return Ok(Vec::new());
                    }
                    notified.await;
                }
            })
        }

        fn read_input<'a>(
            &'a mut self,
            max_bytes: usize,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, Vec<u8>> {
            Box::pin(async move {
                let mut input = self
                    .state
                    .input
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_default();
                input.truncate(max_bytes);
                Ok(input)
            })
        }

        fn write_styled_output_with_modes<'a>(
            &'a mut self,
            lines: &'a [String],
            _line_style_spans: &'a [Vec<super::super::TerminalStyleSpan>],
            _modes: super::super::AttachedTerminalOutputModes,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, usize> {
            Box::pin(async move { Ok(lines.iter().map(String::len).sum()) })
        }

        fn pending_output_bytes(&self) -> usize {
            self.state
                .pending_output_bytes
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn flush_pending_output<'a>(
            &'a mut self,
            _max_bytes: usize,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<
            'a,
            crate::host::async_runtime::AsyncTerminalOutputWriteReport,
        > {
            Box::pin(async move {
                let pending = self.pending_output_bytes();
                if pending == 0 {
                    return Ok(
                        crate::host::async_runtime::AsyncTerminalOutputWriteReport::completed(0),
                    );
                }
                let remaining = pending - 1;
                self.state
                    .pending_output_bytes
                    .store(remaining, std::sync::atomic::Ordering::SeqCst);
                Ok(crate::host::async_runtime::AsyncTerminalOutputWriteReport {
                    bytes_written: 1,
                    completed: remaining == 0,
                    pending_bytes: remaining,
                })
            })
        }

        fn write_owned_styled_output_with_modes_bounded<'a>(
            &'a mut self,
            _lines: Vec<String>,
            _line_style_spans: Vec<Vec<super::super::TerminalStyleSpan>>,
            _modes: super::super::AttachedTerminalOutputModes,
            _max_bytes: usize,
        ) -> crate::host::async_runtime::AsyncTerminalIoFuture<
            'a,
            crate::host::async_runtime::AsyncTerminalOutputWriteReport,
        > {
            Box::pin(async move {
                const RETAINED_BYTES: usize = 128;
                self.state
                    .pending_output_bytes
                    .store(RETAINED_BYTES - 1, std::sync::atomic::Ordering::SeqCst);
                self.state.first_output_pass.notify_one();
                Ok(crate::host::async_runtime::AsyncTerminalOutputWriteReport {
                    bytes_written: 1,
                    completed: false,
                    pending_bytes: RETAINED_BYTES - 1,
                })
            })
        }
    }

    /// Verifies negotiated v3 renders its initial pushed snapshot and exits on
    /// terminal EOF without issuing the legacy RTT-bound `terminal/view`.
    #[tokio::test(flavor = "current_thread")]
    async fn primary_v3_initial_snapshot_requires_no_terminal_view_request() {
        let (mut client_stream, _server_stream) = tokio::io::duplex(16 * 1024);
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.push_pending_input_read();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(IrohAttachRenderWakeup::pushed_snapshot(
                super::super::event_stream::IrohPushedRenderSnapshot {
                    revision: 1,
                    frame: super::super::AttachClientFrame {
                        lines: vec!["pushed initial".to_string()],
                        line_style_spans: vec![Vec::new()],
                        modes: super::super::AttachedTerminalOutputModes::default(),
                        iroh_status_slot: None,
                        event_cutoff: Some(7),
                    },
                    invalidate_output: true,
                },
            )))
            .await
            .unwrap();

        run_iroh_attached_primary_client_loop_async_with_events(
            &mut client_stream,
            &mut terminal_io,
            None,
            ClientId::parse('c', "c1".to_string()).unwrap(),
            Size::new(80, 24).unwrap(),
            std::time::Duration::from_millis(50),
            Some(&mut receiver),
            true,
        )
        .await
        .unwrap();

        assert_eq!(terminal_io.written_frames.len(), 1);
        assert_eq!(terminal_io.written_frames[0].lines, ["pushed initial"]);
        assert_eq!(terminal_io.invalidated_output_frames, 1);
    }

    /// Verifies consecutive primary resize mutations use distinct idempotency
    /// keys even when pushed rendering avoids an intervening view request.
    ///
    /// Reusing the resize sequence for different terminal geometries causes
    /// the server to reject the second request as conflicting idempotency data,
    /// after which the client closes the retained Iroh control stream.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn primary_v3_consecutive_resizes_use_distinct_idempotency_keys() {
        let (mut client_stream, mut server_stream) = tokio::io::duplex(16 * 1024);
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.push_terminal_size(Some(Size::new(100, 30).unwrap()));
        terminal_io.push_terminal_size(Some(Size::new(120, 40).unwrap()));
        terminal_io.push_pending_input_read();

        let server = async {
            let mut keys = Vec::new();
            for expected_size in [(100, 30), (120, 40)] {
                let request =
                    read_async_control_response_frames(&mut server_stream, 1024 * 1024, 1)
                        .await
                        .unwrap();
                let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(
                    parsed.get("method").and_then(serde_json::Value::as_str),
                    Some("terminal/step")
                );
                let params = parsed.get("params").unwrap();
                assert_eq!(
                    params
                        .get("client_size")
                        .and_then(|size| size.get("columns"))
                        .and_then(serde_json::Value::as_u64),
                    Some(expected_size.0)
                );
                assert_eq!(
                    params
                        .get("client_size")
                        .and_then(|size| size.get("rows"))
                        .and_then(serde_json::Value::as_u64),
                    Some(expected_size.1)
                );
                keys.push(
                    params
                        .get("idempotency_key")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .to_string(),
                );

                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": parsed.get("id").unwrap(),
                    "result": {
                        "input_bytes": 0,
                        "application": {
                            "forwarded_bytes": 0,
                            "mux_actions_applied": 0,
                            "mouse_actions_reported": 0,
                            "agent_prompt_inputs_applied": 0,
                            "view_refresh_required": false,
                            "full_redraw_required": false,
                            "unsupported_actions": []
                        },
                        "view": null,
                        "ui_theme": null,
                        "session_terminated": false
                    }
                })
                .to_string();
                tokio::io::AsyncWriteExt::write_all(
                    &mut server_stream,
                    &super::super::encode_control_body(&response),
                )
                .await
                .unwrap();
                tokio::io::AsyncWriteExt::flush(&mut server_stream)
                    .await
                    .unwrap();
            }
            keys
        };
        let client = run_iroh_attached_primary_client_loop_async_with_events(
            &mut client_stream,
            &mut terminal_io,
            None,
            ClientId::parse('c', "c1".to_string()).unwrap(),
            Size::new(80, 24).unwrap(),
            std::time::Duration::from_millis(50),
            None,
            true,
        );
        let (client, keys) = tokio::join!(client, server);
        client.unwrap();

        assert_eq!(
            keys,
            [
                "cli-c1-terminal-resize-0".to_string(),
                "cli-c1-terminal-resize-1".to_string(),
            ]
        );
        assert_eq!(terminal_io.invalidated_output_frames, 2);
    }

    /// Verifies an authoritative v3 update is presented while the matching
    /// terminal-step acknowledgement is still delayed by the control RTT.
    #[tokio::test(flavor = "current_thread")]
    async fn primary_v3_presents_pushed_snapshot_before_step_acknowledgement() {
        let (mut client_stream, mut server_stream) = tokio::io::duplex(16 * 1024);
        let rendered = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.notify_on_write(rendered.clone());
        terminal_io.push_input(b"x".to_vec());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let server = async {
            let request = read_async_control_response_frames(&mut server_stream, 1024 * 1024, 1)
                .await
                .unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            assert!(body.contains(r#""method":"terminal/step""#), "{body}");
            sender
                .send(Ok(IrohAttachRenderWakeup::pushed_snapshot(
                    super::super::event_stream::IrohPushedRenderSnapshot {
                        revision: 2,
                        frame: super::super::AttachClientFrame {
                            lines: vec!["visible before acknowledgement".to_string()],
                            line_style_spans: vec![Vec::new()],
                            modes: super::super::AttachedTerminalOutputModes::default(),
                            iroh_status_slot: None,
                            event_cutoff: Some(8),
                        },
                        invalidate_output: false,
                    },
                )))
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(50), rendered.notified())
                .await
                .expect("pushed v3 frame must render before the control acknowledgement");
            tokio::io::AsyncWriteExt::write_all(
                &mut server_stream,
                &super::super::encode_control_body(
                    r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
                ),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut server_stream)
                .await
                .unwrap();
        };
        let client = run_iroh_attached_primary_client_loop_async_with_events(
            &mut client_stream,
            &mut terminal_io,
            None,
            ClientId::parse('c', "c1".to_string()).unwrap(),
            Size::new(80, 24).unwrap(),
            std::time::Duration::from_millis(200),
            Some(&mut receiver),
            true,
        );
        let (client, ()) = tokio::join!(client, server);
        client.unwrap();

        assert_eq!(terminal_io.written_frames.len(), 1);
        assert_eq!(
            terminal_io.written_frames[0].lines,
            ["visible before acknowledgement"]
        );
    }

    /// Verifies incomplete v3 terminal output cannot starve acknowledgement
    /// handling or capture of the next keystroke. The second ordered step must
    /// reach the server while bytes from the pushed frame remain retained.
    #[tokio::test(flavor = "current_thread")]
    async fn primary_v3_routes_follow_on_input_before_pushed_output_drains() {
        let (mut client_stream, mut server_stream) = tokio::io::duplex(16 * 1024);
        let (mut terminal_io, terminal_state) = SlowBoundedTerminalIo::new();
        terminal_state
            .input
            .lock()
            .unwrap()
            .push_back(b"x".to_vec());
        terminal_state.input_ready.notify_one();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let server = async {
            let first_request =
                read_async_control_response_frames(&mut server_stream, 1024 * 1024, 1)
                    .await
                    .unwrap();
            let (body, _) = decode_control_frame(&first_request, 1024 * 1024).unwrap();
            assert!(body.contains(r#""input_bytes":[120]"#), "{body}");
            sender
                .send(Ok(IrohAttachRenderWakeup::pushed_snapshot(
                    super::super::event_stream::IrohPushedRenderSnapshot {
                        revision: 3,
                        frame: super::super::AttachClientFrame {
                            lines: vec!["large pushed frame".to_string()],
                            line_style_spans: vec![Vec::new()],
                            modes: super::super::AttachedTerminalOutputModes::default(),
                            iroh_status_slot: None,
                            event_cutoff: Some(9),
                        },
                        invalidate_output: false,
                    },
                )))
                .await
                .unwrap();
            terminal_state.first_output_pass.notified().await;
            terminal_state
                .input
                .lock()
                .unwrap()
                .push_back(b"y".to_vec());
            terminal_state.input_ready.notify_one();
            tokio::io::AsyncWriteExt::write_all(
                &mut server_stream,
                &super::super::encode_control_body(
                    r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
                ),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut server_stream)
                .await
                .unwrap();

            let second_request =
                read_async_control_response_frames(&mut server_stream, 1024 * 1024, 1)
                    .await
                    .unwrap();
            let (body, _) = decode_control_frame(&second_request, 1024 * 1024).unwrap();
            assert!(body.contains(r#""input_bytes":[121]"#), "{body}");
            assert!(
                terminal_state
                    .pending_output_bytes
                    .load(std::sync::atomic::Ordering::SeqCst)
                    > 0,
                "follow-on input must advance before the pushed frame drains"
            );
            tokio::io::AsyncWriteExt::write_all(
                &mut server_stream,
                &super::super::encode_control_body(
                    r#"{"jsonrpc":"2.0","id":"cli-terminal-step-1","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null,"client_detached":true}}"#,
                ),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut server_stream)
                .await
                .unwrap();
        };
        let client = run_iroh_attached_primary_client_loop_async_with_events(
            &mut client_stream,
            &mut terminal_io,
            None,
            ClientId::parse('c', "c1".to_string()).unwrap(),
            Size::new(80, 24).unwrap(),
            std::time::Duration::from_millis(200),
            Some(&mut receiver),
            true,
        );
        let (client, ()) = tokio::join!(client, server);
        client.unwrap();
    }
}

/// Runs the primary control-socket attach terminal loop over async terminal I/O.
///
/// The control socket and terminal endpoint both use Tokio I/O in this path.
/// Runtime state is still mutated by the daemon-side control handler; this loop
/// only coordinates foreground terminal bytes, rendered frames, and framed
/// control requests.
#[cfg(test)]
pub(in crate::cli) async fn run_control_socket_attached_primary_client_loop_async<I>(
    stream: &mut tokio::net::UnixStream,
    terminal_io: &mut I,
    primary_client_id: ClientId,
    mut client_size: Size,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut render_requested = true;
    let mut size_refresh = AttachTerminalSizeRefresh::default();

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
            if !request_primary_resize_async(stream, &primary_client_id, client_size, iteration)
                .await?
                .connected
            {
                break Ok(());
            }
            iteration = iteration.saturating_add(1);
            render_requested = true;
        }
        let input = read_attached_client_input_or_deadline(
            terminal_io,
            4096,
            None,
            size_refresh.deadline(),
        )
        .await?;
        size_refresh.reschedule();
        if input.eof {
            break Ok(());
        }
        if input.bytes.is_empty() && !render_requested {
            if control_socket_disconnected_without_pending_response(stream)? {
                break Ok(());
            }
            continue;
        }
        if input.bytes.is_empty() {
            if !request_and_render_primary_view_async(
                stream,
                terminal_io,
                client_size,
                iteration,
                cursor_blink_epoch,
            )
            .await?
            .connected
            {
                break Ok(());
            }
            render_requested = false;
            iteration = iteration.saturating_add(1);
            continue;
        }
        let request = terminal_step_control_request(
            iteration,
            &primary_client_id,
            client_size,
            input.bytes.as_slice(),
            false,
        );
        if !write_async_control_body_or_disconnected(stream, &request).await? {
            break Ok(());
        }
        let Some(response) =
            read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
        else {
            break Ok(());
        };
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        if control_response_forbidden(body.as_str())? {
            break Ok(());
        }
        let refresh_requirement = terminal_step_response_refresh_requirement(body.as_str())?;
        if refresh_requirement.client_detached || refresh_requirement.session_terminated {
            break Ok(());
        }
        if refresh_requirement.full_redraw_required {
            terminal_io.invalidate_output_frame().await?;
        }
        if (render_requested || refresh_requirement.view_refresh_required)
            && !request_and_render_primary_view_async(
                stream,
                terminal_io,
                client_size,
                iteration,
                cursor_blink_epoch,
            )
            .await?
            .connected
        {
            break Ok(());
        }
        render_requested = false;
        iteration = iteration.saturating_add(1);
    }
}
/// Runs the primary control-socket attach terminal loop with runtime event wakeups.
///
/// The event stream is optional so clients can still attach to daemons started
/// without an auxiliary event socket. When runtime events are available, any
/// received event wakes the loop for an explicit `terminal/view` request rather
/// than waiting for the next terminal input timeout.
pub(in crate::cli) async fn run_control_socket_attached_primary_client_loop_async_with_runtime_events<
    I,
>(
    stream: &mut tokio::net::UnixStream,
    terminal_io: &mut I,
    primary_client_id: ClientId,
    mut client_size: Size,
    event_stream: Option<tokio::net::UnixStream>,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut render_requested = true;
    let mut event_stream = event_stream.map(AttachedRuntimeEventStream::new);
    let mut animation_refresh = AttachAnimationRefresh::default();
    let mut size_refresh = AttachTerminalSizeRefresh::default();
    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
            if !request_primary_resize_async(stream, &primary_client_id, client_size, iteration)
                .await?
                .connected
            {
                break Ok(());
            }
            iteration = iteration.saturating_add(1);
            render_requested = true;
        }
        let input = read_attached_client_input_or_runtime_event(
            terminal_io,
            event_stream.as_mut(),
            4096,
            animation_refresh.deadline(),
            size_refresh.deadline(),
        )
        .await?;
        size_refresh.reschedule();
        if input.eof {
            break Ok(());
        }
        match input.render_action {
            AttachRenderAction::None => {}
            AttachRenderAction::View => {
                render_requested = true;
            }
            AttachRenderAction::InvalidateAndView => {
                terminal_io.invalidate_output_frame().await?;
                render_requested = true;
            }
            AttachRenderAction::Disconnect => break Ok(()),
        }
        if input.bytes.is_empty() && !render_requested {
            if control_socket_disconnected_without_pending_response(stream)? {
                break Ok(());
            }
            continue;
        }
        if input.bytes.is_empty() {
            let outcome = request_and_render_primary_view_async(
                stream,
                terminal_io,
                client_size,
                iteration,
                cursor_blink_epoch,
            )
            .await?;
            if !outcome.connected {
                break Ok(());
            }
            animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
            render_requested = false;
            iteration = iteration.saturating_add(1);
            continue;
        }
        let request = terminal_step_control_request(
            iteration,
            &primary_client_id,
            client_size,
            input.bytes.as_slice(),
            false,
        );
        if !write_async_control_body_or_disconnected(stream, &request).await? {
            break Ok(());
        }
        let Some(response) =
            read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
        else {
            break Ok(());
        };
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        if control_response_forbidden(body.as_str())? {
            break Ok(());
        }
        let refresh_requirement = terminal_step_response_refresh_requirement(body.as_str())?;
        if refresh_requirement.client_detached || refresh_requirement.session_terminated {
            break Ok(());
        }
        if refresh_requirement.full_redraw_required {
            terminal_io.invalidate_output_frame().await?;
        }
        if let Some(event_stream) = event_stream.as_mut() {
            match event_stream.try_read_ready_render_action()? {
                AttachRenderAction::None => {}
                AttachRenderAction::View => {
                    render_requested = true;
                }
                AttachRenderAction::InvalidateAndView => {
                    terminal_io.invalidate_output_frame().await?;
                    render_requested = true;
                }
                AttachRenderAction::Disconnect => break Ok(()),
            }
        }
        if render_requested || refresh_requirement.view_refresh_required {
            let outcome = request_and_render_primary_view_async(
                stream,
                terminal_io,
                client_size,
                iteration,
                cursor_blink_epoch,
            )
            .await?;
            if !outcome.connected {
                break Ok(());
            }
            animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
        }
        render_requested = false;
        iteration = iteration.saturating_add(1);
    }
}
