//! Regression tests for terminal presentation agent prompt behavior.

use crate::terminal::tests::fixtures::{
    display_column_for_fragment, test_color_is_grayscale, test_contrast_ratio,
};
use crate::terminal::{
    BTreeMap, TerminalClientLoopConfig, TerminalFrameContext, TerminalPaneFrameContext,
    compose_prompt_overlay_presentation, compose_prompt_overlay_presentation_with_styles,
    compose_prompt_region_presentation_with_styles, compose_readline_prompt_client_presentation,
    render_attached_client_view, render_readline_prompt_status_row,
};
use mez_core::ids::IdFactory;
use mez_mux::layout::{Size, SplitDirection, Window};
use mez_mux::presentation::{
    ClientStatusKind, ClientStatusLine, ClientViewRole, ReadlinePromptRegion, RenderedClientView,
    TerminalCursorStyle,
};
use mez_mux::theme::{UiTheme, builtin_ui_theme_definition, resolve_ui_theme};
use mez_terminal::TerminalScreen;
use mez_terminal::{TerminalColor, active_terminal_text_width};
use unicode_width::UnicodeWidthStr;

/// Verifies pane-local agent prompt rendering preserves the right divider when
/// the selected agent pane is on the left side of a vertical split.
///
/// This protects the selected agent shell prompt from drawing its text or
/// prompt background into the mux-managed border cell.
#[test]
fn render_attached_client_view_keeps_agent_prompt_before_right_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();
    let left_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("go");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        left_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frames_enabled: false,
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

    let divider_column = window.panes()[0].size.columns.saturating_sub(1) as usize;
    let prompt_row = view
        .lines
        .iter()
        .position(|line| line.contains("mez>"))
        .expect("left agent prompt should be visible");
    assert_eq!(
        view.lines[prompt_row].chars().nth(divider_column),
        Some('│'),
        "{}",
        view.lines[prompt_row]
    );
    assert!(
        view.line_style_spans[prompt_row].iter().all(|span| {
            span.start >= divider_column || span.start.saturating_add(span.length) <= divider_column
        }),
        "{:?}",
        view.line_style_spans[prompt_row]
    );
}

/// Verifies readline prompt status row renders prompt and cursor column.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_status_row_renders_prompt_and_cursor_column() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("run");
    assert!(prompt.buffer.move_left());

    let row = render_readline_prompt_status_row(&prompt, 12);

    assert_eq!(
        row.status,
        ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: "▐ mez> run  ".to_string(),
        }
    );
    assert_eq!(row.cursor_column, 9);
    assert!(row.cursor_visible);
}

/// Verifies readline prompt status row reports truncated cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_status_row_reports_truncated_cursor() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("very-long-command");

    let row = render_readline_prompt_status_row(&prompt, 8);

    assert_eq!(row.status.text, "▐ :very-");
    assert_eq!(row.cursor_column, 7);
    assert!(!row.cursor_visible);
}

/// Verifies readline prompt client presentation places prompt on status row.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_client_presentation_places_prompt_on_status_row() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(14, 3).unwrap(),
        client_size: Size::new(14, 3).unwrap(),
        lines: vec!["pane".to_string(), "body".to_string(), "old".to_string()],
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
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("rename");

    let presentation = compose_readline_prompt_client_presentation(&view, &prompt);

    assert_eq!(presentation.lines.len(), 3);
    assert_eq!(presentation.lines[0], "pane");
    assert_eq!(presentation.lines[2], "▐ :rename-wind");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 9);
    assert!(presentation.cursor_visible);
}

/// Verifies agent prompt row styling uses terminal display width instead of
/// Unicode scalar count when the rendered prompt contains a wide glyph.
///
/// A double-width glyph on the prompt row still occupies two terminal cells.
/// The full-row prompt style span must therefore cover the fitted display
/// width, or the trailing cell after the wide glyph renders with the wrong
/// background.
#[test]
fn readline_prompt_client_presentation_styles_agent_prompt_by_display_width() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec!["pane".to_string(), "body".to_string(), "old".to_string()],
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
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("界x");

    let presentation = compose_readline_prompt_client_presentation(&view, &prompt);

    assert_eq!(presentation.lines.len(), 3);
    assert_eq!(active_terminal_text_width(&presentation.lines[2]), 12);
    assert!(presentation.lines[2].contains('界'));
    assert!(presentation.lines[2].chars().count() < 12);
    assert_eq!(presentation.line_style_spans[2].len(), 1);
    assert_eq!(presentation.line_style_spans[2][0].start, 0);
    assert_eq!(presentation.line_style_spans[2][0].length, 12);
}

/// Verifies that prompt overlays composed from plain line batches still carry
/// cursor placement for attached-terminal output. Control-socket and async
/// prompt paths use this helper when they do not have a full `RenderedClientView`
/// but still need to present an interactive prompt cursor.
#[test]
fn prompt_overlay_presentation_places_cursor_on_prompt_row() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("auth-status");

    let presentation = compose_prompt_overlay_presentation(
        &["pane".to_string(), "old".to_string()],
        &prompt,
        Size::new(24, 3).unwrap(),
    );

    assert_eq!(presentation.lines.len(), 3);
    assert_eq!(presentation.lines[0], "pane                    ");
    assert!(
        presentation
            .lines
            .iter()
            .all(|line| line.chars().count() == 24)
    );
    assert_eq!(presentation.lines[2], "▐ :auth-status          ");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 14);
    assert!(presentation.cursor_visible);
}

/// Verifies that command-prompt shadow hints are rendered as dim spans on top
/// of the normal prompt-row styling rather than becoming editable prompt text.
#[test]
fn prompt_overlay_presentation_styles_command_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("mcp-");

    let presentation = compose_prompt_overlay_presentation_with_styles(
        &[
            "pane                    ".to_string(),
            "old                     ".to_string(),
        ],
        &[Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 2).unwrap(),
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ :mcp-status           ");
    assert!(
        presentation.line_style_spans[1]
            .iter()
            .any(|span| span.start == 7 && span.length == 6 && span.rendition.dim)
    );
    assert!(
        presentation.line_style_spans[1]
            .iter()
            .any(|span| span.start == 7
                && span.length == 6
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(
                            foreground,
                            UiTheme::default().colors.prompt.background,
                        ) >= 4.5
                }))
    );
}

/// Verifies that pane-local agent prompt overlays are drawn inside the owning
/// pane region and keep cursor placement relative to that pane rather than the
/// full terminal footer.
#[test]
fn prompt_region_presentation_places_agent_prompt_inside_pane() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("go");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 2,
            columns: 12,
            rows: 2,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[0], "top line            ");
    assert_eq!(presentation.lines[2], "ol▐ mez> go         ");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 11);
    assert!(presentation.cursor_visible);
    assert_eq!(
        presentation.line_style_spans[2]
            .iter()
            .find(|span| span.start == 2)
            .unwrap()
            .rendition
            .background,
        Some(UiTheme::default().colors.agent_prompt.background)
    );
}

/// Verifies pane-local prompts wrap at words before using a hard boundary.
///
/// Agent prompt input can be long, and wrapping at a prior space keeps adjacent
/// words readable while still fitting the reserved prompt region.
#[test]
fn prompt_region_presentation_wraps_prompt_at_word_boundary() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("alpha beta gamma");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line                ".to_string(),
            "left pane               ".to_string(),
            "old prompt              ".to_string(),
            "footer                  ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 16,
            rows: 3,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ mez> alpha            ");
    assert_eq!(presentation.lines[2], "       beta             ");
    assert_eq!(presentation.lines[3], "       gamma            ");
}

/// Verifies hard-wrapped unbroken agent prompt input starts at the top of the
/// prompt region instead of bottom-aligning the first wrapped row.
#[test]
fn prompt_region_presentation_hard_wrap_keeps_first_row_stable() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("abcdefghijkl");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line                ".to_string(),
            "left pane               ".to_string(),
            "old prompt              ".to_string(),
            "footer                  ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 16,
            rows: 3,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ mez> abcdefghi        ");
    assert_eq!(presentation.lines[2], "       jkl              ");
    assert_eq!(presentation.lines[3], "footer                  ");
}

/// Verifies that pane-local agent prompts render slash-command hints inside the
/// pane region with the same dim styling as footer command prompts.
#[test]
fn prompt_region_presentation_styles_agent_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[2], "o▐ mez> /model      ");
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 12 && span.length == 2 && span.rendition.dim)
    );
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 12
                && span.length == 2
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(
                            foreground,
                            UiTheme::default().colors.agent_prompt.background,
                        ) >= 4.5
                }))
    );
}

/// Verifies pane-local agent prompt input and completion shadows choose
/// contrast-aware black/white foregrounds against light prompt themes.
#[test]
fn prompt_region_presentation_uses_contrast_prompt_foreground_on_light_theme() {
    let definition = builtin_ui_theme_definition("catppuccin_latte").unwrap();
    let theme = resolve_ui_theme("catppuccin_latte", definition).unwrap();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &theme,
    );

    let prompt_span = presentation.line_style_spans[2]
        .iter()
        .find(|span| span.start == 1 && span.length == 18)
        .unwrap();
    assert_eq!(
        prompt_span.rendition.foreground,
        Some(TerminalColor::Rgb(0x00, 0x00, 0x00))
    );
    assert_eq!(
        prompt_span.rendition.background,
        Some(theme.colors.agent_prompt.background)
    );
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 12
                && span.length == 2
                && span.rendition.dim
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(foreground, theme.colors.agent_prompt.background)
                            >= 4.5
                        && foreground != prompt_span.rendition.foreground.unwrap()
                }))
    );
}

/// Verifies pane-local `$skill` completion hints receive a readable muted style
/// instead of inheriting the editable prompt foreground.
#[test]
fn prompt_region_presentation_styles_agent_skill_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("$rev");
    prompt.set_selector_extra_candidates([crate::selector::SelectorExtraCandidate::new(
        crate::selector::SelectorSurface::AgentCommand,
        "$",
        mez_mux::selector::SelectorCandidate::new(
            "$review",
            mez_mux::selector::SelectorCandidateKind::Value,
            true,
        )
        .with_detail("Review workflow"),
    )]);
    let theme = UiTheme::default();
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &theme,
    );

    assert_eq!(presentation.lines[2], "o▐ mez> $review     ");
    let prompt_span = presentation.line_style_spans[2]
        .iter()
        .find(|span| span.start == 1 && span.length == 18)
        .unwrap();
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.rendition.dim
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && foreground != prompt_span.rendition.foreground.unwrap()
                        && test_contrast_ratio(foreground, theme.colors.agent_prompt.background)
                            >= 4.5
                }))
    );
}

/// Verifies pane-local `@server` completion hints reuse the same readable muted
/// style as `$skill` hints instead of inheriting the editable prompt foreground.
#[test]
fn prompt_region_presentation_styles_agent_mcp_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("ask @fi");
    prompt.set_selector_extra_candidates([crate::selector::SelectorExtraCandidate::new(
        crate::selector::SelectorSurface::AgentCommand,
        "@",
        mez_mux::selector::SelectorCandidate::new(
            "@fixture",
            mez_mux::selector::SelectorCandidateKind::Value,
            true,
        )
        .with_detail("available"),
    )]);
    let theme = UiTheme::default();
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &theme,
    );

    assert!(
        presentation.lines[2].contains("@fixture"),
        "{}",
        presentation.lines[2]
    );
    let prompt_span = presentation.line_style_spans[2]
        .iter()
        .find(|span| span.start == 1 && span.length == 18)
        .unwrap();
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.rendition.dim
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && foreground != prompt_span.rendition.foreground.unwrap()
                        && test_contrast_ratio(foreground, theme.colors.agent_prompt.background)
                            >= 4.5
                }))
    );
}

/// Verifies attached pane rendering preserves agent prompt shadow hint styling.
///
/// The standalone prompt-region renderer already styles completion shadows, but
/// pane-local agent mode uses a separate `AgentPromptBlock` path. This protects
/// that path so slash and skill completions stay visually muted in real panes.
#[test]
fn render_attached_client_view_styles_agent_prompt_shadow_hint() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    let row = view
        .lines
        .iter()
        .position(|line| line.contains("/model"))
        .expect("agent prompt should include completion shadow");
    let hint_start = display_column_for_fragment(&view.lines[row], "el");
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.start == hint_start
                && span.length == 2
                && span.rendition.dim
                && span.rendition.background == Some(config.ui_theme.colors.agent_prompt.background)
        }),
        "{:?}",
        view.line_style_spans[row]
    );
}

/// Verifies that a long pasted agent prompt expands upward within the pane and
/// exposes a length note instead of silently hiding that the prompt is large.
#[test]
fn prompt_region_presentation_expands_agent_prompt_for_long_input() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text(&"x".repeat(200));
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "one                 ".to_string(),
            "two                 ".to_string(),
            "three               ".to_string(),
            "four                ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 0,
            column: 0,
            columns: 20,
            rows: 4,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[0], "▐ mez> [200 chars pa");
    assert_eq!(presentation.cursor_row, 3);
    assert_eq!(presentation.cursor_column, 19);
    assert!(presentation.cursor_visible);
}

/// Verifies that entering agent mode reserves a persistent prompt row at the
/// bottom of the active pane and exposes that pane content region to clients.
#[test]
fn render_attached_client_view_reserves_agent_prompt_row() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(30, 3).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree");
    let mut screens = BTreeMap::new();
    screens.insert(pane_id.clone(), screen);
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("idle".to_string()),
            agent_model: Some("default".to_string()),
            agent_reasoning: Some("medium".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    assert_eq!(view.lines[3], format!("{:<30}", "▐ mez> "));
    assert_eq!(
        view.agent_prompt_region,
        Some(ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 30,
            rows: 3,
        })
    );
}

/// Verifies that copy mode keeps the pane-local agent prompt reservation while
/// making the prompt itself invisible. Mouse selection uses copy mode for text
/// selection, and retaining the reserved row prevents the terminal buffer from
/// visually shifting when selection starts inside an agent pane.
#[test]
fn render_attached_client_view_keeps_agent_prompt_space_transparent_in_copy_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(30, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    let mut screens = BTreeMap::new();
    screens.insert(pane_id.clone(), screen);
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("copy this");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("copy".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    assert!(view.lines[2].contains("four"), "{:?}", view.lines);
    assert_eq!(view.lines[3], " ".repeat(30));
    assert!(
        view.lines.iter().all(|line| !line.contains("mez>")),
        "{:?}",
        view.lines
    );
}

/// Verifies that pane rendering uses the pane's retained agent prompt buffer
/// and progress rows directly, instead of relying on a modal full-window prompt
/// overlay. This keeps agent mode local to the pane while the rest of the mux
/// remains interactive.
#[test]
fn render_attached_client_view_draws_agent_prompt_state_in_pane() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 5).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("first\nsecond");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            agent_display_lines: vec!["agent: turn turn-1 running".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut screen = TerminalScreen::new(Size::new(30, 4).unwrap(), 10).unwrap();
    screen.feed(b"\n\n\npane output");
    screens.insert(pane_id, screen);
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    assert!(view.lines.iter().any(|line| line.contains("pane output")));
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("agent: turn turn-1 running")),
        "{:?}",
        view.lines
    );
    assert!(view.lines.iter().any(|line| line.contains("▐ mez> first")));
    assert!(view.lines.iter().any(|line| line.contains("second")));
    assert!(view.cursor_visible);
}

/// Verifies that native-mode agent shells mask active alternate-screen pane
/// content as a pane-local overlay without mutating the pane terminal state.
///
/// Native local execution does not write through the pane PTY, so its live
/// transcript and prompt must render above a full-screen alternate-buffer
/// application while the agent shell is visible. Hiding the agent shell should
/// reveal the same still-active alternate-screen application content.
#[test]
fn render_attached_client_view_masks_alternate_screen_for_native_agent_overlay() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(40, 5).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("inspect overlay");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            agent_display_lines: vec!["native action log".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut screen = TerminalScreen::new(Size::new(40, 5).unwrap(), 10).unwrap();
    screen.feed(b"normal shell text");
    screen.feed(b"\x1b[?1049h\x1b[5;1Hfullscreen tui row");
    screens.insert(pane_id.clone(), screen);
    let overlay_config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let overlay_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &overlay_config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("native action log")),
        "{:?}",
        overlay_view.lines
    );
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("mez> inspect overlay")),
        "{:?}",
        overlay_view.lines
    );
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("fullscreen tui row")),
        "{:?}",
        overlay_view.lines
    );
    assert!(
        screens
            .get(&pane_id)
            .is_some_and(TerminalScreen::alternate_screen_active)
    );

    let program_config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let program_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &program_config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(
        program_view
            .lines
            .iter()
            .any(|line| line.contains("fullscreen tui row")),
        "{:?}",
        program_view.lines
    );
    assert!(
        screens
            .get(&pane_id)
            .is_some_and(TerminalScreen::alternate_screen_active)
    );
}

/// Verifies that active-pane footer reconciliation places live status in the
/// prompt row without leaving a stale pane-rendered copy behind.
///
/// The pane renderer may initially place transient display text on a blank
/// content row to avoid covering terminal output. The active prompt-region now
/// owns the live footer in the empty input line. Without clearing the first
/// copy, agent mode can show duplicate working status rows.
#[test]
fn render_attached_client_view_draws_one_agent_live_footer_at_prompt_edge() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 6).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut screen = TerminalScreen::new(Size::new(64, 5).unwrap(), 10).unwrap();
    screen.feed(b"line00\nline01\n\nline03\nline04");
    screens.insert(pane_id, screen);
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    let prompt_row = view
        .lines
        .iter()
        .position(|line| line.contains("mez> running"))
        .unwrap();
    let footer_rows = view
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| line.contains("esc to interrupt").then_some(row))
        .collect::<Vec<_>>();
    assert_eq!(footer_rows, vec![prompt_row], "{view:?}");
}

/// Verifies stale live-footer cleanup uses terminal cells rather than chars.
///
/// Wide glyphs in a neighboring split can make byte/char offsets differ from
/// terminal columns. The cleanup pass must still recognize and remove stale
/// agent footer text in the active pane so a new prompt-edge footer does not
/// leave behind a blank gutterless row or duplicate status line.
#[test]
fn render_agent_live_footer_cleanup_handles_wide_neighbor_glyphs() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(96, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_id = window.panes()[1].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed("✅ left".as_bytes());
    let mut right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    right.feed("running (5m 39s • esc to interrupt)".as_bytes());
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(pane_id, right);
    let config = TerminalClientLoopConfig {
        frame_context,
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
    let footer_rows = view
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| line.contains("esc to interrupt").then_some(row))
        .collect::<Vec<_>>();

    assert_eq!(footer_rows.len(), 1, "{:?}", view.lines);
    assert!(
        view.lines
            .iter()
            .all(|line| !line.trim_end().is_empty() || !line.contains("▐")),
        "{:?}",
        view.lines
    );
}

/// Verifies typed agent prompt input hides the live footer until the prompt is
/// cleared again.
///
/// The live status is placeholder feedback for an empty agent prompt row. Once
/// the user starts composing a request, the row must prioritize editable input
/// and avoid competing status text.
#[test]
fn render_attached_client_view_hides_agent_live_footer_while_prompt_has_input() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 5).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("write tests");
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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

    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("mez> write tests")),
        "{view:?}"
    );
    assert!(
        view.lines
            .iter()
            .all(|line| !line.contains("esc to interrupt")),
        "{view:?}"
    );
}

/// Verifies that the live agent footer renders the active state label with
/// grayscale scan-band motion over the prompt-bar background.
///
/// The state label uses the active grayscale scan while the timer and stop hint
/// remain readable as a muted static parenthetical.
#[test]
fn render_agent_working_footer_uses_prompt_background_grayscale_gradient() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("running (5m 40s • esc to interrupt)"))
        .expect("working footer should be visible");
    let footer_spans = &view.line_style_spans[footer_row];
    assert!(!footer_spans.is_empty());
    let footer_text = &view.lines[footer_row];
    let state_start_byte = footer_text.find("running").unwrap();
    let state_start = UnicodeWidthStr::width(&footer_text[..state_start_byte]);
    let prompt_background = config.ui_theme.colors.agent_prompt.background;
    assert!(footer_spans.iter().any(|span| span.start >= state_start
        && span.rendition.background == Some(prompt_background)
        && span.rendition.foreground.is_some()));
    let parenthetical_start_byte = footer_text.find(" (").unwrap();
    let parenthetical_start = UnicodeWidthStr::width(&footer_text[..parenthetical_start_byte]);
    let parenthetical = " (5m 40s • esc to interrupt)";
    let parenthetical_end = parenthetical_start + UnicodeWidthStr::width(parenthetical);
    let state_spans = footer_spans
        .iter()
        .filter(|span| {
            span.start >= state_start
                && span.start.saturating_add(span.length) <= parenthetical_start
                && span.rendition.foreground.is_some()
        })
        .collect::<Vec<_>>();
    let parenthetical_spans = footer_spans
        .iter()
        .filter(|span| {
            span.start >= parenthetical_start
                && span.start.saturating_add(span.length) <= parenthetical_end
                && span.rendition.background == Some(prompt_background)
                && span.rendition.foreground.is_some()
        })
        .collect::<Vec<_>>();
    assert!(!state_spans.is_empty(), "{footer_spans:?}");
    assert!(!parenthetical_spans.is_empty(), "{footer_spans:?}");
    assert!(
        parenthetical_spans
            .iter()
            .all(|span| matches!(span.rendition.foreground, Some(TerminalColor::Rgb(red, green, blue)) if red == green && green == blue)),
        "{parenthetical_spans:?}"
    );
    let mut foregrounds = Vec::new();
    for span in state_spans {
        if let Some(foreground) = span.rendition.foreground
            && !foregrounds.contains(&foreground)
        {
            foregrounds.push(foreground);
        }
    }
    assert!(foregrounds.len() >= 3, "{foregrounds:?}");
    assert!(
        foregrounds.iter().all(|color| match color {
            TerminalColor::Rgb(red, green, blue) => red == green && green == blue,
            _ => false,
        }),
        "{foregrounds:?}"
    );
    let levels = foregrounds
        .iter()
        .filter_map(|color| match color {
            TerminalColor::Rgb(red, _, _) => Some(*red),
            TerminalColor::Indexed(_) => None,
        })
        .collect::<Vec<_>>();
    let darkest = levels.iter().copied().min().unwrap_or_default();
    let brightest = levels.iter().copied().max().unwrap_or_default();
    assert!(brightest.saturating_sub(darkest) >= 24, "{foregrounds:?}");
}

/// Verifies the live agent footer switches to dark grayscale text on light
/// themes instead of using hardcoded light greys with weak contrast.
#[test]
fn render_agent_working_footer_uses_dark_grayscale_on_light_theme() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let definition = builtin_ui_theme_definition("catppuccin_latte").unwrap();
    let theme = resolve_ui_theme("catppuccin_latte", definition).unwrap();
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ui_theme: theme,
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
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("running (5m 40s • esc to interrupt)"))
        .expect("working footer should be visible");
    let levels = view.line_style_spans[footer_row]
        .iter()
        .filter_map(|span| match span.rendition.foreground {
            Some(TerminalColor::Rgb(red, green, blue)) if red == green && green == blue => {
                Some(red)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(
        !levels.is_empty(),
        "{:?}",
        view.line_style_spans[footer_row]
    );
    assert!(
        levels.iter().all(|level| *level <= 0xa8),
        "light themes should use dark readable footer greys: {levels:?}"
    );
}

/// Verifies narrow panes keep live-footer state styling even when truncation
/// removes the trailing interrupt-hint suffix from the visible line.
#[test]
fn render_agent_working_footer_keeps_state_styling_when_suffix_is_truncated() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(18, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
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
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("mez> running"))
        .expect("working footer should be visible");
    let footer_text = &view.lines[footer_row];
    let state_start_byte = footer_text.find("running").unwrap();
    let state_start = UnicodeWidthStr::width(&footer_text[..state_start_byte]);

    assert!(
        view.line_style_spans[footer_row].iter().any(|span| {
            span.start >= state_start
                && span.rendition.foreground.is_some()
                && span.rendition.background == Some(config.ui_theme.colors.agent_prompt.background)
        }),
        "{:?}",
        view.line_style_spans[footer_row]
    );
}
