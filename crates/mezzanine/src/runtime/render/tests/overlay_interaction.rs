//! Runtime render overlay interaction tests.

use super::*;

/// Verifies pane-agent selector rows fully replace overlapping pane text styling.
///
/// Selector dropdown rows render over live pane content whose cells may
/// already carry bold, underline, or inverse attributes. The overlay must
/// clip any overlapping pane spans and provide a clean selector rendition
/// so per-cell span merging cannot leak those underlying attributes into
/// the dropdown text.
#[test]
fn pane_agent_selector_overlay_clips_underlying_pane_styling() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay_column = 4;
    let overlay_width = 8;
    let pane_rendition = GraphicRendition {
        bold: true,
        underline: true,
        inverse: true,
        ..GraphicRendition::default()
    };
    let selector_rendition = runtime_pane_agent_selector_rendition(
        PaneAgentStatusField::Model,
        "default",
        false,
        &ui_theme,
    );
    let mut spans = vec![TerminalStyleSpan {
        start: 0,
        length: 16,
        rendition: pane_rendition,
    }];

    RuntimeSessionService::clip_line_style_spans_for_overlay(
        &mut spans,
        overlay_column,
        overlay_width,
    );
    spans.push(TerminalStyleSpan {
        start: overlay_column,
        length: overlay_width,
        rendition: selector_rendition,
    });

    let before_overlay = rendered_line_rendition_at(&spans, overlay_column - 1);
    let first_overlay_cell = rendered_line_rendition_at(&spans, overlay_column);
    let final_overlay_cell = rendered_line_rendition_at(&spans, overlay_column + overlay_width - 1);
    let after_overlay = rendered_line_rendition_at(&spans, overlay_column + overlay_width);

    assert_eq!(
        before_overlay, pane_rendition,
        "left pane styling was clipped too broadly: {spans:?}"
    );
    assert_eq!(
        first_overlay_cell, selector_rendition,
        "selector row inherited pane styling on its first cell: {spans:?}"
    );
    assert_eq!(
        final_overlay_cell, selector_rendition,
        "selector row inherited pane styling on its trailing cell: {spans:?}"
    );
    assert_eq!(
        after_overlay, pane_rendition,
        "right pane styling was clipped too broadly: {spans:?}"
    );
}

/// Verifies persistent host execution uses the theme's failure/warning color
/// rather than the ordinary approval-policy selector styling.
#[test]
fn host_access_selector_uses_warning_rendition() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();

    let host_access = runtime_pane_agent_selector_rendition(
        PaneAgentStatusField::ApprovalPolicy,
        "host-access",
        true,
        &ui_theme,
    );
    let full_access = runtime_pane_agent_selector_rendition(
        PaneAgentStatusField::ApprovalPolicy,
        "full-access",
        true,
        &ui_theme,
    );

    assert_eq!(
        host_access.foreground,
        ui_theme.colors.agent_status_failed.rendition().foreground
    );
    assert_ne!(host_access, full_access);
}

/// Verifies pager search highlighting is limited to the matched range.
///
/// Search state stores a concrete body-column range instead of just the
/// matching line, so rendering should style only the submitted match and
/// leave surrounding text with its original body/link rendition.
#[test]
fn display_overlay_search_highlights_only_matching_columns() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let link_rendition = GraphicRendition {
        underline: true,
        foreground: Some(ui_theme.colors.agent_transcript_command.foreground),
        ..GraphicRendition::default()
    };
    let overlay = RuntimeDisplayOverlay {
        lines: vec!["prefix needle suffix".to_string()],
        line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 20,
            rendition: link_rendition,
        }]],
        line_copy_texts: vec![None],
        scroll_offset: 0,
        selections: Vec::new(),
        active_selection_index: None,
        dismiss_on_any_input: false,
        search_input: None,
        search_query: Some("needle".to_string()),
        search_match: Some(OverlaySearchMatch {
            line_index: 0,
            start_column: 7,
            width: 6,
        }),
        search_status: None,
        mouse_selection: None,
        live_source: None,
        record_browser: None,
    };

    let spans = overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
    let before_match = rendered_line_rendition_at(&spans, 6);
    let first_match = rendered_line_rendition_at(&spans, 7);
    let final_match = rendered_line_rendition_at(&spans, 12);
    let after_match = rendered_line_rendition_at(&spans, 13);

    assert_eq!(
        before_match.foreground,
        Some(ui_theme.colors.agent_transcript_command.foreground),
        "style before match was overwritten: {spans:?}"
    );
    assert!(
        before_match.underline,
        "style before match lost underline: {spans:?}"
    );
    assert_eq!(
        first_match,
        ui_theme.colors.copy_selection.rendition(),
        "first match cell was not highlighted: {spans:?}"
    );
    assert_eq!(
        final_match,
        ui_theme.colors.copy_selection.rendition(),
        "final match cell was not highlighted: {spans:?}"
    );
    assert_eq!(
        after_match.foreground,
        Some(ui_theme.colors.agent_transcript_command.foreground),
        "style after match was overwritten: {spans:?}"
    );
    assert!(
        after_match.underline,
        "style after match lost underline: {spans:?}"
    );
}

/// Verifies pager search highlighting skips matches outside the visible row.
///
/// A match range past the clipped viewport should not emit a fallback row
/// highlight, otherwise the visible text appears to match a query that is
/// actually off-screen.
#[test]
fn display_overlay_search_skips_offscreen_match_ranges() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay = RuntimeDisplayOverlay {
        lines: vec!["visible text then hidden needle".to_string()],
        line_style_spans: vec![Vec::new()],
        line_copy_texts: vec![None],
        scroll_offset: 0,
        selections: Vec::new(),
        active_selection_index: None,
        dismiss_on_any_input: false,
        search_input: None,
        search_query: Some("needle".to_string()),
        search_match: Some(OverlaySearchMatch {
            line_index: 0,
            start_column: 25,
            width: 6,
        }),
        search_status: None,
        mouse_selection: None,
        live_source: None,
        record_browser: None,
    };

    let spans = overlay_rendered_line_style_spans(&overlay, 0, 12, &ui_theme);

    assert!(
        spans
            .iter()
            .all(|span| span.rendition != ui_theme.colors.copy_selection.rendition()),
        "off-screen match produced a visible highlight: {spans:?}"
    );
}

/// Verifies a saved-session markdown row registers no action by itself.
///
/// Record rows become selectable only when the product registers a validated
/// open target for a visible record id, so a `mez-agent:` destination in body
/// text stays inert no matter how often its id appears.
#[test]
fn agent_shell_markdown_overlay_registers_no_session_link() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("resume".to_string()),
        "- [`018f6b3a-1b2c-7000-9000-cafebabefeed`](mez-agent:%2Fresume%20018f6b3a-1b2c-7000-9000-cafebabefeed)",
        &ui_theme,
    );

    assert!(content.actions.is_empty(), "{content:?}");
    assert_eq!(
        content
            .lines
            .iter()
            .filter(|line| line.contains("018f6b3a-1b2c-7000-9000-cafebabefeed"))
            .count(),
        1,
        "{content:?}"
    );
}

/// Verifies a rendered markdown destination stays literal and inert.
///
/// Destinations are no longer hidden, so the operator can read exactly what a
/// body-text link points at while the row stays copyable and unselectable.
#[test]
fn agent_shell_markdown_overlay_renders_link_destinations_literally() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("status".to_string()),
        "saved before [`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );

    assert!(content.actions.is_empty(), "{content:?}");
    assert_eq!(content.lines.len(), 1, "{content:?}");
    assert!(
        content.lines[0].starts_with("saved before saved"),
        "{content:?}"
    );
    assert!(
        content.lines[0].contains("(mez-agent:%2Fresume%20saved)"),
        "{content:?}"
    );
}

/// Verifies single-link overlay mouse hit testing remains column bounded.
///
/// Rows with one selectable command still contain inert gutter, whitespace,
/// and descriptive text. Mouse selection should execute only clicks inside
/// the advertised choice range, matching multi-chip rows.
#[test]
fn display_overlay_single_selection_hit_testing_requires_link_bounds() {
    let overlay = RuntimeDisplayOverlay {
        lines: vec!["text before [open] after".to_string()],
        line_style_spans: vec![Vec::new()],
        line_copy_texts: vec![None],
        scroll_offset: 0,
        selections: vec![OverlaySelection {
            logical_id: 0,
            line_index: 0,
            start_column: "text before ".len(),
            width: "[open]".len(),
            action_id: OverlayActionId(0),
            kind: OverlaySelectionKind::Primary,
        }],
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        live_source: None,
        record_browser: None,
    };
    let rendered_start = overlay_rendered_selection_start(&overlay, &overlay.selections[0]);

    assert_eq!(
        super::super::overlay_selection_index_at_position(&overlay, 0, 0),
        None
    );
    assert_eq!(
        super::super::overlay_selection_index_at_position(
            &overlay,
            0,
            rendered_start.saturating_add(1),
        ),
        Some(0)
    );
}

/// Verifies scrolling moves the active command selection to the visible
/// viewport before Enter can execute it.
///
/// Mouse-wheel and page-scroll paths should not leave keyboard execution
/// armed on an off-screen action after the overlay viewport changes.
#[test]
fn display_overlay_scroll_keeps_active_selection_visible() {
    let mut overlay = RuntimeDisplayOverlay {
        lines: vec![
            "first".to_string(),
            "plain".to_string(),
            "also plain".to_string(),
            "second".to_string(),
            "tail".to_string(),
        ],
        line_style_spans: vec![Vec::new(); 5],
        line_copy_texts: vec![None; 5],
        scroll_offset: 0,
        selections: vec![
            OverlaySelection {
                logical_id: 0,
                line_index: 0,
                start_column: 0,
                width: 5,
                action_id: OverlayActionId(0),
                kind: OverlaySelectionKind::Primary,
            },
            OverlaySelection {
                logical_id: 1,
                line_index: 3,
                start_column: 0,
                width: 6,
                action_id: OverlayActionId(1),
                kind: OverlaySelectionKind::Primary,
            },
        ],
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        live_source: None,
        record_browser: None,
    };

    assert!(super::super::apply_overlay_scroll_delta(
        &mut overlay,
        3,
        Size::new(80, 4).unwrap(),
    ));
    assert_eq!(overlay.scroll_offset, 3);
    assert_eq!(overlay.active_selection_index, Some(1));
}
