//! Application mouse and cursor forwarding encoding.
//!
//! This module translates neutral terminal mouse events into pane-local SGR or
//! legacy xterm packets and recognizes packet boundaries in batched host input.
//! Product routing decides whether a packet should be forwarded.

use crate::copy::CopyPosition;
use crate::input::{MouseBorderCell, MousePaneRegion, MousePolicy};
use crate::layout::PaneGeometry;
use crate::presentation::pane_divider_cells;
use mez_terminal::{MouseButton, MouseEvent, MouseEventKind};

/// Neutral mux action selected from one host mouse event and routing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedMouseAction {
    /// Consume the event without changing mux state.
    Ignore,
    /// Forward the event to the pane application.
    ForwardToPane,
    /// Open the window chooser at the frame coordinate.
    ShowWindowChooser {
        /// Zero-based host terminal column.
        column: u16,
        /// Zero-based host terminal row.
        row: u16,
    },
    /// Resize the pane border under the pointer.
    ResizePane {
        /// Zero-based host terminal column.
        column: u16,
        /// Zero-based host terminal row.
        row: u16,
    },
    /// Finish an active pane resize gesture.
    FinishResizePane,
    /// Begin a copy-mode selection.
    CopySelectionStart(CopyPosition),
    /// Update a copy-mode or drag selection.
    CopySelectionUpdate(CopyPosition),
    /// Finish a copy-mode selection.
    CopySelectionFinish(CopyPosition),
    /// Scroll terminal history around the pointer position.
    ScrollHistory {
        /// Signed history-row delta.
        lines: isize,
        /// Pointer position used to select the target pane.
        position: CopyPosition,
    },
    /// Paste the clipboard at the pointer position.
    PasteClipboard(CopyPosition),
    /// Focus the pane under the pointer.
    FocusPane(CopyPosition),
}

/// Classifies one host mouse event using dependency-neutral mux policy.
pub fn classify_attached_mouse_event(
    event: MouseEvent,
    policy: MousePolicy,
) -> AttachedMouseAction {
    if !policy.enabled {
        return AttachedMouseAction::Ignore;
    }
    if matches!(
        (event.kind, event.button),
        (MouseEventKind::Press, MouseButton::Left)
    ) && policy.over_window_frame
    {
        return AttachedMouseAction::ShowWindowChooser {
            column: event.column,
            row: event.row,
        };
    }
    if policy.pane_resize_active {
        return match (event.kind, event.button) {
            (MouseEventKind::Press | MouseEventKind::Drag, MouseButton::Left) => {
                AttachedMouseAction::ResizePane {
                    column: event.column,
                    row: event.row,
                }
            }
            (MouseEventKind::Release, MouseButton::Left) => AttachedMouseAction::FinishResizePane,
            _ => AttachedMouseAction::Ignore,
        };
    }
    match (event.kind, event.button) {
        (MouseEventKind::Press, MouseButton::Left) if policy.copy_mode_active => {
            AttachedMouseAction::CopySelectionStart(mouse_copy_position(event))
        }
        (MouseEventKind::Drag, MouseButton::Left) if policy.copy_mode_active => {
            AttachedMouseAction::CopySelectionUpdate(mouse_copy_position(event))
        }
        (MouseEventKind::Release, MouseButton::Left) if policy.copy_mode_active => {
            AttachedMouseAction::CopySelectionFinish(mouse_copy_position(event))
        }
        (MouseEventKind::Scroll, MouseButton::WheelUp) if policy.copy_mode_active => {
            AttachedMouseAction::ScrollHistory {
                lines: -3,
                position: mouse_copy_position(event),
            }
        }
        (MouseEventKind::Scroll, MouseButton::WheelDown) if policy.copy_mode_active => {
            AttachedMouseAction::ScrollHistory {
                lines: 3,
                position: mouse_copy_position(event),
            }
        }
        (MouseEventKind::Press | MouseEventKind::Drag, MouseButton::Left)
            if policy.over_pane_border =>
        {
            AttachedMouseAction::ResizePane {
                column: event.column,
                row: event.row,
            }
        }
        _ if policy.over_window_frame || policy.over_pane_border => AttachedMouseAction::Ignore,
        _ if policy.pane_application_mouse_mode => AttachedMouseAction::ForwardToPane,
        (MouseEventKind::Scroll, MouseButton::WheelUp) => AttachedMouseAction::ScrollHistory {
            lines: -3,
            position: mouse_copy_position(event),
        },
        (MouseEventKind::Scroll, MouseButton::WheelDown) => AttachedMouseAction::ScrollHistory {
            lines: 3,
            position: mouse_copy_position(event),
        },
        (MouseEventKind::Press, MouseButton::Right) => {
            AttachedMouseAction::PasteClipboard(mouse_copy_position(event))
        }
        (MouseEventKind::Release | MouseEventKind::Drag, MouseButton::Right) => {
            AttachedMouseAction::Ignore
        }
        (MouseEventKind::Press, MouseButton::Left) => {
            AttachedMouseAction::FocusPane(mouse_copy_position(event))
        }
        (MouseEventKind::Drag, MouseButton::Left) => {
            AttachedMouseAction::CopySelectionUpdate(mouse_copy_position(event))
        }
        _ => AttachedMouseAction::Ignore,
    }
}

/// Converts a terminal coordinate into a mux copy-buffer position.
fn mouse_copy_position(event: MouseEvent) -> CopyPosition {
    CopyPosition {
        line: usize::from(event.row),
        column: usize::from(event.column),
    }
}

/// Returns mouse hit cells for every mux-managed pane divider.
///
/// `row_offset` places window-local divider geometry into attached-client
/// coordinates when product frames reserve rows above the window body.
pub fn mouse_border_cells_for_geometries(
    geometries: &[PaneGeometry],
    row_offset: u16,
) -> Vec<MouseBorderCell> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .map(|cell| MouseBorderCell {
            column: cell.column,
            row: cell.row.saturating_add(row_offset),
        })
        .collect()
}

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

    /// Verifies resize, copy, scroll, and pane-forwarding precedence is owned
    /// by the neutral attached-client classifier.
    #[test]
    fn classifies_attached_mouse_policy_precedence() {
        let drag = parse_sgr_mouse(b"\x1b[<32;5;3M").unwrap();
        let border_policy = MousePolicy {
            enabled: true,
            over_pane_border: true,
            ..MousePolicy::default()
        };
        assert_eq!(
            classify_attached_mouse_event(drag, border_policy),
            AttachedMouseAction::ResizePane { column: 4, row: 2 }
        );

        let copy_policy = MousePolicy {
            pane_application_mouse_mode: true,
            copy_mode_active: true,
            ..border_policy
        };
        assert_eq!(
            classify_attached_mouse_event(drag, copy_policy),
            AttachedMouseAction::CopySelectionUpdate(CopyPosition { line: 2, column: 4 })
        );

        let pane_policy = MousePolicy {
            over_pane_border: false,
            copy_mode_active: false,
            ..copy_policy
        };
        assert_eq!(
            classify_attached_mouse_event(drag, pane_policy),
            AttachedMouseAction::ForwardToPane
        );
    }

    /// Verifies divider hit cells preserve mux geometry while applying the
    /// attached-client row offset reserved by product frames.
    #[test]
    fn pane_divider_mouse_cells_apply_client_row_offset() {
        let geometries = [
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 10,
                rows: 5,
            },
            PaneGeometry {
                index: 1,
                column: 10,
                row: 0,
                columns: 10,
                rows: 5,
            },
        ];

        assert!(
            mouse_border_cells_for_geometries(&geometries, 2)
                .iter()
                .all(|cell| cell.column == 9 && (2..7).contains(&cell.row))
        );
    }

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
