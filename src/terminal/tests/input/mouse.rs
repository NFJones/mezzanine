//! Regression tests for terminal input mouse behavior.

use crate::terminal::{MouseAction, classify_mouse_event};
use mez_mux::copy::CopyPosition;
use mez_mux::input::MousePolicy;
use mez_terminal::{MouseButton, MouseEvent, MouseEventKind, MouseModifiers};

/// Verifies classifies mouse actions for resize selection scroll and forwarding.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn classifies_mouse_actions_for_resize_selection_scroll_and_forwarding() {
    let event = MouseEvent {
        kind: MouseEventKind::Drag,
        button: MouseButton::Left,
        column: 4,
        row: 2,
        modifiers: MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        },
    };

    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ResizePane { column: 4, row: 2 }
    );
    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::CopySelectionUpdate(CopyPosition { line: 2, column: 4 })
    );
    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ResizePane { column: 4, row: 2 }
    );

    let pane_drag = MouseEvent {
        column: 8,
        row: 3,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            pane_drag,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );

    let scroll = MouseEvent {
        kind: MouseEventKind::Scroll,
        button: MouseButton::WheelUp,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ScrollHistory {
            lines: -3,
            position: CopyPosition { line: 2, column: 4 },
        }
    );
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::ScrollHistory {
            lines: -3,
            position: CopyPosition { line: 2, column: 4 },
        }
    );

    let right_click = MouseEvent {
        kind: MouseEventKind::Press,
        button: MouseButton::Right,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            right_click,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );

    let release = MouseEvent {
        kind: MouseEventKind::Release,
        button: MouseButton::Left,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            release,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::CopySelectionFinish(CopyPosition { line: 2, column: 4 })
    );
}
