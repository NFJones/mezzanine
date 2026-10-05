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

/// Requires cursor evidence within a finite snapshot viewport. Legacy attach
/// parsing remains unchanged; this opt-in boundary never applies host modes.
pub(crate) fn bounded_view_output_modes(
    view: &serde_json::Value,
    columns: u16,
    rows: u16,
) -> Result<AttachedTerminalOutputModes> {
    if !(1..=4096).contains(&columns) || !(1..=4096).contains(&rows) {
        return Err(MezError::invalid_state(
            "snapshot output geometry unavailable",
        ));
    }
    let modes = parse_view_output_modes(view)?
        .ok_or_else(|| MezError::invalid_state("snapshot cursor evidence unavailable"))?;
    if modes.cursor_row >= usize::from(rows) || modes.cursor_column >= usize::from(columns) {
        return Err(MezError::invalid_state("snapshot cursor exceeds geometry"));
    }
    Ok(modes)
}

/// Projects only decoded cursor and output-mode fields into the established
/// view shape. Local-only enhanced keyboard and blink phase are not exported.
pub(crate) fn output_modes_view_value(modes: AttachedTerminalOutputModes) -> serde_json::Value {
    let style = match modes.cursor_style {
        TerminalCursorStyle::Block => "block",
        TerminalCursorStyle::Underline => "underline",
        TerminalCursorStyle::Bar => "bar",
    };
    serde_json::json!({
        "cursor": {"row":modes.cursor_row,"column":modes.cursor_column,
            "visible":modes.cursor_visible,"style":style,"blink":modes.cursor_blink,
            "blink_interval_ms":modes.cursor_blink_interval_ms},
        "output_modes": {"application_keypad":modes.application_keypad,
            "bracketed_paste":modes.bracketed_paste,"focus_events":modes.focus_events,
            "alternate_screen":modes.alternate_screen,"host_mouse_reporting":modes.host_mouse_reporting,
            "animation_refresh_interval_ms":modes.animation_refresh_interval_ms}
    })
}

#[cfg(test)]
mod tests;
