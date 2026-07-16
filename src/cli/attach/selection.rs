//! Session listing, attach argument resolution, and registry selection.

use super::observer::run_control_socket_attached_observer_client;
use super::primary::run_control_socket_attached_primary_client;
use super::responses::{
    ensure_control_response_success, observer_request_id_from_initialize_response,
    primary_client_id_from_initialize_response,
};
use super::*;

/// Runs the run list operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::cli) fn run_list<W: Write>(
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
pub(in crate::cli) async fn run_attach<W: Write>(
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
pub(in crate::cli) struct AttachRequest {
    /// Stores the socket selection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::cli) socket_selection: SocketSelection,
    /// Stores the requested role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::cli) requested_role: &'static str,
}

/// Runs the attach request from args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(in crate::cli) fn attach_request_from_args(
    socket_selection: &SocketSelection,
    args: &[String],
    owner_uid: u32,
) -> Result<AttachRequest> {
    let parsed = super::super::args::parse_cli_arg_group::<AttachCliArgs>("mez attach", args)?;
    attach_request(socket_selection, parsed, owner_uid)
}

/// Builds the control request implied by parsed `mez attach` arguments.
///
/// # Parameters
/// - `socket_selection`: The selected control socket or registry root.
/// - `parsed`: Parsed attach options.
/// - `owner_uid`: The effective user id that owns the session registry.
pub(super) fn attach_request(
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
pub(in crate::cli) struct AttachCliArgs {
    /// Requests observer access instead of primary access.
    #[arg(long, alias = "observe")]
    pub(in crate::cli) observer: bool,
    /// Optional registered session id or creation-order index alias to attach to.
    pub(in crate::cli) session_id: Option<String>,
}

/// Runs the socket selection for registry session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn socket_selection_for_registry_session(
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
pub(in crate::cli) fn default_attach_socket_selection(
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
pub(super) fn attachable_record<'a>(
    records: &'a [SessionRecord],
    requested_role: &str,
) -> Option<&'a SessionRecord> {
    if requested_role == "primary" {
        records.iter().find(|record| record.primary_available)
    } else {
        records.first()
    }
}
