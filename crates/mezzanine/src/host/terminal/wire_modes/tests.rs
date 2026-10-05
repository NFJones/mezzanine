//! Shared presentation-mode decoding without activating terminal or input modes.
use super::*;

/// Extraction preserves defaults, cursor shapes and each output-mode flag.
/// Presentation values alone cannot change input authority or host state.
#[test]
fn wire_modes_preserve_attach_defaults_and_explicit_fields() {
    assert!(
        parse_view_output_modes(&serde_json::json!({}))
            .unwrap()
            .is_none()
    );
    let defaults = parse_view_output_modes(&serde_json::json!({
        "cursor":{"row":1,"column":2,"visible":false}
    }))
    .unwrap()
    .unwrap();
    assert!(defaults.cursor_blink && defaults.host_mouse_reporting);
    assert_eq!(defaults.cursor_blink_interval_ms, 500);
    assert_eq!((defaults.cursor_row, defaults.cursor_column), (1, 2));
    for (style, expected) in [
        ("block", TerminalCursorStyle::Block),
        ("underline", TerminalCursorStyle::Underline),
        ("bar", TerminalCursorStyle::Bar),
    ] {
        let modes = parse_view_output_modes(&serde_json::json!({
            "cursor":{"row":0,"column":0,"visible":true,"style":style,
                "blink":false,"blink_interval_ms":123},
            "output_modes":{"application_keypad":true,"bracketed_paste":true,
                "focus_events":true,"alternate_screen":true,"host_mouse_reporting":false,
                "animation_refresh_interval_ms":42}
        }))
        .unwrap()
        .unwrap();
        assert_eq!(modes.cursor_style, expected);
        assert!(
            modes.application_keypad
                && modes.bracketed_paste
                && modes.focus_events
                && modes.alternate_screen
        );
        assert!(!modes.host_mouse_reporting);
        assert!(!modes.cursor_blink);
        assert_eq!(modes.cursor_blink_interval_ms, 123);
        assert_eq!(modes.animation_refresh_interval_ms, 42);
        assert!(!modes.enhanced_keyboard_reporting);
    }
}

/// Required malformed cursor values and unsupported shapes reject; optional
/// nonboolean flags keep the legacy defaults instead of acquiring new semantics.
#[test]
fn wire_modes_reject_malformed_required_cursor_evidence() {
    for cursor in [
        serde_json::json!({"row":-1,"column":0,"visible":true}),
        serde_json::json!({"row":0,"column":0}),
        serde_json::json!({"row":0,"column":0,"visible":true,"style":"unknown"}),
    ] {
        assert!(parse_view_output_modes(&serde_json::json!({"cursor":cursor})).is_err());
    }
}
