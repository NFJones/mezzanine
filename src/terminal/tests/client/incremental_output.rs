//! Regression tests for terminal client incremental output behavior.

use crate::terminal::client_loop::{
    AttachedTerminalOutputFrameState, AttachedTerminalOutputModes,
    compose_terminal_output_style_spans, encode_attached_terminal_output_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_styles,
};
use crate::terminal::tests::fixtures::{display_column_for_fragment, styled_line_rendition_at};
use crate::terminal::{
    ClientViewRole, CopyPosition, RenderedClientView, Size, TerminalCursorStyle, TerminalScreen,
    UiTheme,
};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

/// Verifies that stable-size attached-terminal redraws are encoded as row
/// updates instead of clearing the full viewport. This reduces foreground TTY
/// flicker while still allowing the first draw and resizes to invalidate the
/// whole surface. Changed rows are already full-width, so the update must not
/// append erase-to-end-of-line after the row text because that can clear a
/// freshly drawn final-column cell while host autowrap is pending.
#[test]
fn attached_terminal_output_update_redraws_only_changed_rows() {
    let previous_lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["one    ".to_string(), "changed".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.starts_with("\x1b[?25l"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;1H\x1b[0mchanged"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[K"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
}

/// Verifies that same-width printable ASCII row changes can update only the
/// changed span instead of rewriting the whole row. This keeps frequent status
/// or prompt edits small on slower terminal links while preserving the existing
/// row-diff contract for unsafe text.
#[test]
fn attached_terminal_output_update_uses_changed_ascii_span_when_safe() {
    let previous_lines = vec!["aaaaaaaaaa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aaaabaaaaa".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;5H\x1b[0mb"), "{rendered:?}");
    assert!(!rendered.contains("aaaabaaaaa"), "{rendered:?}");
}

/// Verifies title-like frame edits can update several changed rows through
/// bounded row segments instead of repainting each full row. Window-group,
/// window, and pane title changes commonly touch a small set of frame rows, and
/// the default window bar includes non-ASCII action glyphs that previously
/// forced a full-row rewrite even when the changed title segment itself was
/// narrow.
#[test]
fn attached_terminal_output_update_uses_segment_updates_for_small_multi_row_title_changes() {
    let previous_lines = vec![
        "0 shell □ ⊕ λ".to_string(),
        "1 default".to_string(),
        "#1 shell".to_string(),
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &[
            "0 build □ ⊕ λ".to_string(),
            "1 staging".to_string(),
            "#1 build".to_string(),
        ],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;3H\x1b[0mbuild"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;3H\x1b[0mstaging"), "{rendered:?}");
    assert!(rendered.contains("\x1b[3;4H\x1b[0mbuild"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1H0 build"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[2;1H1 staging"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[3;1H#1 build"), "{rendered:?}");
}

/// Verifies large multi-row viewport changes still rewrite whole rows instead
/// of emitting many small segment updates. Scrollback paging and similar bulk
/// transitions change enough rows that self-contained full-row redraws remain
/// the better tradeoff.
#[test]
fn attached_terminal_output_update_rewrites_full_rows_for_many_row_changes() {
    let previous_lines = vec![
        "row 001".to_string(),
        "row 002".to_string(),
        "row 003".to_string(),
        "row 004".to_string(),
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &[
            "row 101".to_string(),
            "row 102".to_string(),
            "row 103".to_string(),
            "row 104".to_string(),
        ],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0mrow 101"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;1H\x1b[0mrow 102"), "{rendered:?}");
    assert!(rendered.contains("\x1b[3;1H\x1b[0mrow 103"), "{rendered:?}");
    assert!(rendered.contains("\x1b[4;1H\x1b[0mrow 104"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;5H1"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[2;5H1"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[3;5H1"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[4;5H1"), "{rendered:?}");
}

/// Verifies rows that change display width still keep the full-row rewrite
/// path. Cursor-column updates can only target a bounded segment when the old
/// and new text occupy the same visible columns.
#[test]
fn attached_terminal_output_update_rewrites_rows_when_glyph_width_changes() {
    let previous_lines = vec!["aa✔aa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aaXaa ".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0m"), "{rendered:?}");
    assert!(rendered.contains("aaXaa "), "{rendered:?}");
}

/// Verifies row-diff updates expand to cover trailing prompt padding when one
/// changed prompt segment overlaps a full-row background span.
///
/// Pasting multiline input can replace only the visible text on one wrapped
/// prompt row while preserving the same prompt background on trailing spaces.
/// The incremental row encoder must repaint those trailing padding cells so the
/// attached terminal does not leave them with stale default styling.
#[test]
fn attached_terminal_output_update_repaints_trailing_prompt_padding_after_text_change() {
    let previous_lines = vec!["      ".to_string()];
    let current_lines = vec!["alpha ".to_string()];
    let prompt_span = crate::terminal::TerminalStyleSpan {
        start: 0,
        length: 6,
        rendition: crate::terminal::GraphicRendition {
            foreground: Some(crate::terminal::TerminalColor::Rgb(255, 255, 255)),
            background: Some(crate::terminal::TerminalColor::Rgb(37, 40, 39)),
            ..crate::terminal::GraphicRendition::default()
        },
    };
    let previous_spans = vec![vec![prompt_span]];
    let current_spans = vec![vec![prompt_span]];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0m"), "{rendered:?}");
    assert!(rendered.contains("alpha "), "{rendered:?}");
}

/// Verifies wrapped agent-prompt continuation rows with full-row styling fall
/// back to a full-row rewrite instead of a bounded segment update.
///
/// History navigation can swap one wrapped continuation row for another while
/// keeping the changed text inside a narrow interior segment. The row-diff
/// path must still rewrite the whole row when prompt styling spans extend past
/// the changed text, or the continuation indentation and prompt background can
/// inherit stale cells from the prior render.
#[test]
fn attached_terminal_output_update_rewrites_fully_styled_prompt_continuation_rows() {
    let previous_lines = vec!["      alpha     ".to_string()];
    let current_lines = vec!["      omega     ".to_string()];
    let prompt_span = crate::terminal::TerminalStyleSpan {
        start: 0,
        length: 16,
        rendition: crate::terminal::GraphicRendition {
            foreground: Some(crate::terminal::TerminalColor::Rgb(255, 255, 255)),
            background: Some(crate::terminal::TerminalColor::Rgb(37, 40, 39)),
            ..crate::terminal::GraphicRendition::default()
        },
    };
    let previous_spans = vec![vec![prompt_span]];
    let current_spans = vec![vec![prompt_span]];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0m"), "{rendered:?}");
    assert!(rendered.contains("      omega     "), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;7H\x1b[0momega"), "{rendered:?}");
}

/// Verifies bounded row-segment updates keep selected-link styling off the
/// separator cell on a `/resume` picker row.
///
/// The live pager moves selection between saved-session rows without a full
/// redraw. When that happens, the row-differential encoder must preserve the
/// link foreground, underline, and active background on the first session-id
/// cell without shifting any of that styling one column left into the bullet
/// separator.
#[test]
fn attached_terminal_output_update_preserves_resume_picker_link_boundary() {
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let link_rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(230, 195, 132)),
        bold: true,
        underline: true,
        ..GraphicRendition::default()
    };
    let active_link_rendition = GraphicRendition {
        background: Some(TerminalColor::Rgb(122, 168, 159)),
        ..link_rendition
    };
    let previous_lines = vec!["> • latest".to_string(), format!("  • {session_id}")];
    let current_lines = vec!["  • latest".to_string(), format!("> • {session_id}")];
    let previous_spans = vec![
        vec![
            TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition: GraphicRendition::default(),
            },
            TerminalStyleSpan {
                start: 4,
                length: "latest".len(),
                rendition: active_link_rendition,
            },
        ],
        vec![TerminalStyleSpan {
            start: 4,
            length: session_id.len(),
            rendition: link_rendition,
        }],
    ];
    let current_spans = vec![
        vec![TerminalStyleSpan {
            start: 4,
            length: "latest".len(),
            rendition: link_rendition,
        }],
        vec![
            TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition: GraphicRendition::default(),
            },
            TerminalStyleSpan {
                start: 4,
                length: session_id.len(),
                rendition: active_link_rendition,
            },
        ],
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let initial_frame = encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
    );
    let update_frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        modes,
        Some(&previous),
    );
    let mut screen = TerminalScreen::new(Size::new(120, 2).unwrap(), 10).unwrap();
    screen.feed(&initial_frame);
    screen.feed(&update_frame);

    let styled_lines = screen.visible_styled_lines();
    let row = styled_lines
        .iter()
        .find(|line| line.text.contains(session_id))
        .unwrap();
    let start = display_column_for_fragment(&row.text, session_id);
    let previous_rendition = styled_line_rendition_at(row, start.saturating_sub(1));
    let first_rendition = styled_line_rendition_at(row, start);

    assert_ne!(
        previous_rendition.foreground,
        Some(TerminalColor::Rgb(230, 195, 132)),
        "resume picker link foreground shifted left after segment update: {styled_lines:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left after segment update: {styled_lines:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(TerminalColor::Rgb(122, 168, 159)),
        "resume picker active background shifted left after segment update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(TerminalColor::Rgb(230, 195, 132)),
        "resume picker first session-id cell lost link foreground after segment update: {styled_lines:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline after segment update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(TerminalColor::Rgb(122, 168, 159)),
        "resume picker first session-id cell lost active background after segment update: {styled_lines:?}"
    );
}

/// Verifies stable-row attached-terminal updates clear only rows that shrink
/// instead of falling back to a full-screen redraw. This avoids stale trailing
/// cells over remote terminal links while keeping the update bounded to the
/// changed row.
#[test]
fn attached_terminal_output_update_clears_shrinking_rows_without_full_redraw() {
    let previous_lines = vec!["wide text".to_string(), "steady".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["short".to_string(), "steady".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(
        rendered.contains("\x1b[1;1H\x1b[0m\x1b[2Kshort"),
        "{rendered:?}"
    );
    assert!(!rendered.contains("\x1b[2;1Hsteady"), "{rendered:?}");
}

/// Verifies stable-size attached-terminal updates avoid sending any bytes when
/// the rendered rows, style spans, bracketed-paste mode, and cursor
/// presentation are unchanged. This keeps idle status refreshes cheap over
/// higher-latency terminal links.
#[test]
fn attached_terminal_output_update_omits_unchanged_frame_bytes() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], modes);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        modes,
        Some(&previous),
    );

    assert!(frame.is_empty(), "{:?}", String::from_utf8_lossy(&frame));
}

/// Verifies alternate-screen exit forces a full attached-terminal redraw even
/// when the visible row count is unchanged. Exiting a fullscreen pane app can
/// restore the shell prompt beneath stale host rows unless the diff encoder
/// clears and repaints the composed normal-screen view in one frame.
#[test]
fn attached_terminal_output_update_full_redraws_on_alternate_screen_exit() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous_modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        alternate_screen: true,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], previous_modes);
    let next_modes = AttachedTerminalOutputModes {
        alternate_screen: false,
        ..previous_modes
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[?1049h"), "{rendered:?}");
    assert!(rendered.contains("\x1b[?1049l"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2J\x1b[H"), "{rendered:?}");
    assert!(rendered.contains("one    "), "{rendered:?}");
    assert!(rendered.contains("two    "), "{rendered:?}");
}

/// Verifies stable-size attached-terminal updates emit only cursor bytes when
/// the visible content is unchanged and the cursor moves. Row-differential
/// updates should resend coordinate-state presentation setup before cursor
/// addressing, but not clear or repaint static content.
#[test]
fn attached_terminal_output_update_uses_cursor_only_frame_for_cursor_moves() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous_modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], previous_modes);
    let next_modes = AttachedTerminalOutputModes {
        cursor_column: 1,
        ..previous_modes
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[?2004"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
    assert_eq!(
        rendered,
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?25l\x1b[0m\x1b[2 q\x1b[1;2H\x1b[?25h"
    );
}

/// Verifies stable-size attached-terminal updates emit bracketed-paste mode
/// changes without resending the rest of the static presentation prologue.
#[test]
fn attached_terminal_output_update_emits_only_changed_bracketed_paste_mode() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new_with_modes(
        &lines,
        &[],
        AttachedTerminalOutputModes::default(),
    );
    let next_modes = AttachedTerminalOutputModes {
        bracketed_paste: true,
        ..AttachedTerminalOutputModes::default()
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert_eq!(rendered, "\x1b[?2004h");
}

/// Verifies that attached-terminal row-diff updates keep styling on the final
/// changed cell when a full-word style span reaches the row end. Segment-only
/// updates are the most likely place for an off-by-one to leave the trailing
/// glyph plain even though the line-level span length is correct.
#[test]
fn attached_terminal_output_update_keeps_style_on_final_changed_character() {
    let previous_lines = vec!["gone".to_string()];
    let previous_spans = vec![Vec::new()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 4,
        rendition: GraphicRendition {
            foreground: Some(TerminalColor::Indexed(4)),
            ..GraphicRendition::default()
        },
    }]];

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["blue".to_string()],
        &spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.contains("\x1b[1;1H\x1b[0m\x1b[0;34mblue"),
        "{rendered:?}"
    );
    assert!(
        !rendered.contains("\x1b[1;1H\x1b[0m\x1b[0;34mblue\x1b[0m"),
        "{rendered:?}"
    );
}

/// Verifies terminal output styling drops styles for matching rendered slices.
///
/// Partial text matches are not a safe ownership proof because unrelated action
/// output can match rows already visible in the rendered presentation. This
/// regression keeps render-owned styles from leaking onto bounded writes.
#[test]
fn terminal_output_style_spans_drop_styles_for_matching_row_slice() {
    let rendered_view = RenderedClientView {
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
    let output_lines = vec!["beta match".to_string()];
    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));
    assert!(style_spans.is_empty(), "{style_spans:?}");
}
