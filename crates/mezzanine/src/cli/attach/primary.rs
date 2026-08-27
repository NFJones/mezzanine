//! Primary control-socket attach setup and interactive loop.

use super::event_stream::read_attached_client_input_or_deadline;
use super::event_stream::{
    AttachRenderAction, AttachedRuntimeEventStream, IrohAttachRenderWakeup,
    control_socket_disconnected_without_pending_response, optional_control_socket_event_stream,
    read_attached_client_input_or_iroh_event, read_attached_client_input_or_runtime_event,
};
use super::requests::{
    read_async_control_response_frames, read_async_control_response_frames_or_disconnected,
    refresh_attached_client_size_async, render_iroh_attach_client_frame_async,
    request_and_render_primary_view_async, request_primary_resize_async,
    request_primary_view_frame_async, terminal_step_control_request,
    write_async_control_body_or_disconnected,
};
use super::responses::{control_response_forbidden, terminal_step_response_refresh_requirement};
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
    )
    .await
}

async fn run_iroh_attached_primary_client_loop_async_with_events<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    connection: Option<&iroh::endpoint::Connection>,
    primary_client_id: ClientId,
    mut client_size: Size,
    request_timeout: std::time::Duration,
    mut event_receiver: Option<&mut tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>>,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut render_requested = true;
    let mut size_refresh = AttachTerminalSizeRefresh::default();
    let mut animation_refresh = AttachAnimationRefresh::default();
    let mut health = super::AttachIrohHealthTracker::default();
    let mut cached_frame: Option<super::AttachClientFrame> = None;
    loop {
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
            render_requested = true;
        }
        let wake_deadline = connection
            .map(|_| health.deadline().min(size_refresh.deadline()))
            .unwrap_or_else(|| size_refresh.deadline());
        let input = match event_receiver.as_deref_mut() {
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
        };
        size_refresh.reschedule();
        match input.render_action {
            AttachRenderAction::None => {}
            AttachRenderAction::View => render_requested = true,
            AttachRenderAction::InvalidateAndView => {
                terminal_io.invalidate_output_frame().await?;
                render_requested = true;
            }
            AttachRenderAction::Disconnect => {
                if let Some(frame) = cached_frame.as_ref() {
                    let _ = render_iroh_attach_client_frame_async(
                        terminal_io,
                        frame,
                        false,
                        health.quality(),
                        cursor_blink_epoch,
                    )
                    .await;
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
            let quality_changed = connection.is_some_and(|connection| {
                health.deadline() <= tokio::time::Instant::now() && health.sample(connection)
            });
            if !render_requested && quality_changed {
                if let Some(frame) = cached_frame.as_ref() {
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

        let request = terminal_step_control_request(
            iteration,
            &primary_client_id,
            client_size,
            input.bytes.as_slice(),
            false,
        );
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
        let response = match tokio::time::timeout(
            request_timeout,
            read_async_control_response_frames(stream, 1024 * 1024, 1),
        )
        .await
        {
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
        if refresh_requirement.client_detached || refresh_requirement.session_terminated {
            return Ok(());
        }
        if refresh_requirement.full_redraw_required {
            terminal_io.invalidate_output_frame().await?;
        }
        if render_requested || refresh_requirement.view_refresh_required {
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
        }
        render_requested = false;
        iteration = iteration.saturating_add(1);
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
