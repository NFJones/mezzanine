//! Observer control-socket attach setup and presentation loop.

use super::event_stream::{
    AttachRenderAction, AttachedRuntimeEventStream, IrohAttachRenderWakeup,
    optional_control_socket_event_stream, read_attached_client_input_or_deadline,
    read_attached_client_input_or_iroh_event, read_attached_client_input_or_runtime_event,
};
use super::requests::{
    refresh_attached_client_size_async, render_attach_client_frame_async,
    render_iroh_attach_client_frame_async, request_observer_resize_async,
    request_primary_view_frame_async,
};
use super::{
    AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachAnimationRefresh, AttachTerminalSizeRefresh, ClientId, MezError, Result, Size,
    UnixStream, io,
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
        None,
        None,
        client_size,
        std::time::Duration::from_secs(30),
        event_stream.as_mut(),
        None,
        false,
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
    connection: &iroh::endpoint::Connection,
    observer_client_id: ClientId,
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
    let run_result = run_attached_observer_client_loop_async(
        stream,
        terminal_guard.io_mut(),
        Some(connection),
        Some(observer_client_id),
        client_size,
        request_timeout,
        None,
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
    run_attached_observer_client_loop_async(
        stream,
        terminal_io,
        None,
        None,
        client_size,
        std::time::Duration::from_secs(30),
        None,
        None,
        false,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "control stream, terminal I/O, connection health, observer identity, geometry, event delivery, and render ownership are independent attach inputs"
)]
async fn run_attached_observer_client_loop_async<I, S>(
    stream: &mut S,
    terminal_io: &mut I,
    connection: Option<&iroh::endpoint::Connection>,
    observer_client_id: Option<ClientId>,
    mut client_size: Size,
    request_timeout: std::time::Duration,
    mut event_stream: Option<&mut AttachedRuntimeEventStream>,
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
    let mut size_refresh = AttachTerminalSizeRefresh::default();
    let mut animation_refresh = AttachAnimationRefresh::default();
    let mut health = super::AttachIrohHealthTracker::default();
    let mut cached_frame: Option<super::AttachClientFrame> = None;
    let mut render_requested = !pushed_render_owner;

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
            if pushed_render_owner {
                let observer_client_id = observer_client_id.as_ref().ok_or_else(|| {
                    MezError::invalid_state("pushed observer render owner omitted its client id")
                })?;
                let outcome = tokio::time::timeout(
                    request_timeout,
                    request_observer_resize_async(
                        stream,
                        observer_client_id,
                        client_size,
                        iteration,
                    ),
                )
                .await
                .map_err(|_| {
                    MezError::invalid_state(
                        "Iroh observer resize acknowledgement timed out; reattach required",
                    )
                })??;
                if !outcome.connected {
                    return Err(MezError::invalid_state(
                        "Iroh observer attach disconnected during terminal resize; reattach required",
                    ));
                }
                iteration = iteration.saturating_add(1);
            }
            render_requested = !pushed_render_owner;
        }
        let wake_deadline = connection
            .map(|_| health.deadline().min(size_refresh.deadline()))
            .unwrap_or_else(|| size_refresh.deadline());
        let input = match (event_stream.as_deref_mut(), event_receiver.as_deref_mut()) {
            (Some(event_stream), None) => {
                read_attached_client_input_or_runtime_event(
                    terminal_io,
                    Some(event_stream),
                    4096,
                    animation_refresh.deadline(),
                    wake_deadline,
                )
                .await?
            }
            (None, Some(event_receiver)) => {
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
            (None, None) => {
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
            (Some(_), Some(_)) => {
                return Err(MezError::invalid_state(
                    "observer attach cannot use Unix and Iroh event streams together",
                ));
            }
        };
        size_refresh.reschedule();
        let received_pushed_snapshot = input.pushed_snapshot.is_some();
        if let Some(snapshot) = input.pushed_snapshot {
            if snapshot.invalidate_output {
                terminal_io.invalidate_output_frame().await?;
            }
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
                    "Iroh observer attach disconnected while rendering a pushed snapshot",
                ));
            }
            cached_frame = Some(snapshot.frame);
            animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
            render_requested = false;
        }
        match input.render_action {
            AttachRenderAction::None => {}
            AttachRenderAction::View if pushed_render_owner && !received_pushed_snapshot => {
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
                            "Iroh observer terminal disconnected during local animation repaint",
                        ));
                    }
                    animation_refresh
                        .update_from_rendered_view(outcome.animation_refresh_interval_ms);
                }
            }
            AttachRenderAction::View => render_requested = !pushed_render_owner,
            AttachRenderAction::InvalidateAndView => {
                terminal_io.invalidate_output_frame().await?;
                render_requested = !pushed_render_owner;
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
                    "runtime event stream disconnected; reattach required",
                ));
            }
        }
        if input.eof {
            break Ok(());
        }
        if !input.bytes.is_empty() {
            render_requested = !pushed_render_owner;
        }
        let quality_changed = connection.is_some_and(|connection| {
            health.deadline() <= tokio::time::Instant::now() && health.sample(connection)
        });
        if !render_requested && quality_changed {
            if let Some(frame) = cached_frame.as_ref()
                && !render_iroh_attach_client_frame_async(
                    terminal_io,
                    frame,
                    true,
                    health.quality(),
                    cursor_blink_epoch,
                )
                .await?
                .connected
            {
                break Ok(());
            }
            continue;
        }
        if !render_requested {
            continue;
        }
        let Some(frame) = request_primary_view_frame_async(stream, client_size, iteration).await?
        else {
            break Ok(());
        };
        if let Some(connection) = connection {
            health.sample(connection);
        }
        let outcome = if connection.is_some() {
            render_iroh_attach_client_frame_async(
                terminal_io,
                &frame,
                true,
                health.quality(),
                cursor_blink_epoch,
            )
            .await?
        } else {
            render_attach_client_frame_async(terminal_io, &frame, cursor_blink_epoch).await?
        };
        if !outcome.connected {
            break Ok(());
        }
        cached_frame = Some(frame);
        animation_refresh.update_from_rendered_view(outcome.animation_refresh_interval_ms);
        render_requested = false;
        iteration = iteration.saturating_add(1);
    }
}

#[cfg(test)]
mod pushed_snapshot_tests {
    use super::*;

    /// Verifies negotiated observer v3 renders its initial pushed snapshot and
    /// exits on terminal EOF without issuing the legacy `terminal/view` request.
    #[tokio::test(flavor = "current_thread")]
    async fn observer_v3_initial_snapshot_requires_no_terminal_view_request() {
        let (mut client_stream, _server_stream) = tokio::io::duplex(16 * 1024);
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.push_pending_input_read();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(IrohAttachRenderWakeup::pushed_snapshot(
                super::super::event_stream::IrohPushedRenderSnapshot {
                    revision: 1,
                    frame: super::super::AttachClientFrame {
                        lines: vec!["pushed observer".to_string()],
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

        run_attached_observer_client_loop_async(
            &mut client_stream,
            &mut terminal_io,
            None,
            Some(ClientId::parse('c', "c2".to_string()).unwrap()),
            Size::new(80, 24).unwrap(),
            std::time::Duration::from_millis(50),
            None,
            Some(&mut receiver),
            true,
        )
        .await
        .unwrap();

        assert_eq!(terminal_io.written_frames.len(), 1);
        assert_eq!(terminal_io.written_frames[0].lines, ["pushed observer"]);
        assert_eq!(terminal_io.invalidated_output_frames, 1);
    }
}
