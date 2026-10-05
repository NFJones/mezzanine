//! Shared cursor and output-mode decoding for terminal transport adapters.
//!
//! Preserves existing attach defaults and field interpretation. These values
//! describe presentation, not input or receipt authority. The decoder does not
//! enforce viewport geometry or enable modes on a host terminal; consumers own
//! those boundaries and the enclosing frame budgets.

use crate::error::{MezError, Result};
use mez_mux::presentation::{AttachedTerminalOutputModes, TerminalCursorStyle};

/// Decodes an optional cursor-bearing view using established attach defaults.
/// Missing cursor returns None; malformed required coordinates or explicit
/// cursor style reject. Geometry validation remains the consumer's obligation.
pub(crate) fn parse_view_output_modes(
    view: &serde_json::Value,
) -> Result<Option<AttachedTerminalOutputModes>> {
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
    let focus_events = view
        .get("output_modes")
        .and_then(|modes| modes.get("focus_events"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let alternate_screen = view
        .get("output_modes")
        .and_then(|modes| modes.get("alternate_screen"))
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
        focus_events,
        alternate_screen,
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
mod tests;
