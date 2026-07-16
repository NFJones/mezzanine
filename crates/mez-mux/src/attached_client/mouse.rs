//! Application mouse and cursor forwarding encoding.
//!
//! This module translates neutral terminal mouse events into pane-local SGR or
//! legacy xterm packets and recognizes packet boundaries in batched host input.
//! Product routing decides whether a packet should be forwarded.

use crate::input::{MousePaneRegion, MousePolicy};
use mez_terminal::{MouseButton, MouseEvent, MouseEventKind};

/// Encodes one host mouse event in pane-local coordinates and negotiated mode.
pub fn application_mouse_forwarding_bytes(
    event: MouseEvent,
    region: &MousePaneRegion,
) -> Option<Vec<u8>> {
    let local_column = event.column.checked_sub(region.column)?.saturating_add(1);
    let local_row = event.row.checked_sub(region.row)?.saturating_add(1);
    if region.application_sgr_mouse_mode {
        return Some(encode_sgr_mouse_event(event, local_column, local_row).into_bytes());
    }
    encode_legacy_xterm_mouse_event(event, local_column, local_row)
}

/// Returns the first SGR mouse-packet prefix in a host input batch.
pub fn sgr_mouse_sequence_start(input: &[u8]) -> Option<usize> {
    input.windows(3).position(|window| window == b"\x1b[<")
}

/// Returns the length of a complete leading SGR mouse packet.
pub fn sgr_mouse_sequence_len(input: &[u8]) -> Option<usize> {
    if !input.starts_with(b"\x1b[<") {
        return None;
    }
    input
        .iter()
        .position(|byte| matches!(byte, b'M' | b'm'))
        .map(|index| index.saturating_add(1))
}

/// Returns the malformed leading SGR prefix length before ordinary input.
pub fn malformed_sgr_mouse_prefix_len(input: &[u8]) -> Option<usize> {
    if !input.starts_with(b"\x1b[<") {
        return None;
    }
    for (index, byte) in input.iter().enumerate().skip(3) {
        match byte {
            b'0'..=b'9' | b';' => continue,
            b'M' | b'm' => return None,
            _ => return Some(index),
        }
    }
    None
}

/// Rewrites ordinary cursor keys for an application-cursor-mode pane.
pub fn application_cursor_forwarding_bytes(input: &[u8], policy: MousePolicy) -> Option<Vec<u8>> {
    if !policy.pane_application_cursor_mode {
        return None;
    }
    match input {
        b"\x1b[A" => Some(b"\x1bOA".to_vec()),
        b"\x1b[B" => Some(b"\x1bOB".to_vec()),
        b"\x1b[C" => Some(b"\x1bOC".to_vec()),
        b"\x1b[D" => Some(b"\x1bOD".to_vec()),
        _ => None,
    }
}

/// Encodes one event using xterm SGR mouse syntax.
fn encode_sgr_mouse_event(event: MouseEvent, column: u16, row: u16) -> String {
    let code = mouse_event_code(event);
    let final_byte = if event.kind == MouseEventKind::Release {
        'm'
    } else {
        'M'
    };
    format!("\x1b[<{code};{column};{row}{final_byte}")
}

/// Encodes one event using the bounded legacy xterm mouse packet.
fn encode_legacy_xterm_mouse_event(event: MouseEvent, column: u16, row: u16) -> Option<Vec<u8>> {
    let code = match event.kind {
        MouseEventKind::Release => 3u16.saturating_add(mouse_modifier_code(event)),
        _ => mouse_event_code(event),
    };
    Some(vec![
        b'\x1b',
        b'[',
        b'M',
        u8::try_from(code.saturating_add(32)).ok()?,
        legacy_mouse_coordinate(column)?,
        legacy_mouse_coordinate(row)?,
    ])
}

/// Encodes one one-based legacy mouse coordinate.
fn legacy_mouse_coordinate(value: u16) -> Option<u8> {
    if value == 0 || value > 223 {
        return None;
    }
    u8::try_from(value.saturating_add(32)).ok()
}

/// Returns the xterm button, wheel, drag, and modifier code.
fn mouse_event_code(event: MouseEvent) -> u16 {
    let button: u16 = match event.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::Other(code) => code,
    };
    let drag = u16::from(matches!(event.kind, MouseEventKind::Drag)).saturating_mul(32);
    button
        .saturating_add(drag)
        .saturating_add(mouse_modifier_code(event))
}

/// Returns the xterm modifier bit field for one mouse event.
fn mouse_modifier_code(event: MouseEvent) -> u16 {
    u16::from(event.modifiers.shift).saturating_mul(4)
        + u16::from(event.modifiers.alt).saturating_mul(8)
        + u16::from(event.modifiers.ctrl).saturating_mul(16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::MousePaneRegion;
    use mez_terminal::parse_sgr_mouse;

    /// Verifies pane-local SGR forwarding preserves button and coordinates.
    #[test]
    fn sgr_mouse_forwarding_uses_pane_local_coordinates() {
        let event = parse_sgr_mouse(b"\x1b[<0;12;5M").unwrap();
        let region = MousePaneRegion {
            pane_id: "%1".to_string(),
            column: 10,
            row: 3,
            columns: 20,
            rows: 10,
            active: true,
            application_mouse_mode: true,
            application_sgr_mouse_mode: true,
            copy_mode_active: false,
        };
        assert_eq!(
            application_mouse_forwarding_bytes(event, &region),
            Some(b"\x1b[<0;2;2M".to_vec())
        );
    }

    /// Verifies malformed SGR prefixes can be skipped without losing suffix input.
    #[test]
    fn malformed_sgr_prefix_reports_only_invalid_prefix() {
        assert_eq!(malformed_sgr_mouse_prefix_len(b"\x1b[<0;12;bad"), Some(8));
        assert_eq!(sgr_mouse_sequence_len(b"\x1b[<0;12;5Mtail"), Some(10));
    }

    /// Verifies application cursor mode rewrites arrows and ignores ordinary text.
    #[test]
    fn application_cursor_mode_rewrites_arrow_sequences() {
        let policy = MousePolicy {
            pane_application_cursor_mode: true,
            ..MousePolicy::default()
        };
        assert_eq!(
            application_cursor_forwarding_bytes(b"\x1b[A", policy),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(application_cursor_forwarding_bytes(b"x", policy), None);
    }
}
