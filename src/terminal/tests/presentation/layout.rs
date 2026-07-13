//! Regression tests for terminal presentation layout behavior.

use crate::ids::IdFactory;
use crate::layout::{PaneGeometry, SplitDirection};
use crate::terminal::tests::fixtures::display_column_for_fragment;
use crate::terminal::{
    BTreeMap, ClientViewRole, DEFAULT_PANE_FRAME_TEMPLATE,
    DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE, DEFAULT_WINDOW_FRAME_TEMPLATE, PaneRenderInput,
    RenderedClientView, Size, TerminalClientLoopConfig, TerminalCursorStyle, TerminalFrameContext,
    TerminalFrameRenderOptions, TerminalPaneFrameContext, TerminalScreen, UiTheme, Window,
    apply_client_view_offset, compose_client_presentation, draw_window_from_screens,
    pane_render_region_size_for_geometry, render_attached_client_view, render_window,
    render_window_with_pane_frame_template,
};
use mez_mux::presentation::{
    TerminalFramePosition, TerminalWindowFrameContext, TerminalWindowGroupFrameContext,
    TerminalWindowStatusContext,
};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};
use unicode_width::UnicodeWidthStr;

pub(super) fn window_from_test_geometries(size: Size, geometries: Vec<PaneGeometry>) -> Window {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", size);
    while window.panes().len() < geometries.len() {
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
    }
    window.replace_pane_geometries(geometries).unwrap();
    window
}

/// Returns blank render inputs for every pane in a test window.
///
/// # Parameters
/// - `window`: The window whose pane IDs should be covered.
fn blank_inputs_for_window(window: &Window) -> Vec<PaneRenderInput> {
    window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![String::new()],
        })
        .collect()
}

/// Verifies client loop draws window from live pane screens.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_draws_window_from_live_pane_screens() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    window
        .split_active(&mut ids, crate::layout::SplitDirection::Vertical)
        .unwrap();
    let mut screens = BTreeMap::new();
    let body_size = |size: Size| Size::new(size.columns, size.rows - 1).unwrap();
    let mut left = TerminalScreen::new(body_size(window.panes()[0].size), 10).unwrap();
    left.feed(b"left");
    let mut right = TerminalScreen::new(body_size(window.panes()[1].size), 10).unwrap();
    right.feed(b"right");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let rendered = draw_window_from_screens(&window, &screens, &config).unwrap();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 4);
    assert!(joined.contains("left"));
    assert!(joined.contains("right"));
}

/// Verifies left panes reserve the shared divider column when an even vertical
/// split creates a right divider neighbor.
///
/// This regression covers the selected-agent prompt bug directly at the
/// render-region sizing boundary so later render changes cannot let content
/// overwrite the right-side divider.
#[test]
fn pane_render_region_reserves_right_divider_for_even_vertical_split() {
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 5,
            rows: 3,
        },
        PaneGeometry {
            index: 1,
            column: 5,
            row: 0,
            columns: 5,
            rows: 3,
        },
    ];

    assert_eq!(
        pane_render_region_size_for_geometry(&geometries[0], &geometries).unwrap(),
        Size::new(4, 3).unwrap()
    );
}

/// Verifies left panes reserve the shared divider column when an odd vertical
/// split leaves the left pane one column wider than its neighbor.
///
/// This regression protects the off-by-one case called out in the fix plan so
/// uneven split math cannot let agent-prompt text overwrite the divider.
#[test]
fn pane_render_region_reserves_right_divider_for_odd_vertical_split() {
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 6,
            rows: 3,
        },
        PaneGeometry {
            index: 1,
            column: 6,
            row: 0,
            columns: 5,
            rows: 3,
        },
    ];

    assert_eq!(
        pane_render_region_size_for_geometry(&geometries[0], &geometries).unwrap(),
        Size::new(5, 3).unwrap()
    );
}

/// Verifies that rendered client views carry visible screen SGR spans beside
/// their plain text lines. This keeps terminal/view consumers from needing
/// private screen access to observe colors and attributes.
#[test]
fn client_view_preserves_terminal_style_spans() {
    let mut ids = crate::ids::IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[1;38;5;120mAB\x1b[0mC");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(8, 2).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], "ABC     ");
    assert_eq!(
        view.line_style_spans[0],
        vec![TerminalStyleSpan {
            start: 0,
            length: 2,
            rendition: GraphicRendition {
                bold: true,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(120)),
                background: None,
            },
        }]
    );
}

/// Verifies that a terminal style run covering the full visible word keeps the
/// final character inside the same span. The user-visible report here was that
/// a fully colored word rendered with its trailing character unstyled, so this
/// exercises the screen-to-client-view path with a span that reaches the end of
/// the visible text.
#[test]
fn client_view_keeps_full_word_style_span_through_final_character() {
    let mut ids = crate::ids::IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[34mblue\x1b[0m");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(8, 2).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], "blue    ");
    assert_eq!(
        view.line_style_spans[0],
        vec![TerminalStyleSpan {
            start: 0,
            length: 4,
            rendition: GraphicRendition {
                foreground: Some(TerminalColor::Indexed(4)),
                ..GraphicRendition::default()
            },
        }]
    );
}

/// Verifies that side-by-side rendering offsets style spans by each pane's
/// rendered width, so styled content from a later pane points at the correct
/// terminal-cell columns in the composed client view.
#[test]
fn client_view_offsets_style_spans_across_side_by_side_panes() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    window
        .split_active(&mut ids, crate::layout::SplitDirection::Vertical)
        .unwrap();
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed(b"L");
    let mut right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    right.feed(b"\x1b[7mR\x1b[0m");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(8, 2).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], "L  \u{2502}R   ");
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 4
            && span.length == 1
            && span.rendition
                == GraphicRendition {
                    bold: false,
                    dim: false,
                    italic: false,
                    strikethrough: false,
                    double_underline: false,
                    hidden: false,
                    underline: false,
                    inverse: true,
                    foreground: None,
                    background: None,
                }
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 3
            && span.length == 1
            && span.rendition.foreground == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
            && span.rendition.background.is_none()
    }));
}

/// Verifies client view hides pending observers and keeps primary dimensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_view_hides_pending_observers_and_keeps_primary_dimensions() {
    let mut ids = crate::ids::IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"live\nviewport");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig::default();

    let pending = render_attached_client_view(
        ClientViewRole::PendingObserver,
        &window,
        &screens,
        &config,
        Size::new(10, 2).unwrap(),
    )
    .unwrap();
    let observer = render_attached_client_view(
        ClientViewRole::Observer,
        &window,
        &screens,
        &config,
        Size::new(10, 2).unwrap(),
    )
    .unwrap()
    .unwrap();
    let primary = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(20, 4).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert!(pending.is_none());
    assert_eq!(observer.authoritative_size, Size::new(20, 4).unwrap());
    assert_eq!(observer.client_size, Size::new(10, 2).unwrap());
    assert!(observer.requires_client_scroll);
    assert_eq!(observer.lines.len(), 4);
    assert!(observer.lines.join("\n").contains("live"));
    assert!(!primary.requires_client_scroll);
}

/// Verifies observer client presentation uses local viewport offset.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_client_presentation_uses_local_viewport_offset() {
    let mut view = RenderedClientView {
        role: ClientViewRole::Observer,
        authoritative_size: Size::new(8, 4).unwrap(),
        client_size: Size::new(4, 2).unwrap(),
        lines: vec![
            "abcd1234".to_string(),
            "efgh5678".to_string(),
            "ijkl9012".to_string(),
            "mnop3456".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: true,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: false,
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

    apply_client_view_offset(&mut view, 2, 4);
    assert_eq!(
        compose_client_presentation(&view, None),
        vec!["9012".to_string(), "3456".to_string()]
    );
    apply_client_view_offset(&mut view, 99, 99);
    assert_eq!(view.viewport_row, 2);
    assert_eq!(view.viewport_column, 4);
}

/// Verifies that the built-in attached-terminal render configuration presents
/// visible window and pane state by default instead of launching into an
/// unframed, state-free viewport.
#[test]
fn default_client_loop_config_renders_window_and_pane_state_rows() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[0].contains("0 shell"), "{:?}", view.lines);
    assert!(view.lines[3].contains("main"), "{:?}", view.lines);
    assert!(view.line_style_spans[3].iter().any(|span| {
        span.start == 0
            && span.length == usize::from(window.size.columns)
            && span.rendition.background == Some(TerminalColor::Rgb(0x1f, 0x1f, 0x28))
    }));
    assert!(
        view.line_style_spans[3]
            .iter()
            .any(|span| span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8)))
    );
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 0
            && span.length == usize::from(window.size.columns)
            && span.rendition.background == Some(TerminalColor::Rgb(0x1f, 0x1f, 0x28))
    }));
    assert!(
        view.line_style_spans[0]
            .iter()
            .any(|span| span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f)))
    );
    assert!(view.cursor_visible);
    assert_eq!(view.cursor_row, 1);
    assert_eq!(view.cursor_column, 0);
}

/// Verifies that attached-client rendering honors pane applications that hide
/// the terminal cursor, including alternate-screen full-screen TUIs.
#[test]
fn attached_client_view_hides_cursor_when_pane_screen_hides_cursor() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(24, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[?1049h\x1b[?25lhtop");
    let screens = BTreeMap::from([(pane_id, screen)]);

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(!view.cursor_visible);
}

/// Verifies that attached-terminal cursor composition treats pane titles merged
/// into horizontal dividers as divider content rather than as an extra row in
/// the bottom pane. This protects over/under splits from reporting the active
/// bottom pane cursor one terminal row below the PTY cursor position.
#[test]
fn attached_client_view_places_bottom_split_cursor_below_merged_divider_title() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(24, 5).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(window.active_pane_index(), 1);
    let merged_title_row = view
        .lines
        .iter()
        .position(|line| line.starts_with(" 1 shell"))
        .unwrap();
    assert_eq!(view.cursor_row, merged_title_row + 1);
    assert_eq!(view.cursor_column, 0);
}

/// Verifies attached-client cursor clamping stops before a shared right divider.
///
/// A pane's rightmost shared divider cell belongs to the mux frame, not the
/// pane content region. Cursor placement must therefore clamp before that cell
/// so pane-local UI cannot overwrite the divider.
#[test]
fn attached_client_view_clamps_cursor_before_right_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();

    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed(b"abcde");
    let right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    let screens = BTreeMap::from([
        (window.panes()[0].id.to_string(), left),
        (window.panes()[1].id.to_string(), right),
    ]);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.cursor_row, 0);
    assert_eq!(view.cursor_column, 3);
}

/// Verifies render window composes vertical split side by side.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_window_composes_vertical_split_side_by_side() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(rendered.len(), 3);
    assert_eq!(rendered[0], "pane\u{2502}pane1");
}

/// Verifies wide glyphs in pane content do not shift divider placement.
///
/// Pane composition is cell based. A double-width glyph immediately before a
/// divider must occupy its own cells without causing the final rendered string
/// to carry an extra filler cell that would push the border right.
#[test]
fn render_window_keeps_divider_fixed_after_wide_glyph() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["ab✅".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "ab✅\u{2502}right");
}

/// Verifies a wide glyph cannot overlap a divider and shift the pane to the
/// right of it.
///
/// If the continuation half of a wide glyph is overwritten by a divider, the
/// leading glyph cell must be cleared too. Otherwise the collected output
/// string still advances the terminal by two cells and pushes the neighboring
/// pane one column right on that row.
#[test]
fn render_window_clips_wide_glyph_that_overlaps_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["abc✅".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "abc \u{2502}right");
}

/// Verifies emoji-presentation warning signs keep their two-cell width before a
/// pane divider.
///
/// This protects the render path for grapheme clusters such as `⚠️`, where the
/// leading scalar alone is one cell but the rendered grapheme is two cells.
/// Dropping the variation selector during pane composition makes the divider
/// appear one column too far left on affected rows.
#[test]
fn render_window_keeps_divider_fixed_after_warning_sign_grapheme() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["ab⚠️".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "ab⚠️\u{2502}right");
}

/// Verifies a warning-sign grapheme clipped by a pane divider clears the full
/// wide-cell footprint.
///
/// When the divider overwrites the continuation half of `⚠️`, the leading
/// scalar must be cleared too. Otherwise only rows containing that grapheme
/// report a mismatched terminal width and the adjacent pane appears shifted.
#[test]
fn render_window_clips_warning_sign_grapheme_that_overlaps_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["abc⚠️".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "abc \u{2502}right");
}

/// Verifies render window composes horizontal split stacked.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_window_composes_horizontal_split_stacked() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(12, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window(&window, &inputs, true).unwrap();

    assert_eq!(rendered.len(), 4);
    assert!(
        rendered[0].contains("0 shell") || rendered[0].starts_with("0 shell"),
        "unexpected pane frame: {}",
        rendered[0]
    );
    assert!(
        rendered[1].contains("1 shell"),
        "unexpected pane frame: {}",
        rendered[1]
    );
    assert_eq!(rendered[2], "pane1       ");
    assert!(rendered[3].trim().is_empty());
}

/// Verifies that horizontal split dividers remain visible when pane frame rows
/// are enabled and that pane body content is clipped to the rows left after the
/// frame and divider reservations.
#[test]
fn render_window_reserves_horizontal_divider_above_next_pane_header() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(12, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec![
                "old-top".to_string(),
                "visible-top".to_string(),
                "overflow-top".to_string(),
            ],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["bottom".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, true).unwrap();

    assert_eq!(rendered.len(), 6);
    assert!(
        rendered[0].contains("0 shell") || rendered[0].starts_with("0 shell"),
        "unexpected pane frame: {}",
        rendered[0]
    );
    assert_eq!(rendered[1], "overflow-top");
    assert!(
        rendered[2].contains("1 shell") || rendered[2].starts_with("1 shell"),
        "unexpected pane frame: {}",
        rendered[2]
    );
    assert_eq!(rendered[2], " 1 shell ───");
    assert_eq!(rendered[3], "bottom      ");
}

/// Verifies that rendering uses the window's stored pane rectangles instead of
/// reducing layout to a side-by-side-or-stacked choice. The right pane is split
/// horizontally, so the left pane must remain visible across the full height
/// while the two right panes occupy only their stored upper and lower halves and
/// the adjacent divider junction is rendered as a connected tee.
#[test]
fn render_window_composes_irregular_layout_from_stored_geometry() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec![
                "L0".to_string(),
                "L1".to_string(),
                "L2".to_string(),
                "L3".to_string(),
            ],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["T0".to_string(), "T1".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[2].id.to_string(),
            lines: vec!["B0".to_string(), "B1".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(
        rendered,
        vec![
            "L0  \u{2502}T1   ".to_string(),
            "L1  \u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}".to_string(),
            "L2  \u{2502}B0   ".to_string(),
            "L3  \u{2502}B1   ".to_string(),
        ],
    );
}

/// Verifies that a horizontal split ending at the vertical divider from a
/// neighboring side-by-side pane uses a connected box-drawing tee rather than an
/// ASCII fallback. This is the overlapping junction shape that previously
/// produced `+` when the left pane was split horizontally.
#[test]
fn render_window_connects_overlapped_mixed_split_divider_junction() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec!["TL0".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["BL0".to_string(), "BL1".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[2].id.to_string(),
            lines: vec![
                "R0".to_string(),
                "R1".to_string(),
                "R2".to_string(),
                "R3".to_string(),
            ],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(
        rendered,
        vec![
            "TL0 \u{2502}R0   ".to_string(),
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}R1   ".to_string(),
            "BL0 \u{2502}R2   ".to_string(),
            "BL1 \u{2502}R3   ".to_string(),
        ],
    );
}

/// Verifies rendered irregular pane layouts compose every mixed split junction
/// as connected Unicode box drawing rather than ASCII fallback characters.
#[test]
fn render_window_connects_all_mixed_split_junction_shapes() {
    let size = Size::new(24, 12).unwrap();
    let cases = [
        (
            '\u{253c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 3,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{252c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 24,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{2534}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 0,
                    row: 6,
                    columns: 24,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{251c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 12,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{2524}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 12,
                },
            ],
        ),
    ];

    for (expected, geometries) in cases {
        let window = window_from_test_geometries(size, geometries);
        let inputs = blank_inputs_for_window(&window);
        let rendered = render_window(&window, &inputs, false).unwrap();

        assert_eq!(
            rendered[5].chars().nth(11),
            Some(expected),
            "unexpected junction in layout:\n{}",
            rendered.join("\n")
        );
    }
}

/// Verifies context usage has its own derived scale instead of borrowing the
/// agent-state blocked color. Context pressure is related to compaction, not the
/// current scheduler state, so the two pills should not collapse visually.
#[test]
fn render_context_usage_uses_distinct_pill_background() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_context_usage: Some("87%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let context_start = display_column_for_fragment(&view.lines[0], "87%");
    let context_background = view.line_style_spans[0]
        .iter()
        .find(|span| {
            span.start <= context_start && span.start.saturating_add(span.length) > context_start
        })
        .and_then(|span| span.rendition.background)
        .unwrap();

    assert_ne!(
        context_background,
        config.ui_theme.colors.agent_status_blocked.background
    );
    assert_ne!(
        context_background,
        config.ui_theme.colors.agent_status_running.background
    );
}

/// Verifies that group frame rendering appears only for multiple groups.
///
/// The group bar is a conditional top bar, so a default single-group session
/// must keep the full terminal height for the window while a multi-group
/// session reserves one top row with styled, mouse-addressable group pills.
#[test]
fn render_attached_view_uses_conditional_window_group_bar() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "shell", Size::new(40, 4).unwrap());
    let single_group_config = TerminalClientLoopConfig {
        frame_context: TerminalFrameContext {
            groups: vec![TerminalWindowGroupFrameContext {
                id: "g1".to_string(),
                index: 0,
                title: "default".to_string(),
                active: true,
            }],
            ..TerminalFrameContext::default()
        },
        ..TerminalClientLoopConfig::default()
    };

    let single_group_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &single_group_config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(single_group_view.lines.len(), 4);
    assert!(
        !single_group_view.lines[0].contains("default"),
        "single-group sessions should not reserve the top group bar"
    );

    let multi_group_config = TerminalClientLoopConfig {
        frame_context: TerminalFrameContext {
            groups: vec![
                TerminalWindowGroupFrameContext {
                    id: "g1".to_string(),
                    index: 0,
                    title: "default".to_string(),
                    active: false,
                },
                TerminalWindowGroupFrameContext {
                    id: "g2".to_string(),
                    index: 1,
                    title: "work".to_string(),
                    active: true,
                },
            ],
            ..TerminalFrameContext::default()
        },
        ..TerminalClientLoopConfig::default()
    };

    let multi_group_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &multi_group_config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(multi_group_view.lines.len(), 4);
    assert!(multi_group_view.lines[0].contains("0 default"));
    assert!(multi_group_view.lines[0].contains("1 work"));
    assert!(
        multi_group_view.line_style_spans[0].iter().any(|span| {
            span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
        })
    );
}

/// Verifies that the pane working-directory field used by window status and
/// explicit pane frame templates is compacted to the final three path segments.
///
/// The default window footer places `pane.pwd` at the left edge of the
/// right-status region, so deep project paths must not crowd out command pills
/// and clock fields. Explicit pane templates share the same named field and
/// must keep the same display contract for scrollback-aware pane frames.
#[test]
fn render_pane_pwd_fields_compact_deep_paths_to_three_segments() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(120, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let frame_context = TerminalFrameContext {
        windows: vec![TerminalWindowFrameContext {
            id: "@2".to_string(),
            index: 1,
            title: "work".to_string(),
            active: true,
            subagent: false,
        }],
        panes: BTreeMap::from([(
            pane_id,
            TerminalPaneFrameContext {
                current_working_directory: Some("/var/tmp/a/b/c/d".to_string()),
                ..TerminalPaneFrameContext::default()
            },
        )]),
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/Documents/a/b/c/d".to_string()),
            status_pills: BTreeMap::new(),
            system_uptime: "2d 03h 04m".to_string(),
            datetime_local: "2026-05-05 10:11:12".to_string(),
        }),
        ..TerminalFrameContext::default()
    };
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Bottom,
        ),
        TerminalFrameRenderOptions::plain(true, "#{pane.pwd}", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "…/b/c/d");
    assert!(rendered[2].contains(" …/b/c/d "), "{}", rendered[2]);
    assert!(!rendered[2].contains("~/Documents/a"), "{}", rendered[2]);
}

/// Verifies that after text wraps and bash sends its backspace erasure
/// sequences, the rendered output reflects the erased characters. This
/// exercises the full screen-update + render path.
#[test]
fn render_output_reflects_wrapped_text_erasure() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    let mut screen = TerminalScreen::new(Size::new(10, 1).unwrap(), 10).unwrap();
    screen.feed(b"hello");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(view.lines, vec!["hello     ", "          ", "          "]);

    let screen = screens.get_mut(window.active_pane().id.as_str()).unwrap();
    screen.feed(b"\x08 \x08");
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        view.lines,
        vec!["hell      ", "          ", "          "],
        "backspace+space should erase last char"
    );
}

/// Verifies rendering after backspace erases a wrapped character via
/// explicit CSI sequences (cursor back, delete char).
#[test]
fn render_output_reflects_wrapped_csi_erasure() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(5, 3).unwrap());
    let mut screen = TerminalScreen::new(Size::new(5, 3).unwrap(), 10).unwrap();
    screen.feed(b"abcde");
    screen.feed(b"f");
    assert_eq!(screen.visible_lines()[0], "abcde");
    assert!(screen.visible_lines()[1].starts_with('f'));

    screen.feed(b"\x1b[D\x1b[P");
    assert!(
        screen.visible_lines()[1].is_empty(),
        "row 1 should be empty after DCH: {:?}",
        screen.visible_lines()
    );

    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let joined = view.lines.join("\n");
    assert!(
        !joined.contains('f'),
        "erased 'f' should not appear in rendered output:\n{joined}"
    );
}
