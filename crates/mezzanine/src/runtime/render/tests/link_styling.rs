//! Runtime render link styling tests.

use super::*;

/// Verifies agent slash markdown shown in the command overlay keeps
/// `mez-agent:` links selectable after markdown rendering. This preserves
/// `/list-sessions` resume links while moving informational slash output
/// out of the pane transcript.
#[test]
fn agent_shell_markdown_overlay_preserves_agent_links() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-sessions".to_string()),
        "- [`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );

    assert_eq!(content.command.as_deref(), Some("list-sessions"));
    assert!(
        content
            .lines
            .iter()
            .any(|line| line.contains("saved") && !line.contains("mez-agent:")),
        "{content:?}"
    );
    assert!(
        content
            .selections
            .iter()
            .any(|selection| selection.command == "/resume saved"),
        "{content:?}"
    );
    assert_eq!(
        content
            .selections
            .iter()
            .filter(|selection| selection.command == "/resume saved")
            .count(),
        1,
        "{content:?}"
    );
}
/// Verifies selectable pager links keep the markdown link styling emitted
/// by the CommonMark renderer.
///
/// `/list-sessions` and similar markdown-backed command overlays should
/// keep links readable as ordinary text links while remaining keyboard and
/// mouse selectable, so the overlay must retain the rendered line spans in
/// addition to the selection metadata.
#[test]
fn agent_shell_markdown_overlay_preserves_selectable_link_style_spans() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-sessions".to_string()),
        "- [`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );
    assert_eq!(content.selections.len(), 1, "{content:?}");
    let selection = &content.selections[0];
    let line = content.lines.get(selection.line_index).unwrap();
    let column = overlay_rendered_selection_start(
        &RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            line_copy_texts: content.line_copy_texts.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        },
        selection,
    );
    assert_eq!(&line[column..column + selection.width], "saved");
    assert!(
        content.line_style_spans[selection.line_index]
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
        "{content:?}"
    );
}
/// Verifies an active pager link keeps link styling on every rendered cell.
///
/// Selected command-overlay links layer selector and markdown spans on the
/// same columns. The final rendered row must preserve the markdown link
/// rendition through the last link character instead of letting the
/// fallback selection span leak onto the tail cell.
#[test]
fn active_markdown_overlay_link_keeps_tail_cell_link_styling() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-sessions".to_string()),
        "- [`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );
    let overlay = RuntimeDisplayOverlay {
        lines: content.lines.clone(),
        line_style_spans: content.line_style_spans.clone(),
        line_copy_texts: content.line_copy_texts.clone(),
        scroll_offset: 0,
        selections: content.selections.clone(),
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };
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
/// Verifies an active saved-session UUID row keeps link styling on the
/// final visible UUID character.
///
/// `/list-sessions` rows are emitted as hidden `mez-agent:` resume links
/// with bold UUID labels. The command overlay must preserve that link
/// rendition across the full visible UUID when the row is selected,
/// including the final character that previously fell back to plain text.
#[test]
fn active_saved_session_overlay_uuid_keeps_tail_cell_link_styling() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-sessions".to_string()),
        &format!("- [**{session_id}**](mez-agent:%2Fresume%20{session_id})"),
        &ui_theme,
    );
    let overlay = RuntimeDisplayOverlay {
        lines: content.lines.clone(),
        line_style_spans: content.line_style_spans.clone(),
        line_copy_texts: content.line_copy_texts.clone(),
        scroll_offset: 0,
        selections: content.selections.clone(),
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };
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

/// Verifies an active saved-session UUID row does not shift link styling
/// onto the preceding bullet separator cell.
///
/// `/resume` opens a selectable saved-session pager whose rows render as a
/// bullet plus a bold linked UUID label. The selected-link foreground,
/// underline, and active background must begin on the first UUID cell
/// rather than leaking one column left onto the separator space.
#[test]
fn active_saved_session_overlay_uuid_does_not_style_previous_cell() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-sessions".to_string()),
        &format!("- [**{session_id}**](mez-agent:%2Fresume%20{session_id})"),
        &ui_theme,
    );
    let overlay = RuntimeDisplayOverlay {
        lines: content.lines.clone(),
        line_style_spans: content.line_style_spans.clone(),
        line_copy_texts: content.line_copy_texts.clone(),
        scroll_offset: 0,
        selections: content.selections.clone(),
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };
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

/// Verifies the active selector gutter stays isolated from a link that
/// begins at the first visible body column.
///
/// `/status` renders some selectable links without a list-prefix gap. When
/// the active row's selector gutter abuts that first link cell, the gutter
/// must remain a standalone styled cell so the link highlight does not
/// visually shift left into the gutter column.
#[test]
fn active_markdown_overlay_front_of_line_link_keeps_gutter_separate() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("status".to_string()),
        "[`saved`](mez-agent:%2Fresume%20saved)",
        &ui_theme,
    );
    let overlay = RuntimeDisplayOverlay {
        lines: content.lines.clone(),
        line_style_spans: content.line_style_spans.clone(),
        line_copy_texts: content.line_copy_texts.clone(),
        scroll_offset: 0,
        selections: content.selections.clone(),
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };
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
/// Active selected-link spans should preserve link foreground and underline
/// on the link body without leaking that rendition into the following
/// display cell, because cursor presentation and adjacent overlay text are
/// composed after the selected-link span list.
#[test]
fn active_markdown_overlay_link_style_stops_before_following_cell() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("status".to_string()),
        "[`saved`](mez-agent:%2Fresume%20saved) next",
        &ui_theme,
    );
    let overlay = RuntimeDisplayOverlay {
        lines: content.lines.clone(),
        line_style_spans: content.line_style_spans.clone(),
        line_copy_texts: content.line_copy_texts.clone(),
        scroll_offset: 0,
        selections: content.selections.clone(),
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };
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
