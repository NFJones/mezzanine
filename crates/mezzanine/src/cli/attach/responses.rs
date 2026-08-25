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
        .and_then(|result| result.get("client"))
        .and_then(|client| client.get("id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MezError::invalid_state("control initialize did not return a primary client id")
        })?;
    ClientId::parse('c', client_id.to_string())
        .ok_or_else(|| MezError::invalid_state("control initialize returned an invalid client id"))
}

/// Extracts the one-time Unix event binding token from initialization.
pub(super) fn event_binding_token_from_initialize_response(body: &str) -> Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("control initialize response is not valid JSON"))?;
    parsed
        .get("result")
        .and_then(|result| result.get("event_binding"))
        .and_then(|binding| binding.get("token"))
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            MezError::invalid_state("control initialize did not return an event binding")
        })
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

/// Decodes one terminal view for client-local semantic overlays.
pub(super) fn terminal_step_response_client_frame(
    body: &str,
) -> Result<Option<super::AttachClientFrame>> {
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
    let iroh_status_slot = view
        .get("iroh_status_slot")
        .filter(|slot| !slot.is_null())
        .map(parse_terminal_iroh_status_slot)
        .transpose()?;
    Ok(Some(super::AttachClientFrame {
        lines: terminal_step_response_lines(body)?,
        line_style_spans: terminal_step_response_line_style_spans(body)?,
        modes: terminal_step_response_output_modes(body)?.unwrap_or_default(),
        iroh_status_slot,
    }))
}

/// Decodes one server-owned client-space Iroh status slot.
fn parse_terminal_iroh_status_slot(
    value: &serde_json::Value,
) -> Result<crate::host::terminal::TerminalIrohStatusSlot> {
    let number = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| MezError::invalid_state("terminal Iroh status slot is incomplete"))
            .and_then(|value| {
                usize::try_from(value)
                    .map_err(|_| MezError::invalid_state("terminal Iroh status slot is too large"))
            })
    };
    let rendition = |field: &str| {
        value
            .get(field)
            .ok_or_else(|| MezError::invalid_state("terminal Iroh status rendition is missing"))
            .and_then(parse_terminal_graphic_rendition)
    };
    Ok(crate::host::terminal::TerminalIrohStatusSlot {
        row: number("row")?,
        column: number("column")?,
        width: number("width")?,
        good: rendition("good")?,
        degraded: rendition("degraded")?,
        poor: rendition("poor")?,
        unknown: rendition("unknown")?,
    })
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
    let client_detached = parsed
        .get("result")
        .and_then(|result| result.get("client_detached"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let session_terminated = parsed
        .get("result")
        .and_then(|result| result.get("session_terminated"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(TerminalStepRefreshRequirement {
        view_refresh_required: view_refresh_required || full_redraw_required,
        full_redraw_required,
        client_detached,
        session_terminated,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::terminal::TerminalIrohStatusQuality;

    /// Verifies semantic slot decoding and local composition use only the
    /// connected/disconnected labels while quality changes rendition colors.
    #[test]
    fn terminal_view_iroh_slot_decodes_and_composes_local_state() {
        let response = r#"{"result":{"view":{"lines":["base              tail"],"line_style_spans":[[{"start":0,"length":4,"rendition":{"foreground":null,"background":{"kind":"indexed","index":7}}},{"start":19,"length":4,"rendition":{"foreground":null,"background":{"kind":"indexed","index":6}}}]],"cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"iroh_status_slot":{"row":0,"column":4,"width":14,"good":{"foreground":null,"background":{"kind":"indexed","index":2}},"degraded":{"foreground":null,"background":{"kind":"indexed","index":3}},"poor":{"foreground":null,"background":{"kind":"indexed","index":1}},"unknown":{"foreground":null,"background":{"kind":"indexed","index":8}}}}}}"#;
        let frame = terminal_step_response_client_frame(response)
            .unwrap()
            .expect("view should decode");

        for (quality, expected_background) in [
            (TerminalIrohStatusQuality::Good, TerminalColor::Indexed(2)),
            (
                TerminalIrohStatusQuality::Degraded,
                TerminalColor::Indexed(3),
            ),
            (TerminalIrohStatusQuality::Poor, TerminalColor::Indexed(1)),
            (
                TerminalIrohStatusQuality::Unknown,
                TerminalColor::Indexed(8),
            ),
        ] {
            let (lines, spans) = frame.with_iroh_status(true, quality);
            assert_eq!(lines, ["base connected    tail"]);
            assert!(spans[0].iter().any(|span| {
                span.start == 0
                    && span.length == 4
                    && span.rendition.background == Some(TerminalColor::Indexed(7))
            }));
            assert!(spans[0].iter().any(|span| {
                span.start == 19
                    && span.length == 4
                    && span.rendition.background == Some(TerminalColor::Indexed(6))
            }));
            assert_eq!(
                spans[0]
                    .iter()
                    .find(|span| span.start == 4 && span.length == 14)
                    .unwrap()
                    .rendition
                    .background,
                Some(expected_background)
            );
        }

        let (lines, spans) = frame.with_iroh_status(false, TerminalIrohStatusQuality::Poor);
        assert_eq!(lines, ["base disconnected tail"]);
        assert_eq!(
            spans[0]
                .iter()
                .find(|span| span.start == 4 && span.length == 14)
                .unwrap()
                .rendition
                .background,
            Some(TerminalColor::Indexed(8))
        );
    }

    /// Verifies ordinary Unix-compatible frames omit local Iroh composition.
    #[test]
    fn terminal_view_without_iroh_slot_remains_unchanged() {
        let response = r#"{"result":{"view":{"lines":["plain"],"line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},"output_modes":{}}}}"#;
        let frame = terminal_step_response_client_frame(response)
            .unwrap()
            .expect("view should decode");
        let (lines, spans) = frame.with_iroh_status(true, TerminalIrohStatusQuality::Good);
        assert_eq!(lines, ["plain"]);
        assert_eq!(spans, [Vec::new()]);
    }
}
