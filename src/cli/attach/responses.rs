//! Attached-client control response validation and terminal payload decoding.

use super::{
    AttachedTerminalOutputModes, ClientId, GraphicRendition, MezError, Result, TerminalColor,
    TerminalCursorStyle, TerminalStepRefreshRequirement, TerminalStyleSpan, json_escape,
};

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
pub(super) fn control_response_forbidden(body: &str) -> Result<bool> {
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
pub(super) fn observer_request_id_from_value(value: &serde_json::Value) -> Option<&str> {
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
pub(in crate::cli) fn terminal_step_response_refresh_requirement(
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
pub(in crate::cli) fn terminal_step_response_line_style_spans(
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
pub(super) fn parse_terminal_style_span_row(
    value: &serde_json::Value,
) -> Result<Vec<TerminalStyleSpan>> {
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
pub(super) fn parse_terminal_style_span(value: &serde_json::Value) -> Result<TerminalStyleSpan> {
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
pub(super) fn parse_terminal_graphic_rendition(
    value: &serde_json::Value,
) -> Result<GraphicRendition> {
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
pub(super) fn bool_field(value: &serde_json::Value, field: &str) -> bool {
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
pub(super) fn parse_terminal_color_field(
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
pub(super) fn parse_terminal_color_value(color: &serde_json::Value) -> Result<TerminalColor> {
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
pub(super) fn parse_u8_color_component(value: &serde_json::Value, field: &str) -> Result<u8> {
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
pub(in crate::cli) fn terminal_step_response_output_modes(
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
