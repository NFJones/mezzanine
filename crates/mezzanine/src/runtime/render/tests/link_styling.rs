//! Runtime render link styling tests.

use super::*;

/// Builds one overlay row that carries a product-registered link action.
///
/// Only registered ranges are selectable, so styling tests compose the retained
/// link rendition and the opaque action identity directly instead of deriving
/// either one from rendered text.
fn registered_link_overlay(
    ui_theme: &mez_mux::theme::UiTheme,
    display: &str,
    copy_text: Option<String>,
    start_column: usize,
    width: usize,
) -> RuntimeDisplayOverlay {
    RuntimeDisplayOverlay {
        lines: vec![display.to_string()],
        line_style_spans: vec![vec![TerminalStyleSpan {
            start: start_column,
            length: width,
            rendition: overlay_link_rendition(ui_theme),
        }]],
        line_copy_texts: vec![copy_text],
        scroll_offset: 0,
        selections: vec![OverlaySelection {
            logical_id: 0,
            line_index: 0,
            start_column,
            width,
            action_id: OverlayActionId(1),
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
    }
}

/// Verifies agent slash markdown containing link syntax registers no action.
///
/// Command bodies are presentation text the product does not author, so a
/// rendered `mez-agent:` destination stays inert while its source text remains
/// copyable for the operator.
#[test]
fn agent_shell_markdown_overlay_keeps_untrusted_links_inert() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("resume".to_string()),
        "- [`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );

    assert_eq!(content.command.as_deref(), Some("resume"));
    assert!(content.actions.is_empty(), "{content:?}");
    assert!(
        content.lines.iter().any(|line| line.contains("saved")),
        "{content:?}"
    );
    assert!(
        content
            .line_copy_texts
            .iter()
            .flatten()
            .any(|copy_text| copy_text.contains("(mez-agent:%2Fresume%20saved)")),
        "{content:?}"
    );
}

/// Verifies a registered pager link keeps the markdown link styling emitted
/// for the body range it was registered from.
///
/// `/resume` rows and listing actions must stay readable as ordinary text
/// links while remaining keyboard and mouse selectable, so the overlay retains
/// the rendered link spans in addition to the registered range.
#[test]
fn registered_link_overlay_preserves_selectable_link_style_spans() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay = registered_link_overlay(
        &ui_theme,
        "• saved",
        Some("- [`saved`](mez-agent:%2Fresume%20saved)".to_string()),
        2,
        5,
    );
    let selection = &overlay.selections[0];
    let line = overlay.lines.get(selection.line_index).unwrap();
    let column = overlay_rendered_selection_start(&overlay, selection);
    assert_eq!(&line[column..column + selection.width], "saved");
    assert!(
        overlay.line_style_spans[selection.line_index]
            .iter()
            .any(|span| {
                span.start == selection.start_column
                    && span.length == selection.width
                    && span.rendition.bold
                    && span.rendition.underline
                    && !span.rendition.inverse
                    && span.rendition.background.is_none()
                    && span.rendition.foreground
                        == Some(ui_theme.colors.agent_transcript_command.foreground)
            }),
        "{overlay:?}"
    );
}

/// Verifies an active registered link keeps link styling on every cell of its
/// range, including the final one.
///
/// Selected ranges layer the selector, body-link, and active-selection spans on
/// the same columns. The final rendered row must preserve the link rendition
/// through the last link character instead of letting the fallback selection
/// span leak onto the tail cell.
#[test]
fn active_registered_link_keeps_tail_cell_link_styling() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay = registered_link_overlay(
        &ui_theme,
        "• saved",
        Some("- [`saved`](mez-agent:%2Fresume%20saved)".to_string()),
        2,
        5,
    );
    let selection = &overlay.selections[0];
    let start = overlay_rendered_selection_start(&overlay, selection);
    let spans = overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
    for column in start..start.saturating_add(selection.width) {
        let rendition = rendered_line_rendition_at(&spans, column);
        assert!(
            rendition.bold,
            "column {column} lost bold styling: {spans:?}"
        );
        assert!(
            rendition.underline,
            "column {column} lost underline styling: {spans:?}"
        );
        assert!(
            !rendition.inverse,
            "column {column} became inverse: {spans:?}"
        );
        assert_eq!(
            rendition.background,
            Some(ui_theme.colors.agent_model.background),
            "column {column} lost active selection background: {spans:?}"
        );
        assert_eq!(
            rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "column {column} lost link foreground: {spans:?}"
        );
    }
}

/// Verifies an active saved-session row keeps link styling on the final visible
/// UUID character.
///
/// Saved-session rows register one action over the visible UUID, and the row
/// keeps the link rendition across the whole id when it is selected, including
/// the final character that previously fell back to plain text.
#[test]
fn active_saved_session_overlay_uuid_keeps_tail_cell_link_styling() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let overlay = registered_link_overlay(
        &ui_theme,
        &format!("• {session_id}"),
        Some(format!(
            "- [`{session_id}`](mez-agent:%2Fresume%20{session_id})"
        )),
        2,
        session_id.len(),
    );
    let selection = &overlay.selections[0];
    let start = overlay_rendered_selection_start(&overlay, selection);
    let spans = overlay_rendered_line_style_spans(&overlay, 0, 120, &ui_theme);
    for column in start..start.saturating_add(selection.width) {
        let rendition = rendered_line_rendition_at(&spans, column);
        assert!(
            rendition.bold,
            "column {column} lost bold styling: {spans:?}"
        );
        assert!(
            rendition.underline,
            "column {column} lost underline styling: {spans:?}"
        );
        assert!(
            !rendition.inverse,
            "column {column} became inverse: {spans:?}"
        );
        assert_eq!(
            rendition.background,
            Some(ui_theme.colors.agent_model.background),
            "column {column} lost active selection background: {spans:?}"
        );
        assert_eq!(
            rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "column {column} lost link foreground: {spans:?}"
        );
    }
}

/// Verifies an active saved-session row does not shift link styling onto the
/// preceding bullet separator cell.
///
/// Saved-session rows render as a bullet plus a linked UUID label. The selected
/// link foreground, underline, and active background must begin on the first
/// UUID cell rather than leaking one column left onto the separator space.
#[test]
fn active_saved_session_overlay_uuid_does_not_style_previous_cell() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let overlay = registered_link_overlay(
        &ui_theme,
        &format!("• {session_id}"),
        Some(format!(
            "- [`{session_id}`](mez-agent:%2Fresume%20{session_id})"
        )),
        2,
        session_id.len(),
    );
    let selection = &overlay.selections[0];
    let start = overlay_rendered_selection_start(&overlay, selection);
    let spans = overlay_rendered_line_style_spans(&overlay, 0, 120, &ui_theme);
    let previous_rendition = rendered_line_rendition_at(&spans, start.saturating_sub(1));

    assert_ne!(
        previous_rendition.foreground,
        Some(ui_theme.colors.agent_transcript_command.foreground),
        "saved-session link foreground shifted left into the separator cell: {spans:?}"
    );
    assert!(
        !previous_rendition.underline,
        "saved-session link underline shifted left into the separator cell: {spans:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(ui_theme.colors.agent_model.background),
        "saved-session active background shifted left into the separator cell: {spans:?}"
    );
}

/// Verifies the active selector gutter stays isolated from a registered link
/// that begins at the first visible body column.
///
/// Some registered rows start at column zero without a list prefix. When the
/// active row's selector gutter abuts that first link cell, the gutter must
/// remain a standalone styled cell so the link highlight does not visually
/// shift left into the gutter column.
#[test]
fn active_markdown_overlay_front_of_line_link_keeps_gutter_separate() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay = registered_link_overlay(
        &ui_theme,
        "saved",
        Some("[`saved`](mez-agent:%2Fresume%20saved)".to_string()),
        0,
        5,
    );
    let selection = &overlay.selections[0];
    let start = overlay_rendered_selection_start(&overlay, selection);
    let spans = overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
    assert_eq!(start, overlay_selection_prefix_columns(), "{spans:?}");
    assert!(
        spans
            .iter()
            .any(|span| { span.start == 0 && span.length == overlay_selection_prefix_columns() }),
        "missing isolated selector gutter span: {spans:?}"
    );
    let gutter_rendition = rendered_line_rendition_at(&spans, 0);
    let gutter_trailing_rendition = rendered_line_rendition_at(&spans, start - 1);
    let first_link_rendition = rendered_line_rendition_at(&spans, start);
    assert_eq!(
        gutter_rendition.foreground, None,
        "gutter inherited selected-link foreground styling: {spans:?}"
    );
    assert!(
        !gutter_rendition.bold,
        "gutter inherited bold link styling: {spans:?}"
    );
    assert!(
        !gutter_rendition.underline,
        "gutter inherited underline link styling: {spans:?}"
    );
    assert_eq!(
        gutter_rendition.background, None,
        "gutter picked up active body highlight: {spans:?}"
    );
    assert_eq!(
        gutter_trailing_rendition.foreground, None,
        "selector gutter trailing cell inherited selected-link foreground styling: {spans:?}"
    );
    assert!(
        !gutter_trailing_rendition.bold,
        "selector gutter trailing cell inherited bold link styling: {spans:?}"
    );
    assert!(
        !gutter_trailing_rendition.underline,
        "selector gutter trailing cell inherited underline link styling: {spans:?}"
    );
    assert_eq!(
        gutter_trailing_rendition.background, None,
        "selector gutter trailing cell picked up active body highlight: {spans:?}"
    );
    assert_eq!(
        first_link_rendition.foreground,
        Some(ui_theme.colors.agent_transcript_command.foreground),
        "front-of-line link styling shifted into the gutter: {spans:?}"
    );
    assert_eq!(
        first_link_rendition.background,
        Some(ui_theme.colors.agent_model.background),
        "front-of-line link lost active body highlight: {spans:?}"
    );
    assert!(
        first_link_rendition.underline,
        "front-of-line link lost underline: {spans:?}"
    );
}

/// Verifies selected-link styling stops at the selected link boundary.
///
/// Active selected-link spans should preserve link foreground and underline on
/// the link body without leaking that rendition into the following display
/// cell, because cursor presentation and adjacent overlay text are composed
/// after the selected-link span list.
#[test]
fn active_markdown_overlay_link_style_stops_before_following_cell() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let overlay = registered_link_overlay(
        &ui_theme,
        "saved next",
        Some("[`saved`](mez-agent:%2Fresume%20saved) next".to_string()),
        0,
        5,
    );
    let selection = &overlay.selections[0];
    let start = overlay_rendered_selection_start(&overlay, selection);
    let following_column = start.saturating_add(selection.width);
    let spans = overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
    let following_rendition = rendered_line_rendition_at(&spans, following_column);
    assert_ne!(
        following_rendition.foreground,
        Some(ui_theme.colors.agent_transcript_command.foreground),
        "link foreground leaked past selected link: {spans:?}"
    );
    assert!(
        !following_rendition.underline,
        "link underline leaked past selected link: {spans:?}"
    );
    assert_eq!(
        following_rendition.background, None,
        "active selection background leaked past selected link: {spans:?}"
    );
}
