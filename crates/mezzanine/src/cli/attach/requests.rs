//! Attached-client JSON-RPC request construction and control transport helpers.

use super::responses::{
    control_response_forbidden, terminal_step_response_line_style_spans,
    terminal_step_response_lines, terminal_step_response_output_modes,
    terminal_step_response_refresh_requirement,
};
use super::{
    AsyncAttachedTerminalIo, AttachedTerminalOutputModes, ClientId, MezError,
    PrimaryResizeRequestOutcome, PrimaryViewRenderOutcome, Result, Size, TerminalStyleSpan,
    attached_terminal_output_disconnected, decode_control_frame, encode_control_body,
    incomplete_control_response_error, json_escape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Runs the terminal step control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::cli) fn terminal_step_control_request(
    iteration: u64,
    primary_client_id: &ClientId,
    client_size: Size,
    input: &[u8],
    render: bool,
) -> String {
    let input_bytes = input
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"jsonrpc":"2.0","id":"cli-terminal-step-{iteration}","method":"terminal/step","params":{{"idempotency_key":"cli-{}-terminal-step-{iteration}","client_size":{{"columns":{},"rows":{}}},"render":{},"input_bytes":[{}]}}}}"#,
        json_escape(primary_client_id.as_str()),
        client_size.columns,
        client_size.rows,
        render,
        input_bytes
    )
}
/// Builds a mutation-only terminal-step request for a detected terminal resize.
pub(super) fn terminal_resize_control_request(
    iteration: u64,
    primary_client_id: &ClientId,
    client_size: Size,
) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":"cli-terminal-resize-{iteration}","method":"terminal/step","params":{{"idempotency_key":"cli-{}-terminal-resize-{iteration}","client_size":{{"columns":{},"rows":{}}},"render":false,"input_bytes":[]}}}}"#,
        json_escape(primary_client_id.as_str()),
        client_size.columns,
        client_size.rows,
    )
}

/// Runs the refresh attached client size async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn refresh_attached_client_size_async<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    client_size: &mut Size,
) -> Result<bool> {
    let Some(size) = terminal_io.terminal_size().await? else {
        return Ok(false);
    };
    if size == *client_size {
        return Ok(false);
    }
    *client_size = size;
    Ok(true)
}

/// Runs the write styled output or disconnected async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn write_styled_output_or_disconnected_async<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    modes: AttachedTerminalOutputModes,
) -> Result<bool> {
    match terminal_io
        .write_styled_output_with_modes(lines, line_style_spans, modes)
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if attached_terminal_output_disconnected(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Requests an explicit terminal view and writes the rendered frame locally.
pub(super) async fn request_and_render_primary_view_async<I: AsyncAttachedTerminalIo>(
    stream: &mut tokio::net::UnixStream,
    terminal_io: &mut I,
    client_size: Size,
    iteration: u64,
    cursor_blink_epoch: std::time::Instant,
) -> Result<PrimaryViewRenderOutcome> {
    let request = terminal_view_control_request(iteration, client_size);
    if !write_async_control_body_or_disconnected(stream, &request).await? {
        return Ok(PrimaryViewRenderOutcome::disconnected());
    }
    let Some(response) =
        read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
    else {
        return Ok(PrimaryViewRenderOutcome::disconnected());
    };
    let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
    if control_response_forbidden(body.as_str())? {
        return Ok(PrimaryViewRenderOutcome::disconnected());
    }
    render_primary_view_response_async(terminal_io, body.as_str(), cursor_blink_epoch).await
}
/// Notifies the runtime that the attached primary terminal size changed.
pub(super) async fn request_primary_resize_async(
    stream: &mut tokio::net::UnixStream,
    primary_client_id: &ClientId,
    client_size: Size,
    iteration: u64,
) -> Result<PrimaryResizeRequestOutcome> {
    let request = terminal_resize_control_request(iteration, primary_client_id, client_size);
    if !write_async_control_body_or_disconnected(stream, &request).await? {
        return Ok(PrimaryResizeRequestOutcome::disconnected());
    }
    let Some(response) =
        read_async_control_response_frames_or_disconnected(stream, 1024 * 1024, 1).await?
    else {
        return Ok(PrimaryResizeRequestOutcome::disconnected());
    };
    let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
    if control_response_forbidden(body.as_str())? {
        return Ok(PrimaryResizeRequestOutcome::disconnected());
    }
    let _ = terminal_step_response_refresh_requirement(body.as_str())?;
    Ok(PrimaryResizeRequestOutcome { connected: true })
}

/// Writes a rendered terminal view response to the attached terminal.
pub(super) async fn render_primary_view_response_async<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    body: &str,
    cursor_blink_epoch: std::time::Instant,
) -> Result<PrimaryViewRenderOutcome> {
    let lines = terminal_step_response_lines(body)?;
    let line_style_spans = terminal_step_response_line_style_spans(body)?;
    let modes = terminal_step_response_output_modes(body)?.unwrap_or_default();
    let animation_refresh_interval_ms = modes.animation_refresh_interval_ms;
    if lines.is_empty() {
        return Ok(PrimaryViewRenderOutcome {
            connected: true,
            animation_refresh_interval_ms,
        });
    }
    let modes = control_socket_cursor_blink_elapsed(modes, cursor_blink_epoch);
    let connected =
        write_styled_output_or_disconnected_async(terminal_io, &lines, &line_style_spans, modes)
            .await?;
    Ok(PrimaryViewRenderOutcome {
        connected,
        animation_refresh_interval_ms: if connected {
            animation_refresh_interval_ms
        } else {
            0
        },
    })
}

/// Runs the write async control body or disconnected operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn write_async_control_body_or_disconnected(
    stream: &mut tokio::net::UnixStream,
    body: &str,
) -> Result<bool> {
    let result = async {
        stream.write_all(&encode_control_body(body)).await?;
        stream.flush().await?;
        Ok::<(), MezError>(())
    }
    .await;
    match result {
        Ok(()) => Ok(true),
        Err(error) if attached_terminal_output_disconnected(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Runs the read async control response frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn read_async_control_response_frames(
    stream: &mut tokio::net::UnixStream,
    max_content_length: usize,
    expected_frames: usize,
) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut buffer = vec![0; 8192];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > max_content_length {
            return Err(MezError::invalid_state("control response exceeds limit"));
        }
        if count_complete_async_control_frames(&response, max_content_length) >= expected_frames {
            return Ok(response);
        }
    }
    Err(incomplete_control_response_error(
        &response,
        max_content_length,
        expected_frames,
    ))
}

/// Reads async control response frames and treats socket closure as disconnect.
///
/// # Parameters
/// - `stream`: The attached control socket.
/// - `max_content_length`: The maximum control frame body length.
/// - `expected_frames`: The number of response frames expected by the caller.
pub(super) async fn read_async_control_response_frames_or_disconnected(
    stream: &mut tokio::net::UnixStream,
    max_content_length: usize,
    expected_frames: usize,
) -> Result<Option<Vec<u8>>> {
    match read_async_control_response_frames(stream, max_content_length, expected_frames).await {
        Ok(response) => Ok(Some(response)),
        Err(error) if control_response_socket_closed_before_complete_frame(&error) => {
            Err(MezError::invalid_state(format!(
                "attached daemon control socket disconnected while awaiting a response: {}",
                error.message()
            )))
        }
        Err(error) if attached_terminal_output_disconnected(&error) => {
            Err(MezError::invalid_state(format!(
                "attached daemon control socket disconnected while awaiting a response: {}",
                error.message()
            )))
        }
        Err(error) => Err(error),
    }
}

/// Reports whether an error means the attached control socket closed.
pub(super) fn control_response_socket_closed_before_complete_frame(error: &MezError) -> bool {
    error
        .message()
        .starts_with("control socket closed before complete response frame")
}

/// Runs the count complete async control frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn count_complete_async_control_frames(
    input: &[u8],
    max_content_length: usize,
) -> usize {
    let mut count = 0usize;
    let mut consumed = 0usize;
    while consumed < input.len() {
        let Ok((_, next)) = decode_control_frame(&input[consumed..], max_content_length) else {
            break;
        };
        if next == 0 {
            break;
        }
        count = count.saturating_add(1);
        consumed = consumed.saturating_add(next);
    }
    count
}

/// Runs the terminal view control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_view_control_request(iteration: u64, client_size: Size) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":"cli-terminal-view-{iteration}","method":"terminal/view","params":{{"client_size":{{"columns":{},"rows":{}}}}}}}"#,
        client_size.columns, client_size.rows
    )
}

/// Builds the request a pending observer uses to inspect only its own approval
/// state while waiting for the primary client.
///
/// # Parameters
/// - `iteration`: A monotonically increasing request sequence.
/// - `observer_request_id`: The observer request id returned by initialization.
pub(super) fn observer_inspect_control_request(
    iteration: u64,
    observer_request_id: &str,
) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":"cli-observer-inspect-{iteration}","method":"observer/inspect","params":{{"observer_request_id":"{}"}}}}"#,
        json_escape(observer_request_id)
    )
}

/// Runs the control socket cursor blink elapsed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn control_socket_cursor_blink_elapsed(
    mut modes: AttachedTerminalOutputModes,
    cursor_blink_epoch: std::time::Instant,
) -> AttachedTerminalOutputModes {
    modes.cursor_blink_elapsed_ms =
        u64::try_from(cursor_blink_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
    modes
}
