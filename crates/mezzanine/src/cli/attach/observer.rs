//! Observer control-socket attach setup and presentation loop.

use super::event_stream::{
    AttachRenderAction, AttachedRuntimeEventStream, optional_control_socket_event_stream,
    read_attached_client_input_or_deadline, read_attached_client_input_or_iroh_event,
    read_attached_client_input_or_runtime_event,
};
use super::requests::{
    control_socket_cursor_blink_elapsed, read_async_control_response_frames_or_disconnected,
    refresh_attached_client_size_async, terminal_view_control_request,
    write_async_control_body_or_disconnected, write_styled_output_or_disconnected_async,
};
use super::responses::{
    terminal_step_response_line_style_spans, terminal_step_response_lines,
    terminal_step_response_output_modes,
};
use super::{
    AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachTerminalSizeRefresh, MezError, Result, Size, UnixStream, decode_control_frame, io,
};

/// Runs the run control socket attached observer client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::cli) async fn run_control_socket_attached_observer_client(
    stream: &mut UnixStream,
    control_socket_path: &std::path::Path,
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
    let mut event_stream = event_stream.map(AttachedRuntimeEventStream::new);
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_attached_observer_client_loop_async(
        &mut control_stream,
        terminal_guard.io_mut(),
        client_size,
        event_stream.as_mut(),
        None,
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

/// Runs an observer attach over one persistent Iroh control stream.
pub(in crate::cli) async fn run_iroh_attached_observer_client<S>(
    stream: &mut S,
    client_size: Size,
    mut event_receiver: tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_attached_observer_client_loop_async(
        stream,
        terminal_guard.io_mut(),
        client_size,
        None,
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

/// Runs the observer control-socket attach terminal loop over async terminal I/O.
///
/// Observers ignore local input after draining it from the terminal, but they
/// still use the async terminal boundary for readiness, resize, presentation,
/// and styled output so observer attachment follows the same terminal ownership
/// model as primary attachment.
#[cfg(test)]
pub(in crate::cli) async fn run_control_socket_attached_observer_client_loop_async<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    client_size: Size,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    run_attached_observer_client_loop_async(stream, terminal_io, client_size, None, None).await
}

async fn run_attached_observer_client_loop_async<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    mut client_size: Size,
    mut event_stream: Option<&mut AttachedRuntimeEventStream>,
    mut event_receiver: Option<&mut tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>>,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut size_refresh = AttachTerminalSizeRefresh::default();

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
        }
        let input = match (event_stream.as_deref_mut(), event_receiver.as_deref_mut()) {
            (Some(event_stream), None) => {
                read_attached_client_input_or_runtime_event(
                    terminal_io,
                    Some(event_stream),
                    4096,
                    None,
                    size_refresh.deadline(),
                )
                .await?
            }
            (None, Some(event_receiver)) => {
                read_attached_client_input_or_iroh_event(
                    terminal_io,
                    event_receiver,
                    4096,
                    size_refresh.deadline(),
                )
                .await?
            }
            (None, None) => {
                read_attached_client_input_or_deadline(
                    terminal_io,
                    4096,
                    None,
                    size_refresh.deadline(),
                )
                .await?
            }
            (Some(_), Some(_)) => {
                return Err(MezError::invalid_state(
                    "observer attach cannot use Unix and Iroh event streams together",
                ));
            }
        };
        size_refresh.reschedule();
        match input.render_action {
            AttachRenderAction::None | AttachRenderAction::View => {}
            AttachRenderAction::InvalidateAndView => {
                terminal_io.invalidate_output_frame().await?;
            }
            AttachRenderAction::Disconnect => {
                return Err(MezError::invalid_state(
                    "runtime event stream disconnected; reattach required",
                ));
            }
        }
        if input.eof {
            break Ok(());
        }

        let request = terminal_view_control_request(iteration, client_size);
        if !write_async_control_body_or_disconnected(stream, &request).await? {
            break Ok(());
        }
        let Some(response) =
            read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
        else {
            break Ok(());
        };
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        let lines = terminal_step_response_lines(body.as_str())?;
        let line_style_spans = terminal_step_response_line_style_spans(body.as_str())?;
        let modes = control_socket_cursor_blink_elapsed(
            terminal_step_response_output_modes(body.as_str())?.unwrap_or_default(),
            cursor_blink_epoch,
        );
        if !write_styled_output_or_disconnected_async(terminal_io, &lines, &line_style_spans, modes)
            .await?
        {
            break Ok(());
        }
        iteration = iteration.saturating_add(1);
    }
}
