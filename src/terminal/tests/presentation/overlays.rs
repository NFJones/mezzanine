//! Regression tests for terminal presentation overlays behavior.

use crate::terminal::{
    compose_display_overlay_line_style_spans, compose_display_region_overlay_line_style_spans,
    compose_display_region_overlay_lines, compose_modal_display_overlay_line_style_spans,
    compose_modal_display_overlay_lines,
};
use mez_mux::copy::CopyPosition;
use mez_mux::layout::Size;
use mez_mux::presentation::{
    ClientViewRole, ReadlinePromptRegion, RenderedClientView, TerminalCursorStyle,
    compose_client_presentation_with_styles,
};
use mez_mux::render::{compose_bottom_overlay_lines, modal_overlay_max_scroll};
use mez_mux::theme::UiTheme;
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

/// Verifies client presentation highlights the submitted pager search match.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_presentation_highlights_current_pager_search_match() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(16, 3).unwrap(),
        client_size: Size::new(16, 3).unwrap(),
        lines: vec![
            "alpha".to_string(),
            "beta match".to_string(),
            "gamma".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        selection: Some((
            CopyPosition { line: 1, column: 5 },
            CopyPosition {
                line: 1,
                column: 10,
            },
        )),
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

    let (lines, line_style_spans) = compose_client_presentation_with_styles(&view, None);

    assert_eq!(lines[1], "beta match");
    assert_eq!(line_style_spans[1].len(), 1);
    assert_eq!(line_style_spans[1][0].start, 5);
    assert_eq!(line_style_spans[1][0].length, 5);
    assert_eq!(
        line_style_spans[1][0].rendition,
        UiTheme::default().colors.copy_selection.rendition()
    );
}

/// Verifies display overlay refits base lines to current size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that overlays normalize retained base rows to the current terminal
/// size. This prevents stale pre-resize frames from leaving long rows behind
/// when the attached terminal shrinks during a prompt or command display.
fn display_overlay_refits_base_lines_to_current_size() {
    let lines = compose_bottom_overlay_lines(
        &["abcdefghijklmnopqrstuvwxyz".to_string()],
        &["ok".to_string()],
        Size::new(10, 3).unwrap(),
    );

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "abcdefghij");
    assert_eq!(lines[1], "          ");
    assert_eq!(lines[2], "ok        ");
    assert!(lines.iter().all(|line| line.chars().count() == 10));
}

/// Verifies that display overlays keep style spans for retained base rows,
/// clip them to the current terminal width, and clear styles on rows replaced
/// by Mezzanine-owned display text.
#[test]
fn display_overlay_preserves_and_refits_retained_base_styles() {
    let spans = compose_display_overlay_line_style_spans(
        &[vec![TerminalStyleSpan {
            start: 0,
            length: 26,
            rendition: GraphicRendition {
                foreground: Some(TerminalColor::Indexed(2)),
                ..GraphicRendition::default()
            },
        }]],
        &["ok".to_string()],
        Size::new(10, 3).unwrap(),
        &UiTheme::default(),
    );

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].len(), 1);
    assert_eq!(spans[0][0].length, 10);
    assert_eq!(
        spans[0][0].rendition.foreground,
        Some(TerminalColor::Indexed(2))
    );
    assert!(spans[1].is_empty());
    assert_eq!(spans[2].len(), 1);
    assert_eq!(spans[2][0].start, 0);
    assert_eq!(spans[2][0].length, 2);
    assert_eq!(
        spans[2][0].rendition.foreground,
        Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
    );
    assert_eq!(spans[2][0].rendition.background, None);
}

/// Verifies that command display overlays can target the active pane's client
/// region instead of replacing the whole terminal frame. This keeps `help` and
/// other command output visually local to the pane that invoked the command.
#[test]
fn display_region_overlay_renders_output_inside_requested_pane_region() {
    let region = ReadlinePromptRegion {
        row: 0,
        column: 2,
        columns: 8,
        rows: 3,
    };
    let base = vec![
        "............".to_string(),
        "............".to_string(),
        "............".to_string(),
        "............".to_string(),
    ];
    let display = vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ];

    let lines =
        compose_display_region_overlay_lines(&base, &display, Size::new(12, 4).unwrap(), region);
    let spans = compose_display_region_overlay_line_style_spans(
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &display,
        Size::new(12, 4).unwrap(),
        region,
        &UiTheme::default(),
    );

    assert_eq!(lines[0], "..second  ..");
    assert_eq!(lines[1], "..third   ..");
    assert_eq!(lines[2], "............");
    assert_eq!(spans[0][0].start, 2);
    assert_eq!(spans[0][0].length, 6);
    assert_eq!(spans[1][0].start, 2);
    assert_eq!(spans[1][0].length, 5);
    assert_eq!(spans[0][0].rendition.background, None);
    assert_eq!(spans[1][0].rendition.background, None);
    assert!(spans[2].is_empty());
}

/// Verifies that modal command display overlays fill the entire terminal
/// window and expose an explicit Escape affordance. Long output is pageable by
/// scroll offset instead of disappearing on the next terminal redraw.
#[test]
fn modal_display_overlay_covers_terminal_and_pages_output() {
    let display = vec![
        "line one".to_string(),
        "line two".to_string(),
        "line three".to_string(),
        "line four".to_string(),
    ];

    let lines = compose_modal_display_overlay_lines(&display, Size::new(24, 4).unwrap(), 1);
    let spans = compose_modal_display_overlay_line_style_spans(
        &display,
        Size::new(24, 4).unwrap(),
        1,
        &UiTheme::default(),
    );

    assert_eq!(
        modal_overlay_max_scroll(display.len(), Size::new(24, 4).unwrap()),
        2
    );
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "mezzanine command output");
    assert_eq!(lines[1], "line two                ");
    assert_eq!(lines[2], "line three              ");
    assert!(lines[3].contains("esc: return"));
    assert_eq!(spans.len(), 4);
    assert_eq!(spans[0][0].start, 0);
    assert_eq!(spans[0][0].length, "mezzanine command output".len());
    assert_eq!(spans[1][0].length, "line two".len());
    assert_eq!(spans[2][0].length, "line three".len());
    assert_eq!(spans[3][0].start, 0);
    assert_eq!(spans[3][0].rendition.background, None);
}
