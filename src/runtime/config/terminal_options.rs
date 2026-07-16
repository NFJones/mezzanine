//! Runtime terminal and history option readers.
//!
//! This module owns history, terminal identity, cursor, timing, clipboard,
//! and host-clipboard option materialization from the effective runtime
//! configuration value. Keeping these readers together separates terminal
//! live-option parsing from frame, agent, permission, provider, and hook
//! config domains.

use serde_json::Value;

use crate::error::{MezError, Result};
use crate::terminal::DEFAULT_AGENT_WRAP_COLUMN_CAP;
use crate::terminal::{HostClipboard, HostClipboardCommand};
use crate::transcript::DEFAULT_SAVED_AGENT_SESSION_LIMIT;
use mez_mux::presentation::TerminalCursorStyle;
use mez_terminal::{
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, DEFAULT_PANE_TERM, TerminalEmojiWidth,
};

use super::{runtime_json_object, runtime_json_string, validate_runtime_terminal_term};

/// Runs the runtime history limit from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_history_limit_from_config(root: &Value) -> Result<usize> {
    let Some(history) = runtime_json_object(root, "history") else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    let Some(value) = history.get("lines") else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    let Some(limit) = value.as_u64() else {
        return Err(MezError::config("history.lines must be a positive integer"));
    };
    let limit = usize::try_from(limit)
        .map_err(|_| MezError::config("history.lines is too large for this platform"))?;
    if limit == 0 {
        return Err(MezError::config("history.lines must be greater than zero"));
    }
    Ok(limit)
}

/// Reads the configured saved agent conversation retention limit.
pub(crate) fn runtime_saved_agent_session_limit_from_config(root: &Value) -> Result<usize> {
    let Some(history) = runtime_json_object(root, "history") else {
        return Ok(DEFAULT_SAVED_AGENT_SESSION_LIMIT);
    };
    let Some(value) = history.get("saved_sessions_limit") else {
        return Ok(DEFAULT_SAVED_AGENT_SESSION_LIMIT);
    };
    let Some(limit) = value.as_u64() else {
        return Err(MezError::config(
            "history.saved_sessions_limit must be a positive integer",
        ));
    };
    let limit = usize::try_from(limit).map_err(|_| {
        MezError::config("history.saved_sessions_limit is too large for this platform")
    })?;
    if limit == 0 {
        return Err(MezError::config(
            "history.saved_sessions_limit must be greater than zero",
        ));
    }
    Ok(limit)
}

/// Reads the configured terminal history overflow rotation batch.
pub(crate) fn runtime_history_rotate_lines_from_config(root: &Value) -> Result<usize> {
    let Some(history) = runtime_json_object(root, "history") else {
        return Ok(DEFAULT_HISTORY_ROTATE_LINES);
    };
    let Some(value) = history.get("rotate_lines") else {
        return Ok(DEFAULT_HISTORY_ROTATE_LINES);
    };
    let Some(rotate_lines) = value.as_u64() else {
        return Err(MezError::config(
            "history.rotate_lines must be a positive integer",
        ));
    };
    let rotate_lines = usize::try_from(rotate_lines)
        .map_err(|_| MezError::config("history.rotate_lines is too large for this platform"))?;
    if rotate_lines == 0 {
        return Err(MezError::config(
            "history.rotate_lines must be greater than zero",
        ));
    }
    Ok(rotate_lines)
}

/// Runs the runtime terminal term from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_term_from_config(root: &Value) -> Result<String> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(DEFAULT_PANE_TERM.to_string());
    };
    let Some(value) = terminal.get("term") else {
        return Ok(DEFAULT_PANE_TERM.to_string());
    };
    let Some(term) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.term must be a string"));
    };
    validate_runtime_terminal_term(term)?;
    Ok(term.to_string())
}

/// Runs the runtime terminal cursor style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_cursor_style_from_config(
    root: &Value,
) -> Result<TerminalCursorStyle> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(TerminalCursorStyle::Block);
    };
    let Some(value) = terminal.get("cursor_style") else {
        return Ok(TerminalCursorStyle::Block);
    };
    let Some(style) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.cursor_style must be a string"));
    };
    match style {
        "block" => Ok(TerminalCursorStyle::Block),
        "underline" => Ok(TerminalCursorStyle::Underline),
        "bar" => Ok(TerminalCursorStyle::Bar),
        _ => Err(MezError::config(
            "terminal.cursor_style must be block, underline, or bar",
        )),
    }
}

/// Runs the runtime terminal cursor blink from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_cursor_blink_from_config(root: &Value) -> Result<bool> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(false);
    };
    let Some(value) = terminal.get("cursor_blink") else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("terminal.cursor_blink must be a boolean"))
}

/// Runs the runtime terminal cursor blink interval ms from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_cursor_blink_interval_ms_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(500);
    };
    let Some(value) = terminal.get("cursor_blink_interval_ms") else {
        return Ok(500);
    };
    let Some(interval) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.cursor_blink_interval_ms must be a positive integer",
        ));
    };
    if interval == 0 {
        return Err(MezError::config(
            "terminal.cursor_blink_interval_ms must be greater than zero",
        ));
    }
    Ok(interval)
}

/// Returns the configured emoji status-glyph width policy for terminal
/// measurement.
pub(crate) fn runtime_terminal_emoji_width_from_config(root: &Value) -> Result<TerminalEmojiWidth> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(TerminalEmojiWidth::Wide);
    };
    let Some(value) = terminal.get("emoji_width") else {
        return Ok(TerminalEmojiWidth::Wide);
    };
    let Some(emoji_width) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.emoji_width must be a string"));
    };
    match emoji_width {
        "wide" => Ok(TerminalEmojiWidth::Wide),
        "narrow" => Ok(TerminalEmojiWidth::Narrow),
        _ => Err(MezError::config(
            "terminal.emoji_width must be wide or narrow",
        )),
    }
}

/// Runs the runtime terminal resize debounce ms from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_resize_debounce_ms_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(200);
    };
    let Some(value) = terminal.get("resize_debounce_ms") else {
        return Ok(200);
    };
    let Some(interval) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.resize_debounce_ms must be a positive integer",
        ));
    };
    if interval == 0 {
        return Err(MezError::config(
            "terminal.resize_debounce_ms must be greater than zero",
        ));
    }
    Ok(interval)
}

/// Returns the configured attached-terminal render rate limit in frames per second.
pub(crate) fn runtime_terminal_render_rate_limit_fps_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(5);
    };
    let Some(value) = terminal.get("render_rate_limit_fps") else {
        return Ok(5);
    };
    value.as_u64().ok_or_else(|| {
        MezError::config("terminal.render_rate_limit_fps must be a non-negative integer")
    })
}

/// Returns the configured hidden shell-output preview tail line count.
pub(crate) fn runtime_terminal_shell_output_preview_lines_from_config(
    root: &Value,
) -> Result<usize> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(5);
    };
    let Some(value) = terminal.get("shell_output_preview_lines") else {
        return Ok(5);
    };
    let Some(lines) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.shell_output_preview_lines must be a positive integer",
        ));
    };
    if lines == 0 {
        return Err(MezError::config(
            "terminal.shell_output_preview_lines must be greater than zero",
        ));
    }
    usize::try_from(lines).map_err(|_| {
        MezError::config("terminal.shell_output_preview_lines is too large for this platform")
    })
}

/// Returns the configured maximum display width for Mezzanine-owned agent rows.
pub(crate) fn runtime_terminal_agent_wrap_column_cap_from_config(root: &Value) -> Result<usize> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(DEFAULT_AGENT_WRAP_COLUMN_CAP);
    };
    let Some(value) = terminal.get("agent_wrap_column_cap") else {
        return Ok(DEFAULT_AGENT_WRAP_COLUMN_CAP);
    };
    let Some(columns) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.agent_wrap_column_cap must be a positive integer",
        ));
    };
    if columns == 0 {
        return Err(MezError::config(
            "terminal.agent_wrap_column_cap must be greater than zero",
        ));
    }
    usize::try_from(columns).map_err(|_| {
        MezError::config("terminal.agent_wrap_column_cap is too large for this platform")
    })
}

/// Returns whether optional terminal animations should render as static UI.
pub(crate) fn runtime_terminal_reduced_motion_from_config(root: &Value) -> Result<bool> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(false);
    };
    let Some(value) = terminal.get("reduced_motion") else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("terminal.reduced_motion must be true or false"))
}

/// Runs the runtime terminal clipboard from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_clipboard_from_config(root: &Value) -> Result<String> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok("external".to_string());
    };
    let Some(value) = terminal.get("clipboard") else {
        return Ok("external".to_string());
    };
    let Some(clipboard) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.clipboard must be a string"));
    };
    match clipboard {
        "external" | "host" | "internal" | "disabled" | "off" | "none" => Ok(clipboard.to_string()),
        _ => Err(MezError::config(
            "terminal.clipboard must be external, internal, or disabled",
        )),
    }
}

/// Runs the runtime host clipboard from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_host_clipboard_from_config(root: &Value) -> Result<HostClipboard> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(HostClipboard::system());
    };
    let copy = runtime_clipboard_command_from_config(
        terminal.get("clipboard_copy_command"),
        "terminal.clipboard_copy_command",
    )?;
    let paste = runtime_clipboard_command_from_config(
        terminal.get("clipboard_paste_command"),
        "terminal.clipboard_paste_command",
    )?;
    if copy.is_none() && paste.is_none() {
        return Ok(HostClipboard::system());
    }
    Ok(HostClipboard::configured(copy, paste))
}

/// Parses one optional host clipboard command value.
///
/// # Parameters
/// - `value`: The configuration value to parse.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_config(
    value: Option<&Value>,
    name: &str,
) -> Result<Option<HostClipboardCommand>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::String(command) => runtime_clipboard_command_from_string(command, name).map(Some),
        Value::Array(arguments) => runtime_clipboard_command_from_array(arguments, name).map(Some),
        _ => Err(MezError::config(format!(
            "{name} must be a command string or array of strings"
        ))),
    }
}

/// Parses one shell-like clipboard command string.
///
/// # Parameters
/// - `command`: The command text to split.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_string(
    command: &str,
    name: &str,
) -> Result<HostClipboardCommand> {
    let arguments = shlex::split(command).ok_or_else(|| {
        MezError::config(format!("{name} must contain a valid shell-like command"))
    })?;
    runtime_clipboard_command_from_arguments(arguments, name)
}

/// Parses one clipboard command array.
///
/// # Parameters
/// - `arguments`: The command tokens supplied in configuration.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_array(
    arguments: &[Value],
    name: &str,
) -> Result<HostClipboardCommand> {
    let mut parsed = Vec::new();
    for argument in arguments {
        let Some(argument) = argument.as_str() else {
            return Err(MezError::config(format!(
                "{name} must contain only string arguments"
            )));
        };
        parsed.push(argument.to_string());
    }
    runtime_clipboard_command_from_arguments(parsed, name)
}

/// Builds a clipboard command from parsed command tokens.
///
/// # Parameters
/// - `arguments`: The command tokens with the program in the first slot.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_arguments(
    mut arguments: Vec<String>,
    name: &str,
) -> Result<HostClipboardCommand> {
    if arguments.is_empty() || arguments[0].trim().is_empty() {
        return Err(MezError::config(format!("{name} must not be empty")));
    }
    let program = arguments.remove(0);
    Ok(HostClipboardCommand::new(program, arguments))
}
