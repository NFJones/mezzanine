//! Primary control-socket attach setup and interactive loop.

#[cfg(test)]
use super::event_stream::read_attached_client_input_or_deadline;
use super::event_stream::{
    AttachRenderAction, AttachedRuntimeEventStream,
    control_socket_disconnected_without_pending_response, optional_control_socket_event_stream,
    read_attached_client_input_or_runtime_event,
};
use super::requests::{
    read_async_control_response_frames_or_disconnected, refresh_attached_client_size_async,
    request_and_render_primary_view_async, request_primary_resize_async,
    terminal_step_control_request, write_async_control_body_or_disconnected,
};
use super::responses::{control_response_forbidden, terminal_step_response_refresh_requirement};
use super::*;

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
) -> Result<()> {
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let control_stream = stream.try_clone()?;
    control_stream.set_nonblocking(true)?;
    let mut control_stream = tokio::net::UnixStream::from_std(control_stream)?;
    let event_stream = optional_control_socket_event_stream(control_socket_path)?;
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
