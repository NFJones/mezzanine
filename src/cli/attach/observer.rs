//! Observer control-socket attach setup and presentation loop.

use super::event_stream::read_attached_client_input_or_deadline;
use super::requests::{
    control_socket_cursor_blink_elapsed, observer_inspect_control_request,
    read_async_control_response_frames_or_disconnected, refresh_attached_client_size_async,
    terminal_view_control_request, write_async_control_body_or_disconnected,
    write_styled_output_or_disconnected_async,
};
use super::responses::{
    ObserverAttachState, observer_attach_state_from_inspect_response,
    terminal_step_response_line_style_spans, terminal_step_response_lines,
    terminal_step_response_output_modes,
};
use super::*;

/// Runs the run control socket attached observer client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::cli) async fn run_control_socket_attached_observer_client(
    stream: &mut UnixStream,
    observer_request_id: String,
    client_size: Size,
) -> Result<()> {
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let control_stream = stream.try_clone()?;
    control_stream.set_nonblocking(true)?;
    let mut control_stream = tokio::net::UnixStream::from_std(control_stream)?;
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_control_socket_attached_observer_client_loop_async(
        &mut control_stream,
        terminal_guard.io_mut(),
        observer_request_id,
        client_size,
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
pub(in crate::cli) async fn run_control_socket_attached_observer_client_loop_async<I>(
    stream: &mut tokio::net::UnixStream,
    terminal_io: &mut I,
    observer_request_id: String,
    mut client_size: Size,
) -> Result<()>
where
    I: AsyncAttachedTerminalIo,
{
    terminal_io.enter_presentation().await?;
    let mut iteration = 0u64;
    let cursor_blink_epoch = std::time::Instant::now();
    let mut approved = false;
    let mut size_refresh = AttachTerminalSizeRefresh::default();

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
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

        let request = if approved {
            terminal_view_control_request(iteration, client_size)
        } else {
            observer_inspect_control_request(iteration, &observer_request_id)
        };
        if !write_async_control_body_or_disconnected(stream, &request).await? {
            break Ok(());
        }
        let Some(response) =
            read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
        else {
            break Ok(());
        };
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        if !approved {
            match observer_attach_state_from_inspect_response(body.as_str())? {
                ObserverAttachState::Pending => {
                    if !write_styled_output_or_disconnected_async(
                        terminal_io,
                        &["observer pending approval".to_string()],
                        &[],
                        AttachedTerminalOutputModes::default(),
                    )
                    .await?
                    {
                        break Ok(());
                    }
                }
                ObserverAttachState::Approved => {
                    approved = true;
                }
                ObserverAttachState::Rejected => {
                    let line = "observer request rejected".to_string();
                    let _ = write_styled_output_or_disconnected_async(
                        terminal_io,
                        &[line],
                        &[],
                        AttachedTerminalOutputModes::default(),
                    )
                    .await?;
                    break Ok(());
                }
                ObserverAttachState::Revoked => {
                    let line = "observer access revoked".to_string();
                    let _ = write_styled_output_or_disconnected_async(
                        terminal_io,
                        &[line],
                        &[],
                        AttachedTerminalOutputModes::default(),
                    )
                    .await?;
                    break Ok(());
                }
            }
            iteration = iteration.saturating_add(1);
            continue;
        }
        let mut lines = terminal_step_response_lines(body.as_str())?;
        let line_style_spans = terminal_step_response_line_style_spans(body.as_str())?;
        if lines.is_empty() {
            lines.push("observer pending approval".to_string());
        }
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
