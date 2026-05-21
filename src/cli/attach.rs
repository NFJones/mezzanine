//! Cli Attach implementation.
//!
//! This module owns the cli attach boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachedTerminalOutputModes, CliEnv, CliOutputFormat, ClientId,
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT, GraphicRendition, IsTerminal, MezError, Result,
    SessionRecord, SessionRegistry, Size, SocketSelection, TerminalColor, TerminalCursorStyle,
    TerminalStyleSpan, UnixStream, Write, attached_terminal_output_disconnected,
    decode_control_frame, encode_control_body, incomplete_control_response_error, io, json_escape,
    read_control_response_frames, records_to_json, registry_root, resolve_session_record_target,
    selected_socket_path, terminal_size_from_fd_or_environment, write_control_response,
    write_json_or_plain,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Attach clients and interactive control-socket attachment helpers.

/// Runs the run list operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_list<W: Write>(
    socket_selection: &SocketSelection,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let registry = SessionRegistry::new(registry_root(socket_selection)?, env.runtime.uid);
    let _ = registry.prune_stale()?;
    let output = records_to_json(&registry.list()?);
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Runs the run attach operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_attach<W: Write>(
    socket_selection: &SocketSelection,
    parsed: AttachCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let request = attach_request(socket_selection, parsed, env.runtime.uid)?;
    if !interactive {
        let message = if request.requested_role == "observer" {
            "attaching as an observer client requires an interactive terminal"
        } else {
            "attaching as the primary client requires an interactive terminal"
        };
        return Err(MezError::forbidden(message));
    }
    let socket_path = selected_socket_path(&request.socket_selection);
    let mut stream = UnixStream::connect(socket_path)?;
    let terminal_size_fd = io::stdout().is_terminal().then(|| io::stdout().as_raw_fd());
    let (columns, rows) = terminal_size_from_fd_or_environment(terminal_size_fd);
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"cli-init","method":"control/initialize","params":{{"requested_role":"{}","requested_version":1,"client_name":"mez-cli","client":{{"name":"mez-cli","interactive":true,"terminal":{{"columns":{},"rows":{},"term":"{}"}}}}}}}}"#,
        request.requested_role,
        columns,
        rows,
        json_escape(&std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string()))
    );
    if request.requested_role == "observer" {
        stream.write_all(&encode_control_body(&initialize))?;
        stream.flush()?;
        let response = read_control_response_frames(&mut stream, 1024 * 1024, 1)?;
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            ensure_control_response_success(body.as_str())?;
            let observer_request_id = observer_request_id_from_initialize_response(body.as_str())?;
            return run_control_socket_attached_observer_client(
                &mut stream,
                observer_request_id,
                Size::new(columns, rows)?,
            )
            .await;
        }
        write_control_response(stdout, output_format, &body)?;
        return Ok(());
    }
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        stream.write_all(&encode_control_body(&initialize))?;
        stream.flush()?;
        let response = read_control_response_frames(&mut stream, 1024 * 1024, 1)?;
        let (body, _) = decode_control_frame(&response, 1024 * 1024)?;
        let primary_client_id = primary_client_id_from_initialize_response(body.as_str())?;
        return run_control_socket_attached_primary_client(
            &mut stream,
            primary_client_id,
            Size::new(columns, rows)?,
        )
        .await;
    }
    let get = r#"{"jsonrpc":"2.0","id":"cli","method":"session/get","params":{}}"#;
    stream.write_all(&encode_control_body(&initialize))?;
    stream.write_all(&encode_control_body(get))?;
    stream.flush()?;
    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2)?;
    let (first_body, first_consumed) = decode_control_frame(&response, 1024 * 1024)?;
    if first_body.contains(r#""error""#) || first_consumed >= response.len() {
        write_control_response(stdout, output_format, &first_body)?;
        return Ok(());
    }
    let (second_body, _) = decode_control_frame(&response[first_consumed..], 1024 * 1024)?;
    write_control_response(stdout, output_format, &second_body)?;
    Ok(())
}

/// Carries Attach Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttachRequest {
    /// Stores the socket selection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) socket_selection: SocketSelection,
    /// Stores the requested role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) requested_role: &'static str,
}

/// Runs the attach request from args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn attach_request_from_args(
    socket_selection: &SocketSelection,
    args: &[String],
    owner_uid: u32,
) -> Result<AttachRequest> {
    let parsed = super::parse_cli_arg_group::<AttachCliArgs>("mez attach", args)?;
    attach_request(socket_selection, parsed, owner_uid)
}

/// Builds the control request implied by parsed `mez attach` arguments.
///
/// # Parameters
/// - `socket_selection`: The selected control socket or registry root.
/// - `parsed`: Parsed attach options.
/// - `owner_uid`: The effective user id that owns the session registry.
fn attach_request(
    socket_selection: &SocketSelection,
    parsed: AttachCliArgs,
    owner_uid: u32,
) -> Result<AttachRequest> {
    let requested_role = if parsed.observer {
        "observer"
    } else {
        "primary"
    };

    let socket_selection = if let Some(session_id) = parsed.session_id {
        socket_selection_for_registry_session(socket_selection, owner_uid, &session_id)?
    } else if matches!(socket_selection, SocketSelection::Default(_)) {
        default_attach_socket_selection(socket_selection, owner_uid, requested_role)?
            .unwrap_or_else(|| socket_selection.clone())
    } else {
        socket_selection.clone()
    };

    Ok(AttachRequest {
        socket_selection,
        requested_role,
    })
}

/// Typed process CLI arguments for `mez attach`.
#[derive(Debug, Clone, Args)]
pub(super) struct AttachCliArgs {
    /// Requests observer access instead of primary access.
    #[arg(long, alias = "observe")]
    pub(super) observer: bool,
    /// Optional registered session id or creation-order index alias to attach to.
    pub(super) session_id: Option<String>,
}

/// Runs the socket selection for registry session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn socket_selection_for_registry_session(
    socket_selection: &SocketSelection,
    owner_uid: u32,
    session_id: &str,
) -> Result<SocketSelection> {
    let registry = SessionRegistry::new(registry_root(socket_selection)?, owner_uid);
    let _ = registry.prune_stale()?;
    let records = registry.list()?;
    let record = resolve_session_record_target(&records, session_id).ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            format!("session `{session_id}` was not found in the session registry"),
        )
    })?;
    Ok(SocketSelection::Explicit(record.socket_path.clone()))
}

/// Runs the default attach socket selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn default_attach_socket_selection(
    socket_selection: &SocketSelection,
    owner_uid: u32,
    requested_role: &str,
) -> Result<Option<SocketSelection>> {
    let registry = SessionRegistry::new(registry_root(socket_selection)?, owner_uid);
    let _ = registry.prune_stale()?;
    let records = registry.list()?;
    if records.is_empty() {
        return Ok(None);
    }
    attachable_record(&records, requested_role)
        .map(|record| Some(SocketSelection::Explicit(record.socket_path.clone())))
        .ok_or_else(|| {
            MezError::conflict(
                "no registered session currently accepts primary attachment; use --observer or start a new session",
            )
        })
}

/// Runs the attachable record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn attachable_record<'a>(
    records: &'a [SessionRecord],
    requested_role: &str,
) -> Option<&'a SessionRecord> {
    if requested_role == "primary" {
        records.iter().find(|record| record.primary_available)
    } else {
        records.first()
    }
}

/// Runs the run control socket attached primary client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_control_socket_attached_primary_client(
    stream: &mut UnixStream,
    primary_client_id: ClientId,
    client_size: Size,
) -> Result<()> {
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    let control_stream = stream.try_clone()?;
    control_stream.set_nonblocking(true)?;
    let mut control_stream = tokio::net::UnixStream::from_std(control_stream)?;
    let mut terminal_guard =
        AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
    let run_result = run_control_socket_attached_primary_client_loop_async(
        &mut control_stream,
        terminal_guard.io_mut(),
        primary_client_id,
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

/// Runs the primary control-socket attach terminal loop over async terminal I/O.
///
/// The control socket and terminal endpoint both use Tokio I/O in this path.
/// Runtime state is still mutated by the daemon-side control handler; this loop
/// only coordinates foreground terminal bytes, rendered frames, and framed
/// control requests.
pub(super) async fn run_control_socket_attached_primary_client_loop_async<I>(
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

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
            render_requested = true;
        }

        let input = read_attached_client_input_or_timeout(terminal_io, 4096).await?;
        if input.eof {
            break Ok(());
        }
        if input.bytes.is_empty() && !render_requested {
            if control_socket_disconnected_without_pending_response(stream)? {
                break Ok(());
            }
            continue;
        }
        let request = if input.bytes.is_empty() {
            terminal_view_control_request(iteration, client_size)
        } else {
            terminal_step_control_request(
                iteration,
                &primary_client_id,
                client_size,
                input.bytes.as_slice(),
                true,
            )
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
        if control_response_forbidden(body.as_str())? {
            break Ok(());
        }
        render_requested = false;
        let lines = terminal_step_response_lines(body.as_str())?;
        let line_style_spans = terminal_step_response_line_style_spans(body.as_str())?;
        if !lines.is_empty() {
            let modes = control_socket_cursor_blink_elapsed(
                terminal_step_response_output_modes(body.as_str())?.unwrap_or_default(),
                cursor_blink_epoch,
            );
            if !write_styled_output_or_disconnected_async(
                terminal_io,
                &lines,
                &line_style_spans,
                modes,
            )
            .await?
            {
                break Ok(());
            }
        }
        iteration = iteration.saturating_add(1);
    }
}

/// Runs the run control socket attached observer client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_control_socket_attached_observer_client(
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
pub(super) async fn run_control_socket_attached_observer_client_loop_async<I>(
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

    loop {
        if refresh_attached_client_size_async(terminal_io, &mut client_size).await? {
            terminal_io.invalidate_output_frame().await?;
        }
        let input = read_attached_client_input_or_timeout(terminal_io, 4096).await?;
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

/// Carries Attached Client Input Poll state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachedClientInputPoll {
    /// Stores the bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    bytes: Vec<u8>,
    /// Stores the eof value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    eof: bool,
}

/// Runs the read attached client input or timeout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_attached_client_input_or_timeout<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    max_bytes: usize,
) -> Result<AttachedClientInputPoll> {
    match tokio::time::timeout(
        DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
        terminal_io.read_input(max_bytes),
    )
    .await
    {
        Ok(Ok(bytes)) if bytes.is_empty() => Ok(AttachedClientInputPoll { bytes, eof: true }),
        Ok(Ok(bytes)) => Ok(AttachedClientInputPoll { bytes, eof: false }),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(AttachedClientInputPoll {
            bytes: Vec::new(),
            eof: false,
        }),
    }
}
/// Checks whether the control socket has closed while no response is pending.
///
/// Idle control-socket attach loops avoid sending render requests after input
/// timeouts, but they still need to notice daemon teardown promptly. The socket
/// should not deliver unsolicited bytes in this state, so readable EOF means the
/// attached client can exit cleanly without reintroducing periodic renders.
fn control_socket_disconnected_without_pending_response(
    stream: &tokio::net::UnixStream,
) -> Result<bool> {
    let mut byte = [0u8; 1];
    match stream.try_read(&mut byte) {
        Ok(0) => Ok(true),
        Ok(_) => Err(MezError::invalid_state(
            "control socket delivered an unexpected response while idle",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => {
            let error = MezError::from(error);
            if attached_terminal_output_disconnected(&error) {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}

/// Runs the terminal step control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_step_control_request(
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

/// Runs the refresh attached client size async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn refresh_attached_client_size_async<I: AsyncAttachedTerminalIo>(
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
async fn write_styled_output_or_disconnected_async<I: AsyncAttachedTerminalIo>(
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

/// Runs the write async control body or disconnected operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn write_async_control_body_or_disconnected(
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
async fn read_async_control_response_frames(
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
async fn read_async_control_response_frames_or_disconnected(
    stream: &mut tokio::net::UnixStream,
    max_content_length: usize,
    expected_frames: usize,
) -> Result<Option<Vec<u8>>> {
    match read_async_control_response_frames(stream, max_content_length, expected_frames).await {
        Ok(response) => Ok(Some(response)),
        Err(error) if control_response_socket_closed_before_complete_frame(&error) => Ok(None),
        Err(error) if attached_terminal_output_disconnected(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Reports whether an error means the attached control socket closed.
fn control_response_socket_closed_before_complete_frame(error: &MezError) -> bool {
    error
        .message()
        .starts_with("control socket closed before complete response frame")
}

/// Runs the count complete async control frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn count_complete_async_control_frames(input: &[u8], max_content_length: usize) -> usize {
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
fn control_socket_cursor_blink_elapsed(
    mut modes: AttachedTerminalOutputModes,
    cursor_blink_epoch: std::time::Instant,
) -> AttachedTerminalOutputModes {
    modes.cursor_blink_elapsed_ms =
        u64::try_from(cursor_blink_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
    modes
}

/// Runs the ensure control response success operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_control_response_success(body: &str) -> Result<()> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("control response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "control request failed: {}",
            json_escape(&error.to_string())
        )));
    }
    Ok(())
}

/// Runs the control response forbidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn control_response_forbidden(body: &str) -> Result<bool> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("control response is not valid JSON"))?;
    Ok(parsed
        .get("error")
        .and_then(|error| error.get("data"))
        .and_then(|data| data.get("mezzanine_code"))
        .and_then(serde_json::Value::as_str)
        == Some("forbidden"))
}

/// Runs the primary client id from initialize response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn primary_client_id_from_initialize_response(body: &str) -> Result<ClientId> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("control initialize response is not valid JSON"))?;
    let client_id = parsed
        .get("result")
        .and_then(|result| result.get("session"))
        .and_then(|session| session.get("primary_client_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MezError::invalid_state("control initialize did not return a primary client id")
        })?;
    ClientId::parse('c', client_id.to_string())
        .ok_or_else(|| MezError::invalid_state("control initialize returned an invalid client id"))
}

/// Extracts the pending observer request id from a successful initialize
/// response.
///
/// # Parameters
/// - `body`: The JSON-RPC response body returned by `control/initialize`.
pub(super) fn observer_request_id_from_initialize_response(body: &str) -> Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("control initialize response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "control initialize failed: {}",
            json_escape(&error.to_string())
        )));
    }
    parsed
        .get("result")
        .and_then(|result| result.get("observer_request"))
        .and_then(observer_request_id_from_value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            MezError::invalid_state("control initialize did not return an observer request id")
        })
}

/// Stores the observer attach state reported by `observer/inspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObserverAttachState {
    /// The request is still waiting for a primary-client decision.
    Pending,
    /// The request has been approved and may now read the live terminal view.
    Approved,
    /// The request was rejected by the primary client.
    Rejected,
    /// A previously approved observer has been revoked.
    Revoked,
}

/// Extracts the observer request state from an `observer/inspect` response.
///
/// # Parameters
/// - `body`: The JSON-RPC response body returned by `observer/inspect`.
pub(super) fn observer_attach_state_from_inspect_response(
    body: &str,
) -> Result<ObserverAttachState> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("observer inspect response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "observer inspect failed: {}",
            json_escape(&error.to_string())
        )));
    }
    let state = parsed
        .get("result")
        .and_then(|result| result.get("observer"))
        .and_then(|observer| observer.get("state"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("observer inspect did not return a state"))?;
    match state {
        "pending" => Ok(ObserverAttachState::Pending),
        "approved" => Ok(ObserverAttachState::Approved),
        "rejected" => Ok(ObserverAttachState::Rejected),
        "revoked" => Ok(ObserverAttachState::Revoked),
        _ => Err(MezError::invalid_state(format!(
            "observer inspect returned unsupported state `{state}`"
        ))),
    }
}

/// Reads either accepted observer-request id spelling from an observer JSON
/// object.
///
/// # Parameters
/// - `value`: The observer request summary or observer state JSON object.
fn observer_request_id_from_value(value: &serde_json::Value) -> Option<&str> {
    value
        .get("observer_request_id")
        .or_else(|| value.get("id"))
        .and_then(serde_json::Value::as_str)
}

/// Runs the terminal step response lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_step_response_lines(body: &str) -> Result<Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("terminal step response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "terminal step failed: {}",
            json_escape(&error.to_string())
        )));
    }
    let Some(lines) = parsed
        .get("result")
        .and_then(|result| result.get("view"))
        .and_then(|view| view.get("lines"))
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };
    lines
        .iter()
        .map(|line| {
            line.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| MezError::invalid_state("terminal step view line is not a string"))
        })
        .collect()
}

/// Runs the terminal step response line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_step_response_line_style_spans(
    body: &str,
) -> Result<Vec<Vec<TerminalStyleSpan>>> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("terminal step response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "terminal step failed: {}",
            json_escape(&error.to_string())
        )));
    }
    let Some(line_spans) = parsed
        .get("result")
        .and_then(|result| result.get("view"))
        .and_then(|view| view.get("line_style_spans"))
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };
    line_spans
        .iter()
        .map(parse_terminal_style_span_row)
        .collect()
}

/// Runs the parse terminal style span row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_terminal_style_span_row(value: &serde_json::Value) -> Result<Vec<TerminalStyleSpan>> {
    let spans = value
        .as_array()
        .ok_or_else(|| MezError::invalid_state("terminal step style span row is not an array"))?;
    spans.iter().map(parse_terminal_style_span).collect()
}

/// Runs the parse terminal style span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_terminal_style_span(value: &serde_json::Value) -> Result<TerminalStyleSpan> {
    let start = value
        .get("start")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step style span start is missing"))?;
    let length = value
        .get("length")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step style span length is missing"))?;
    let rendition = value
        .get("rendition")
        .ok_or_else(|| MezError::invalid_state("terminal step style span rendition is missing"))
        .and_then(parse_terminal_graphic_rendition)?;
    Ok(TerminalStyleSpan {
        start: usize::try_from(start)
            .map_err(|_| MezError::invalid_state("terminal step style span start is too large"))?,
        length: usize::try_from(length)
            .map_err(|_| MezError::invalid_state("terminal step style span length is too large"))?,
        rendition,
    })
}

/// Runs the parse terminal graphic rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_terminal_graphic_rendition(value: &serde_json::Value) -> Result<GraphicRendition> {
    Ok(GraphicRendition {
        bold: bool_field(value, "bold"),
        dim: bool_field(value, "dim"),
        italic: bool_field(value, "italic"),
        underline: bool_field(value, "underline"),
        double_underline: bool_field(value, "double_underline"),
        strikethrough: bool_field(value, "strikethrough"),
        inverse: bool_field(value, "inverse"),
        hidden: bool_field(value, "hidden"),
        foreground: parse_terminal_color_field(value, "foreground")?,
        background: parse_terminal_color_field(value, "background")?,
    })
}

/// Runs the bool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn bool_field(value: &serde_json::Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Runs the parse terminal color field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_terminal_color_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<TerminalColor>> {
    let Some(color) = value.get(field) else {
        return Ok(None);
    };
    if color.is_null() {
        return Ok(None);
    }
    parse_terminal_color_value(color).map(Some)
}

/// Runs the parse terminal color value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_terminal_color_value(color: &serde_json::Value) -> Result<TerminalColor> {
    let kind = color
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("terminal step style color kind is missing"))?;
    match kind {
        "indexed" => {
            let index = color
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    MezError::invalid_state("terminal step indexed style color is missing")
                })?;
            Ok(TerminalColor::Indexed(u8::try_from(index).map_err(
                |_| MezError::invalid_state("terminal step indexed style color is out of range"),
            )?))
        }
        "rgb" => Ok(TerminalColor::Rgb(
            parse_u8_color_component(color, "red")?,
            parse_u8_color_component(color, "green")?,
            parse_u8_color_component(color, "blue")?,
        )),
        _ => Err(MezError::invalid_state(
            "terminal step style color kind is invalid",
        )),
    }
}

/// Runs the parse u8 color component operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_u8_color_component(value: &serde_json::Value, field: &str) -> Result<u8> {
    let component = value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step RGB style color is missing"))?;
    u8::try_from(component)
        .map_err(|_| MezError::invalid_state("terminal step RGB style color is out of range"))
}

/// Runs the terminal step response output modes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_step_response_output_modes(
    body: &str,
) -> Result<Option<AttachedTerminalOutputModes>> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("terminal step response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "terminal step failed: {}",
            json_escape(&error.to_string())
        )));
    }
    let Some(view) = parsed.get("result").and_then(|result| result.get("view")) else {
        return Ok(None);
    };
    let Some(cursor) = view.get("cursor") else {
        return Ok(None);
    };
    let cursor_row = cursor
        .get("row")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step cursor row is missing"))?;
    let cursor_column = cursor
        .get("column")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step cursor column is missing"))?;
    let cursor_visible = cursor
        .get("visible")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| MezError::invalid_state("terminal step cursor visibility is missing"))?;
    let cursor_style = match cursor.get("style").and_then(serde_json::Value::as_str) {
        Some("block") | None => TerminalCursorStyle::Block,
        Some("underline") => TerminalCursorStyle::Underline,
        Some("bar") => TerminalCursorStyle::Bar,
        Some(_) => {
            return Err(MezError::invalid_state(
                "terminal step cursor style is invalid",
            ));
        }
    };
    let cursor_blink = cursor
        .get("blink")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let cursor_blink_interval_ms = cursor
        .get("blink_interval_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(500);
    let application_keypad = view
        .get("output_modes")
        .and_then(|modes| modes.get("application_keypad"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let bracketed_paste = view
        .get("output_modes")
        .and_then(|modes| modes.get("bracketed_paste"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(Some(AttachedTerminalOutputModes {
        application_keypad,
        bracketed_paste,
        cursor_style,
        cursor_blink,
        cursor_blink_interval_ms,
        cursor_row: usize::try_from(cursor_row)
            .map_err(|_| MezError::invalid_state("terminal step cursor row is too large"))?,
        cursor_column: usize::try_from(cursor_column)
            .map_err(|_| MezError::invalid_state("terminal step cursor column is too large"))?,
        cursor_visible,
        ..AttachedTerminalOutputModes::default()
    }))
}
