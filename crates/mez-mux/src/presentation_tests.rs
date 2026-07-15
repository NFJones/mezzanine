//! Direct regression tests for mux-owned presentation boundaries.

use crate::layout::Size;
use crate::presentation::{
    ClientStatusKind, ClientStatusLine, ClientViewRole, RenderedClientView, TerminalCursorStyle,
    compose_client_presentation, pane_divider_glyph,
};
use crate::theme::UiTheme;

/// Verifies client presentation renders the status line inside the authoritative size.
///
/// Status composition belongs to mux presentation rather than the one-surface
/// terminal compatibility engine, so this regression remains product-owned.
#[test]
fn client_presentation_renders_status_line_inside_authoritative_size() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alternate_screen: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };

    let lines = compose_client_presentation(
        &view,
        Some(&ClientStatusLine {
            kind: ClientStatusKind::CopyMode,
            text: "select".to_string(),
        }),
    );

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "one");
    assert_eq!(lines[2], "copy: select");
}

/// Verifies every mux-managed divider mask maps to the expected box-drawing glyph.
///
/// Divider connectivity belongs to multi-pane presentation and must not move
/// into the one-surface terminal compatibility crate with parser tests.
#[test]
fn pane_divider_connection_masks_use_correct_box_drawing_glyphs() {
    let cases = [
        ((true, true, false, false), '\u{2502}'),
        ((false, false, true, true), '\u{2500}'),
        ((false, true, false, true), '\u{250c}'),
        ((false, true, true, false), '\u{2510}'),
        ((true, false, false, true), '\u{2514}'),
        ((true, false, true, false), '\u{2518}'),
        ((false, true, true, true), '\u{252c}'),
        ((true, false, true, true), '\u{2534}'),
        ((true, true, false, true), '\u{251c}'),
        ((true, true, true, false), '\u{2524}'),
        ((true, true, true, true), '\u{253c}'),
    ];

    for ((up, down, left, right), expected) in cases {
        assert_eq!(
            pane_divider_glyph(up, down, left, right),
            expected,
            "unexpected glyph for up={up} down={down} left={left} right={right}"
        );
    }
}
