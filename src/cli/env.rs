//! Cli Env implementation.
//!
//! This module owns the cli env boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::os::fd::RawFd;

use super::{
    CliOutputFormat, DEFAULT_SOCKET_NAME, MEZ_ENV_FIELD_SEPARATOR, MezError, OsString, Parser,
    PathBuf, Result, RuntimeEnv, default_socket_directory, socket_path_for_name,
};
use crate::terminal::read_attached_terminal_size;

// Socket selection, invocation parsing, and CLI option helpers.

/// Carries Socket Selection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SocketSelection {
    /// Represents the Default case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Default(PathBuf),
    /// Represents the Explicit case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Explicit(PathBuf),
    /// Represents the Named case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Named(PathBuf),
    /// Represents the In Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    InPane(PathBuf),
}

/// Carries Cli Invocation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CliInvocation {
    /// Stores the command value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) command: String,
    /// Stores the args value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) args: Vec<String>,
    /// Stores the socket selection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) socket_selection: SocketSelection,
    /// Stores the output format value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_format: CliOutputFormat,
}

#[derive(Debug, Parser)]
#[command(
    name = "mez",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    allow_external_subcommands = true
)]
/// Carries Cli Argv state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct CliArgv {
    /// Stores the socket value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(short = 'S', value_name = "PATH")]
    pub(super) socket: Option<PathBuf>,
    /// Stores the socket name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(short = 'L', value_name = "NAME")]
    pub(super) socket_name: Option<String>,
    /// Stores the json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(long)]
    pub(super) json: bool,
    /// Stores the rest value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(super) rest: Vec<String>,
}

impl CliInvocation {
    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse(
        args: &[String],
        runtime: &RuntimeEnv,
        mez: Option<&OsString>,
    ) -> Result<Self> {
        let mut socket_selection = None;
        let parsed = CliArgv::try_parse_from(args).map_err(|error| {
            MezError::invalid_args(format!("failed to parse command line: {error}"))
        })?;

        if let Some(socket_path) = parsed.socket {
            if !socket_path.is_absolute() {
                return Err(MezError::invalid_args(
                    "-S requires an absolute socket path",
                ));
            }
            socket_selection = Some(set_socket_selection(
                socket_selection,
                SocketSelection::Explicit(socket_path),
            )?);
        }
        if let Some(name) = parsed.socket_name {
            let directory = default_socket_directory(runtime)?;
            let socket_path = socket_path_for_name(&directory.path, &name)?;
            socket_selection = Some(set_socket_selection(
                socket_selection,
                SocketSelection::Named(socket_path),
            )?);
        }

        let (json, rest) = extract_json_output_flag(parsed.json, parsed.rest);
        let command = rest.first().cloned().unwrap_or_default();
        let command_args = if rest.is_empty() {
            Vec::new()
        } else {
            rest[1..].to_vec()
        };
        let socket_selection = match socket_selection {
            Some(selection) => selection,
            None => match socket_selection_from_mez(mez)? {
                Some(selection) => selection,
                None => default_socket_selection(runtime)?,
            },
        };

        Ok(Self {
            command,
            args: command_args,
            socket_selection,
            output_format: CliOutputFormat::from_json_flag(json),
        })
    }
}

/// Runs the extract json output flag operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn extract_json_output_flag(initial: bool, rest: Vec<String>) -> (bool, Vec<String>) {
    let mut json = initial;
    let mut filtered = Vec::with_capacity(rest.len());
    let mut index = 0usize;
    while index < rest.len() {
        let arg = &rest[index];
        if arg == "--json" {
            json = true;
            index += 1;
            continue;
        }
        filtered.push(arg.clone());
        if cli_option_takes_value(arg)
            && let Some(value) = rest.get(index + 1)
        {
            filtered.push(value.clone());
            index += 2;
            continue;
        }
        index += 1;
    }
    (json, filtered)
}

/// Runs the cli option takes value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "-S" | "-L"
            | "--client-id"
            | "--scope"
            | "--file"
            | "--message-socket"
            | "--event-socket"
            | "--max-control-connections"
            | "--max-message-connections"
            | "--max-event-connections"
            | "--max-event-batches-per-connection"
            | "--session-id"
            | "--restart-command"
            | "--name"
            | "-n"
            | "--api-key-file"
            | "--profile"
            | "--credential-store"
            | "--command"
            | "--url"
            | "--arg"
            | "--content"
            | "--priority"
    )
}

/// Runs the default socket selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn default_socket_selection(runtime: &RuntimeEnv) -> Result<SocketSelection> {
    let directory = default_socket_directory(runtime)?;
    Ok(SocketSelection::Default(socket_path_for_name(
        &directory.path,
        DEFAULT_SOCKET_NAME,
    )?))
}

/// Runs the socket selection from mez operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn socket_selection_from_mez(
    value: Option<&OsString>,
) -> Result<Option<SocketSelection>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    if value.trim().is_empty() {
        return Ok(None);
    }
    let socket = value
        .split(MEZ_ENV_FIELD_SEPARATOR)
        .next()
        .unwrap_or_default()
        .trim();
    if socket.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(socket);
    if !path.is_absolute() {
        return Err(MezError::invalid_args(
            "MEZ contains a relative socket path",
        ));
    }
    Ok(Some(SocketSelection::InPane(path)))
}

/// Runs the set socket selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_socket_selection(
    existing: Option<SocketSelection>,
    next: SocketSelection,
) -> Result<SocketSelection> {
    if existing.is_some() {
        Err(MezError::invalid_args(
            "only one control socket selector may be provided",
        ))
    } else {
        Ok(next)
    }
}
/// Runs the terminal size from environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_size_from_environment() -> (u16, u16) {
    let columns = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(80);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(24);
    (columns, rows)
}

/// Returns the live terminal size from a TTY file descriptor when available,
/// falling back to shell-provided `COLUMNS`/`LINES` and finally 80x24.
///
/// Default `mez` launch starts a background daemon before attaching the primary
/// client, so the daemon cannot query the parent terminal itself. The attach
/// side must prefer the real TTY size for `control/initialize` so the initial
/// pane is resized before its first user-facing render.
pub(super) fn terminal_size_from_fd_or_environment(fd: Option<RawFd>) -> (u16, u16) {
    if let Some(fd) = fd
        && let Ok(Some(size)) = read_attached_terminal_size(fd)
    {
        return (size.columns, size.rows);
    }
    terminal_size_from_environment()
}

/// Runs the selected socket path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn selected_socket_path(socket_selection: &SocketSelection) -> &PathBuf {
    match socket_selection {
        SocketSelection::Default(path)
        | SocketSelection::Explicit(path)
        | SocketSelection::Named(path)
        | SocketSelection::InPane(path) => path,
    }
}

/// Runs the cli idempotency key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn cli_idempotency_key(operation: &str) -> String {
    format!("cli-{}-{operation}", std::process::id())
}
/// Runs the registry root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn registry_root(socket_selection: &SocketSelection) -> Result<PathBuf> {
    let socket_path = match socket_selection {
        SocketSelection::Default(path)
        | SocketSelection::Explicit(path)
        | SocketSelection::Named(path)
        | SocketSelection::InPane(path) => path,
    };
    socket_path
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| MezError::invalid_args("control socket path must have a parent directory"))
}
