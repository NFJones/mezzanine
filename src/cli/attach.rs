//! Cli Attach implementation.
//!
//! This module owns the cli attach boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachedTerminalOutputModes, AuxiliarySocketKind, CliEnv, CliOutputFormat, ClientId,
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT, GraphicRendition, IsTerminal, MezError, Result,
    SessionRecord, SessionRegistry, Size, SocketSelection, TerminalColor, TerminalCursorStyle,
    TerminalStyleSpan, UnixStream, Write, attached_terminal_output_disconnected,
    auxiliary_socket_path_for_control_socket, decode_control_frame, encode_control_body,
    incomplete_control_response_error, io, json_escape, read_control_response_frames,
    records_to_json, registry_root, resolve_session_record_target, selected_socket_path,
    terminal_size_from_fd_or_environment, write_control_response, write_json_or_plain,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Attach clients and interactive control-socket attachment helpers.

/// Maximum JSON-RPC event notification body accepted from the auxiliary event
/// stream.
const ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

/// Maximum bytes read from the auxiliary event stream in one socket read.
const ATTACH_EVENT_STREAM_READ_BUFFER_BYTES: usize = 8192;
/// Interval between idle terminal-size probes for attached control clients.
///
/// The attach loop should notice local terminal resizes even when the user is
/// not typing and the daemon has no new runtime events to report. Probing a
/// few times per second keeps resize-driven redraws responsive without
/// requiring a fixed-cadence render request.
const ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL: std::time::Duration =
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT;

/// Redraw requirements reported by one terminal step response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalStepRefreshRequirement {
    /// Whether the attached client should request a fresh terminal view.
    pub view_refresh_required: bool,
    /// Whether the attached client must discard its retained output frame before
    /// rendering the fresh terminal view.
    pub full_redraw_required: bool,
}

/// Outcome from rendering one explicit primary terminal view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryViewRenderOutcome {
    /// Whether the control connection and attached terminal are still usable.
    connected: bool,
    /// Milliseconds until the next animation-only view refresh.
    animation_refresh_interval_ms: u64,
}

impl PrimaryViewRenderOutcome {
    /// Builds an outcome for a disconnected control or terminal endpoint.
    const fn disconnected() -> Self {
        Self {
            connected: false,
            animation_refresh_interval_ms: 0,
        }
    }
}
/// Outcome from notifying the runtime about a primary terminal resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryResizeRequestOutcome {
    /// Whether the control connection is still usable.
    connected: bool,
}
impl PrimaryResizeRequestOutcome {
    /// Builds an outcome for a disconnected control endpoint.
    const fn disconnected() -> Self {
        Self { connected: false }
    }
}

/// Tracks the local animation refresh deadline for a control-socket attach.
#[derive(Debug, Default)]
struct AttachAnimationRefresh {
    /// Current refresh interval advertised by the last rendered view.
    interval_ms: Option<u64>,
    /// Next local deadline for an animation-only `terminal/view`.
    deadline: Option<tokio::time::Instant>,
}

impl AttachAnimationRefresh {
    /// Returns the next animation refresh deadline, when animation is active.
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    /// Updates the local refresh schedule from the latest rendered view.
    fn update_from_rendered_view(&mut self, refresh_interval_ms: u64) {
        if refresh_interval_ms == 0 {
            self.interval_ms = None;
            self.deadline = None;
            return;
        }
        self.interval_ms = Some(refresh_interval_ms);
        self.deadline = Some(
            tokio::time::Instant::now() + std::time::Duration::from_millis(refresh_interval_ms),
        );
    }
}
/// Tracks the next local wake deadline for idle terminal-size refresh probes.
#[derive(Debug)]
struct AttachTerminalSizeRefresh {
    /// Next local wake deadline for an idle terminal-size probe.
    deadline: tokio::time::Instant,
}
impl Default for AttachTerminalSizeRefresh {
    /// Builds the default size-refresh schedule for an attached client loop.
    fn default() -> Self {
        Self {
            deadline: tokio::time::Instant::now() + ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL,
        }
    }
}
impl AttachTerminalSizeRefresh {
    /// Returns the next idle terminal-size refresh deadline.
    fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    /// Reschedules the next idle terminal-size refresh from the current time.
    fn reschedule(&mut self) {
        self.deadline = tokio::time::Instant::now() + ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL;
    }
}

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
    let detach_primary_on_disconnect = request.requested_role == "primary";
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"cli-init","method":"control/initialize","params":{{"requested_role":"{}","requested_version":1,"client_name":"mez-cli","detach_primary_on_disconnect":{},"client":{{"name":"mez-cli","interactive":true,"terminal":{{"columns":{},"rows":{},"term":"{}"}}}}}}}}"#,
        request.requested_role,
        detach_primary_on_disconnect,
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
            socket_path,
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
pub(super) async fn run_control_socket_attached_primary_client_loop_async_with_runtime_events<I>(
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
    /// Render action requested by an auxiliary runtime event.
    render_action: AttachRenderAction,
}

/// Render action requested by an attached runtime event stream notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachRenderAction {
    /// No visible attached-terminal redraw is needed.
    None,
    /// Request a fresh `terminal/view` while preserving the diff-render base.
    View,
    /// Invalidate the diff-render base before requesting a fresh view.
    InvalidateAndView,
    /// The auxiliary event stream disconnected.
    Disconnect,
}

impl AttachRenderAction {
    /// Combines two actions, preserving the strongest action for an event burst.
    const fn combine(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }

    /// Returns the precedence rank for this action.
    const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::View => 1,
            Self::InvalidateAndView => 2,
            Self::Disconnect => 3,
        }
    }
}

/// Runs the read attached client input or deadline wake operation.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_attached_client_input_or_deadline<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    max_bytes: usize,
    animation_deadline: Option<tokio::time::Instant>,
    wake_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let input = async {
        let _ = terminal_io.poll_input_readiness().await?;
        terminal_io.read_input(max_bytes).await
    };
    match tokio::time::timeout_at(wake_deadline, input).await {
        Ok(Ok(bytes)) if bytes.is_empty() => Ok(AttachedClientInputPoll {
            bytes,
            eof: true,
            render_action: AttachRenderAction::None,
        }),
        Ok(Ok(bytes)) => Ok(AttachedClientInputPoll {
            bytes,
            eof: false,
            render_action: AttachRenderAction::None,
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(idle_deadline_input_poll(animation_deadline)),
    }
}
/// Builds the synthetic input poll produced by an idle local deadline wakeup.
fn idle_deadline_input_poll(
    animation_deadline: Option<tokio::time::Instant>,
) -> AttachedClientInputPoll {
    if animation_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
        animation_refresh_input_poll()
    } else {
        AttachedClientInputPoll {
            bytes: Vec::new(),
            eof: false,
            render_action: AttachRenderAction::None,
        }
    }
}
/// Reads terminal input while also accepting runtime event redraw wakeups.
///
/// # Parameters
/// - `terminal_io`: The attached terminal input/output boundary.
/// - `event_stream`: Optional auxiliary runtime event stream.
/// - `max_bytes`: Maximum terminal input bytes to read.
async fn read_attached_client_input_or_runtime_event<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    event_stream: Option<&mut AttachedRuntimeEventStream>,
    max_bytes: usize,
    animation_deadline: Option<tokio::time::Instant>,
    size_refresh_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let wake_deadline = animation_deadline
        .filter(|deadline| *deadline <= size_refresh_deadline)
        .unwrap_or(size_refresh_deadline);
    let input = read_attached_client_input_or_deadline(
        terminal_io,
        max_bytes,
        animation_deadline,
        wake_deadline,
    );
    tokio::pin!(input);
    let Some(event_stream) = event_stream else {
        return tokio::select! {
            result = &mut input => result,
        };
    };
    let mut input = tokio::select! {
        biased;
        input = &mut input => input,
        render_action = read_runtime_event_stream_action(event_stream) => {
            return Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action: render_action?,
            });
        }
    }?;
    if !input.eof && !input.bytes.is_empty() {
        input.render_action = input
            .render_action
            .combine(event_stream.try_read_ready_render_action()?);
    }
    Ok(input)
}

/// Builds the synthetic input poll produced by a local animation refresh tick.
fn animation_refresh_input_poll() -> AttachedClientInputPoll {
    AttachedClientInputPoll {
        bytes: Vec::new(),
        eof: false,
        render_action: AttachRenderAction::View,
    }
}

/// Reads auxiliary runtime event notifications and returns the coalesced action.
async fn read_runtime_event_stream_action(
    stream: &mut AttachedRuntimeEventStream,
) -> Result<AttachRenderAction> {
    stream.read_render_action().await
}

/// Stateful auxiliary runtime event stream decoder.
pub(super) struct AttachedRuntimeEventStream {
    /// Auxiliary event stream socket.
    stream: tokio::net::UnixStream,
    /// Buffered bytes that have not yet formed a complete control frame.
    pending: Vec<u8>,
}

impl AttachedRuntimeEventStream {
    /// Creates a stateful decoder for one auxiliary event stream.
    pub(super) fn new(stream: tokio::net::UnixStream) -> Self {
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    /// Reads one event burst and returns the strongest render action it implies.
    pub(super) async fn read_render_action(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        if !self.pending_contains_complete_frame() {
            match self.read_event_stream_chunk().await? {
                RuntimeEventStreamRead::Read => {}
                RuntimeEventStreamRead::Disconnected => return Ok(AttachRenderAction::Disconnect),
                RuntimeEventStreamRead::Pending => return Ok(AttachRenderAction::None),
            }
        }
        action = action.combine(self.drain_complete_event_frames()?);
        loop {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {
                    action = action.combine(self.drain_complete_event_frames()?);
                }
                RuntimeEventStreamRead::Pending => return Ok(action),
                RuntimeEventStreamRead::Disconnected => {
                    return Ok(action.combine(AttachRenderAction::Disconnect));
                }
            }
        }
    }

    /// Drains any already-ready redraw events without waiting for new bytes.
    ///
    /// The foreground input loop uses this after local input wins the readiness
    /// race so a simultaneous runtime redraw wakeup can be satisfied by the same
    /// post-input render instead of lingering for a later redundant view request.
    fn try_read_ready_render_action(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        if !self.pending_contains_complete_frame() {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {}
                RuntimeEventStreamRead::Pending | RuntimeEventStreamRead::Disconnected => {
                    return Ok(AttachRenderAction::None);
                }
            }
        }
        action = action.combine(self.drain_complete_event_frames()?);
        loop {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {
                    action = action.combine(self.drain_complete_event_frames()?);
                }
                RuntimeEventStreamRead::Pending | RuntimeEventStreamRead::Disconnected => {
                    return Ok(action);
                }
            }
        }
    }

    /// Reports whether the pending byte buffer begins with a complete frame.
    fn pending_contains_complete_frame(&self) -> bool {
        decode_control_frame(
            self.pending.as_slice(),
            ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH,
        )
        .is_ok()
    }

    /// Reads one awaited chunk from the event stream into the pending buffer.
    async fn read_event_stream_chunk(&mut self) -> Result<RuntimeEventStreamRead> {
        let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
        match self.stream.read(&mut buffer).await {
            Ok(0) => Ok(RuntimeEventStreamRead::Disconnected),
            Ok(read) => {
                self.push_pending_event_bytes(&buffer[..read])?;
                Ok(RuntimeEventStreamRead::Read)
            }
            Err(error) if runtime_event_stream_disconnected(error.kind()) => {
                Ok(RuntimeEventStreamRead::Disconnected)
            }
            Err(error) => Err(MezError::from(error)),
        }
    }

    /// Reads one immediately available chunk from the event stream.
    fn try_read_event_stream_chunk(&mut self) -> Result<RuntimeEventStreamRead> {
        let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
        match self.stream.try_read(&mut buffer) {
            Ok(0) => Ok(RuntimeEventStreamRead::Disconnected),
            Ok(read) => {
                self.push_pending_event_bytes(&buffer[..read])?;
                Ok(RuntimeEventStreamRead::Read)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(RuntimeEventStreamRead::Pending)
            }
            Err(error) if runtime_event_stream_disconnected(error.kind()) => {
                Ok(RuntimeEventStreamRead::Disconnected)
            }
            Err(error) => Err(MezError::from(error)),
        }
    }

    /// Appends bytes to the pending buffer while enforcing a bounded frame size.
    fn push_pending_event_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024 {
            return Err(MezError::invalid_state(
                "runtime event stream frame exceeds limit",
            ));
        }
        Ok(())
    }

    /// Drains all complete frames from the pending buffer into one render action.
    fn drain_complete_event_frames(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        loop {
            let Ok((body, consumed)) = decode_control_frame(
                self.pending.as_slice(),
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH,
            ) else {
                return Ok(action);
            };
            if consumed == 0 {
                return Ok(action);
            }
            action = action.combine(attach_render_action_for_event_body(body.as_str()));
            self.pending.drain(..consumed);
        }
    }
}

/// Result of one auxiliary event stream socket read attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeEventStreamRead {
    /// Bytes were read and appended to the pending buffer.
    Read,
    /// No bytes are currently available without awaiting the socket.
    Pending,
    /// The auxiliary event stream disconnected.
    Disconnected,
}

/// Reports whether an event stream I/O error should be treated as disconnect.
fn runtime_event_stream_disconnected(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

/// Classifies one event notification body into an attach render action.
fn attach_render_action_for_event_body(body: &str) -> AttachRenderAction {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return AttachRenderAction::None;
    };
    let Some(event_type) = event_type_from_notification(&value) else {
        return AttachRenderAction::None;
    };
    attach_render_action_for_event_type(event_type)
}

/// Extracts an event type from a JSON-RPC event notification.
fn event_type_from_notification(value: &serde_json::Value) -> Option<&str> {
    if let Some(event_type) = value
        .get("params")
        .and_then(|params| params.get("event_type"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(event_type);
    }
    value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .and_then(|method| method.strip_prefix("event/"))
}

/// Maps a runtime event type onto the attached client's render needs.
fn attach_render_action_for_event_type(event_type: &str) -> AttachRenderAction {
    match event_type {
        "diagnostic" | "snapshot_changed" => AttachRenderAction::None,
        "client_attached" | "client_detached" | "config_changed" | "observer_decided"
        | "window_changed" => AttachRenderAction::InvalidateAndView,
        "agent_status" | "approval_changed" | "hook_failed" | "mcp_server_changed" | "message"
        | "observer_requested" | "pane_changed" => AttachRenderAction::View,
        _ => AttachRenderAction::View,
    }
}

/// Connects to the auxiliary event socket for event-driven attach redraws.
fn optional_control_socket_event_stream(
    control_socket_path: &std::path::Path,
) -> Result<Option<tokio::net::UnixStream>> {
    let event_socket_path =
        auxiliary_socket_path_for_control_socket(control_socket_path, AuxiliarySocketKind::Event)?;
    let stream = match UnixStream::connect(event_socket_path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(MezError::from(error)),
    };
    stream.set_nonblocking(true)?;
    Ok(Some(tokio::net::UnixStream::from_std(stream)?))
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
/// Builds a mutation-only terminal-step request for a detected terminal resize.
fn terminal_resize_control_request(
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

/// Requests an explicit terminal view and writes the rendered frame locally.
async fn request_and_render_primary_view_async<I: AsyncAttachedTerminalIo>(
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
async fn request_primary_resize_async(
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
async fn render_primary_view_response_async<I: AsyncAttachedTerminalIo>(
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

/// Returns the redraw requirements reported by a terminal step response.
pub(super) fn terminal_step_response_refresh_requirement(
    body: &str,
) -> Result<TerminalStepRefreshRequirement> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("terminal step response is not valid JSON"))?;
    if let Some(error) = parsed.get("error") {
        return Err(MezError::invalid_state(format!(
            "terminal step failed: {}",
            json_escape(&error.to_string())
        )));
    }
    let application = parsed
        .get("result")
        .and_then(|result| result.get("application"));
    let view_refresh_required = application
        .and_then(|application| application.get("view_refresh_required"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let full_redraw_required = application
        .and_then(|application| application.get("full_redraw_required"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(TerminalStepRefreshRequirement {
        view_refresh_required: view_refresh_required || full_redraw_required,
        full_redraw_required,
    })
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
    let host_mouse_reporting = view
        .get("output_modes")
        .and_then(|modes| modes.get("host_mouse_reporting"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let animation_refresh_interval_ms = view
        .get("output_modes")
        .and_then(|modes| modes.get("animation_refresh_interval_ms"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(Some(AttachedTerminalOutputModes {
        application_keypad,
        bracketed_paste,
        host_mouse_reporting,
        animation_refresh_interval_ms,
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
