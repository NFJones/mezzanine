//! Regression tests for terminal mouse protocol parsing.

use crate::{MouseButton, MouseEventKind, parse_sgr_mouse};

/// Verifies SGR mouse packets preserve button actions and zero-based cells.
#[test]
fn parses_sgr_mouse_press_drag_release_and_scroll() {
    let press = parse_sgr_mouse(b"\x1b[<0;12;5M").unwrap();
    assert_eq!(press.kind, MouseEventKind::Press);
    assert_eq!(press.button, MouseButton::Left);
    assert_eq!(press.column, 11);
    assert_eq!(press.row, 4);

    let drag = parse_sgr_mouse(b"\x1b[<32;12;6M").unwrap();
    assert_eq!(drag.kind, MouseEventKind::Drag);

    let release = parse_sgr_mouse(b"\x1b[<0;12;6m").unwrap();
    assert_eq!(release.kind, MouseEventKind::Release);

    let scroll = parse_sgr_mouse(b"\x1b[<65;12;6M").unwrap();
    assert_eq!(scroll.kind, MouseEventKind::Scroll);
    assert_eq!(scroll.button, MouseButton::WheelDown);
}

/// Verifies malformed SGR mouse packets with extra fields are rejected.
#[test]
fn rejects_sgr_mouse_packets_with_extra_fields() {
    assert!(parse_sgr_mouse(b"\x1b[<0;12;5;999M").is_none());
}
