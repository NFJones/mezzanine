//! Attached-terminal output planning and encoding.
//!
//! This module owns retained frame state, safe row and segment diffing,
//! terminal style-span merging, SGR encoding, host mode transitions, and cursor
//! presentation. It operates only on neutral mux presentation records and
//! terminal contracts; product crates remain responsible for actual writes and
//! endpoint lifecycle.

use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalStyleSpan, terminal_emoji_width, terminal_graphemes,
};

#[cfg(test)]
use crate::layout::Size;
use crate::presentation::AttachedTerminalOutputModes;
#[cfg(test)]
use crate::presentation::{
    ClientStatusLine, RenderedClientView, compose_client_presentation_with_styles,
};
#[cfg(test)]
use crate::presentation::{ClientViewRole, TerminalCursorStyle};
#[cfg(test)]
use crate::theme::UiTheme;

/// Measures one terminal grapheme under the active process compatibility mode.
fn terminal_grapheme_width(grapheme: &str) -> usize {
    mez_terminal::terminal_grapheme_width(grapheme, terminal_emoji_width())
}

/// Measures terminal text under the active process compatibility mode.
fn terminal_text_width(value: &str) -> usize {
    mez_terminal::terminal_text_width(value, terminal_emoji_width())
}

/// Retained attached-terminal frame used to plan differential updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedTerminalOutputFrameState {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Whether host bracketed paste was enabled for the retained frame.
    bracketed_paste: bool,
    /// Whether host focus event reporting was enabled for the retained frame.
    focus_events: bool,
    /// Whether alternate-screen host presentation was enabled for the retained frame.
    alternate_screen: bool,
    /// Whether host mouse reporting was enabled for the retained frame.
    host_mouse_reporting: bool,
    /// Cursor presentation sequence emitted by the retained frame.
    cursor_presentation: String,
}

impl AttachedTerminalOutputFrameState {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(crate) fn new(lines: &[String], line_style_spans: &[Vec<TerminalStyleSpan>]) -> Self {
        Self::new_with_modes(
            lines,
            line_style_spans,
            AttachedTerminalOutputModes::default(),
        )
    }

    /// Builds retained frame state using the presentation modes emitted with
    /// the frame.
    pub fn new_with_modes(
        lines: &[String],
        line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> Self {
        Self {
            lines: lines.to_vec(),
            line_style_spans: normalized_style_span_rows(line_style_spans, lines.len()),
            bracketed_paste: modes.bracketed_paste,
            focus_events: modes.focus_events,
            alternate_screen: modes.alternate_screen,
            host_mouse_reporting: modes.host_mouse_reporting,
            cursor_presentation: cursor_presentation_sequence(lines, modes),
        }
    }
}

/// Encodes a full frame with an optional application-keypad transition.
#[cfg(test)]
pub(super) fn encode_attached_terminal_output_frame_with_keypad_transition(
    lines: &[String],
    keypad_transition: Option<bool>,
) -> Vec<u8> {
    encode_attached_terminal_output_frame_with_styles(
        lines,
        &[],
        keypad_transition,
        AttachedTerminalOutputModes::default(),
    )
}

/// Runs the encode attached terminal output frame with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn encode_attached_terminal_output_frame_with_styles(
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    keypad_transition: Option<bool>,
    modes: AttachedTerminalOutputModes,
) -> Vec<u8> {
    let mut frame = Vec::new();
    match keypad_transition {
        Some(true) => frame.extend_from_slice(b"\x1b="),
        Some(false) => frame.extend_from_slice(b"\x1b>"),
        None => {}
    }
    frame.extend_from_slice(attached_terminal_enter_presentation_frame());
    frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
        modes.host_mouse_reporting,
    ));
    frame.extend_from_slice(attached_terminal_bracketed_paste_frame(
        modes.bracketed_paste,
    ));
    frame.extend_from_slice(attached_terminal_focus_events_frame(modes.focus_events));
    frame.extend_from_slice(attached_terminal_alternate_screen_frame(
        modes.alternate_screen,
    ));
    frame.extend_from_slice(b"\x1b[2J\x1b[H");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            frame.extend_from_slice(b"\r\n\x1b[0m");
        }
        let spans = line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        frame.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
    }
    frame.extend_from_slice(cursor_presentation_sequence(lines, modes).as_bytes());
    frame
}

/// Encodes either a full redraw or a row-differential update for an attached TTY.
///
/// The first frame and row-count changes still get a full redraw. Stable-row
/// updates rewrite only the rows whose text or SGR spans changed before
/// restoring Mezzanine's cursor. Rows that shrink are cleared before their new
/// content is written, avoiding a full-screen clear without relying on
/// erase-after-text behavior at the final terminal column.
pub fn encode_attached_terminal_output_update_frame_with_styles(
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    keypad_transition: Option<bool>,
    modes: AttachedTerminalOutputModes,
    previous: Option<&AttachedTerminalOutputFrameState>,
) -> Vec<u8> {
    let Some(previous) = previous else {
        return encode_attached_terminal_output_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
        );
    };
    if output_row_count_changed(previous, lines)
        || previous.alternate_screen != modes.alternate_screen
    {
        return encode_attached_terminal_output_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
        );
    }
    let mut frame = Vec::new();
    match keypad_transition {
        Some(true) => frame.extend_from_slice(b"\x1b="),
        Some(false) => frame.extend_from_slice(b"\x1b>"),
        None => {}
    }
    if previous.bracketed_paste != modes.bracketed_paste {
        frame.extend_from_slice(attached_terminal_bracketed_paste_frame(
            modes.bracketed_paste,
        ));
    }
    if previous.focus_events != modes.focus_events {
        frame.extend_from_slice(attached_terminal_focus_events_frame(modes.focus_events));
    }
    if previous.host_mouse_reporting != modes.host_mouse_reporting {
        frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
            modes.host_mouse_reporting,
        ));
    }
    let changed_row_count = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            let previous_line = previous.lines.get(*index).map(String::as_str).unwrap_or("");
            let previous_spans = previous
                .line_style_spans
                .get(*index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let spans = line_style_spans
                .get(*index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            line.as_str() != previous_line || spans != previous_spans
        })
        .count();
    let allow_segment_updates = changed_row_count <= 3;
    let mut changed_rows = 0usize;
    let mut presentation_reset_emitted = false;
    for (index, line) in lines.iter().enumerate() {
        let previous_line = previous.lines.get(index).map(String::as_str).unwrap_or("");
        let previous_spans = previous
            .line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let spans = line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if line.as_str() == previous_line && spans == previous_spans {
            continue;
        }
        if !presentation_reset_emitted {
            frame.extend_from_slice(attached_terminal_enter_presentation_frame());
            frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
                modes.host_mouse_reporting,
            ));
            if previous.focus_events || modes.focus_events {
                frame.extend_from_slice(attached_terminal_focus_events_frame(modes.focus_events));
            }
            presentation_reset_emitted = true;
        }
        let row = index.saturating_add(1);
        if allow_segment_updates
            && let Some(span_update) =
                encode_safe_changed_row_span_update(row, previous_line, line, previous_spans, spans)
        {
            frame.extend_from_slice(&span_update);
        } else {
            frame.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
            frame.extend_from_slice(b"\x1b[0m");
            if terminal_line_width(line) < terminal_line_width(previous_line) {
                frame.extend_from_slice(b"\x1b[2K");
            }
            frame.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
        }
        changed_rows = changed_rows.saturating_add(1);
    }
    let cursor_presentation = cursor_presentation_sequence(lines, modes);
    if changed_rows > 0 || cursor_presentation != previous.cursor_presentation {
        if !presentation_reset_emitted {
            frame.extend_from_slice(attached_terminal_enter_presentation_frame());
            frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
                modes.host_mouse_reporting,
            ));
            if previous.focus_events || modes.focus_events {
                frame.extend_from_slice(attached_terminal_focus_events_frame(modes.focus_events));
            }
        }
        frame.extend_from_slice(cursor_presentation.as_bytes());
    }
    frame
}

/// Runs the normalized style span rows operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalized_style_span_rows(
    line_style_spans: &[Vec<TerminalStyleSpan>],
    line_count: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    (0..line_count)
        .map(|index| line_style_spans.get(index).cloned().unwrap_or_default())
        .collect()
}
/// Builds style rows for full terminal presentation output lines from the same
/// rendered view that produced the text rows.
///
/// The function intentionally drops style rows unless the caller-provided rows
/// are the complete rendered presentation. Text equality for a partial row slice
/// is not a safe provenance signal: agent output such as apply-patch diff
/// previews can match rows already visible in an unfocused pane, and reusing
/// those render-owned spans can apply hidden or overlay attributes to unrelated
/// output.
#[cfg(test)]
pub(crate) fn compose_terminal_output_style_spans(
    output_lines: &[String],
    rendered: Option<&(RenderedClientView, Option<ClientStatusLine>)>,
) -> Vec<Vec<TerminalStyleSpan>> {
    let Some((view, status)) = rendered else {
        return Vec::new();
    };
    let (styled_lines, line_style_spans) =
        compose_client_presentation_with_styles(view, status.as_ref());
    if styled_lines == output_lines {
        normalized_style_span_rows(&line_style_spans, output_lines.len())
    } else {
        Vec::new()
    }
}
/// Verifies focused render-only style rows are not reused for changed diff text.
///
/// This regression protects apply-patch previews containing Rust option text such
/// as `Some` and `None`. A focused primary view can carry overlay style spans for
/// the rendered rows; if the terminal writer receives a different set of output
/// rows, those spans must be dropped instead of being applied to the new diff
/// text where they can hide otherwise-present symbols.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_drop_focused_overlay_spans_for_mismatched_diff_rows() {
    let hidden_some_span = TerminalStyleSpan {
        start: 9,
        length: 4,
        rendition: GraphicRendition {
            hidden: true,
            ..GraphicRendition::default()
        },
    };
    let rendered_view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(40, 2).unwrap(),
        client_size: Size::new(40, 2).unwrap(),
        lines: vec![
            "agent output".to_string(),
            "- value: Some(None)".to_string(),
        ],
        line_style_spans: vec![Vec::new(), vec![hidden_some_span]],
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
        primary_prompt_active: true,
    };
    let output_lines = vec!["+ value: Some(None)".to_string()];

    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));

    assert!(style_spans.is_empty(), "{style_spans:?}");
}

/// Verifies hidden render spans are not reused for matching diff row slices.
///
/// This regression covers apply-patch previews in unfocused panes. The textual
/// diff row can already be present in the rendered view, but matching that text
/// does not prove that the output write owns the render spans. Hidden spans from
/// the stale presentation must not be copied onto the new output where they can
/// make Rust tokens such as `Some` and `None` invisible.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_drop_hidden_spans_for_matching_diff_row_slices() {
    let hidden_some_span = TerminalStyleSpan {
        start: 9,
        length: 4,
        rendition: GraphicRendition {
            hidden: true,
            ..GraphicRendition::default()
        },
    };
    let rendered_view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(40, 3).unwrap(),
        client_size: Size::new(40, 3).unwrap(),
        lines: vec![
            "agent output".to_string(),
            "+ value: Some(None)".to_string(),
            "done".to_string(),
        ],
        line_style_spans: vec![Vec::new(), vec![hidden_some_span], Vec::new()],
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
    let output_lines = vec!["+ value: Some(None)".to_string()];

    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));

    assert!(style_spans.is_empty(), "{style_spans:?}");
}

/// Verifies later overlay spans preserve earlier diff-token foreground colors.
///
/// This regression covers the focused apply-patch preview path where a later
/// selection or focus overlay contributes background styling on top of an
/// existing syntax or diff foreground. The attached-terminal encoder must merge
/// the overlapping spans instead of letting the later overlay replace the whole
/// rendition, or Rust tokens such as `Some` can become invisible in focused
/// panes.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_merge_overlay_background_with_diff_foreground() {
    let encoded = encode_styled_terminal_line(
        "+ value: Some(None)",
        &[
            TerminalStyleSpan {
                start: 9,
                length: 4,
                rendition: GraphicRendition {
                    foreground: Some(TerminalColor::Indexed(2)),
                    ..GraphicRendition::default()
                },
            },
            TerminalStyleSpan {
                start: 9,
                length: 4,
                rendition: GraphicRendition {
                    background: Some(TerminalColor::Indexed(4)),
                    ..GraphicRendition::default()
                },
            },
        ],
    );

    assert!(
        encoded.contains("+ value: \x1b[0;32;44mSome"),
        "{encoded:?}"
    );
}

/// Verifies ASCII-only row diffs still use bounded attached-terminal updates.
///
/// This regression keeps the retained-frame optimization active for ordinary
/// single-width text after the wide-glyph safety fallback is added. Narrow row
/// changes should continue to emit a compact cursor-positioned segment update
/// instead of forcing a full-row redraw.
#[cfg(test)]
#[test]
fn attached_terminal_row_span_update_keeps_ascii_segment_diffs() {
    let span_update =
        encode_safe_changed_row_span_update(2, "status: old", "status: new", &[], &[]);

    assert_eq!(span_update, Some(b"\x1b[2;9H\x1b[0mnew".to_vec()));
}

/// Verifies row diffs reject segment updates beside an old wide glyph.
///
/// Full-screen TUIs often update one ASCII cell next to an existing wide
/// grapheme. A partial rewrite can leave the old continuation cell visible on
/// the host terminal, so the diff encoder must fall back to a full-row redraw
/// when the changed span abuts the previous wide-glyph footprint.
#[cfg(test)]
#[test]
fn attached_terminal_row_span_update_rejects_change_adjacent_to_previous_wide_glyph() {
    let span_update = encode_safe_changed_row_span_update(1, "a界x", "a界y", &[], &[]);

    assert!(span_update.is_none(), "{span_update:?}");
}

/// Verifies row diffs reject segment updates that replace one wide glyph.
///
/// This regression covers full-screen rows that swap one double-width grapheme
/// for another while leaving the surrounding text stable. The host terminal
/// needs a full-row rewrite to replace the old continuation footprint safely,
/// so the segment diff path must decline the update.
#[cfg(test)]
#[test]
fn attached_terminal_row_span_update_rejects_wide_glyph_replacement() {
    let span_update = encode_safe_changed_row_span_update(1, "a界x", "a海x", &[], &[]);

    assert!(span_update.is_none(), "{span_update:?}");
}

/// Runs the output row count changed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn output_row_count_changed(previous: &AttachedTerminalOutputFrameState, lines: &[String]) -> bool {
    previous.lines.len() != lines.len()
}

/// Encodes a bounded row-segment update when the changed text occupies a stable
/// display-column range.
fn encode_safe_changed_row_span_update(
    row: usize,
    previous_line: &str,
    line: &str,
    previous_spans: &[TerminalStyleSpan],
    spans: &[TerminalStyleSpan],
) -> Option<Vec<u8>> {
    if terminal_line_width(previous_line) != terminal_line_width(line) {
        return None;
    }

    let previous_cells = terminal_row_cells(previous_line, previous_spans);
    let current_cells = terminal_row_cells(line, spans);
    if previous_cells.len() != current_cells.len() {
        return None;
    }
    let start = previous_cells
        .iter()
        .zip(current_cells.iter())
        .position(|(previous, current)| !terminal_row_cells_match(previous, current))?;
    let mut previous_end = previous_cells.len();
    let mut current_end = current_cells.len();
    while previous_end > start
        && current_end > start
        && terminal_row_cells_match(
            &previous_cells[previous_end.saturating_sub(1)],
            &current_cells[current_end.saturating_sub(1)],
        )
    {
        previous_end = previous_end.saturating_sub(1);
        current_end = current_end.saturating_sub(1);
    }

    let start_column = current_cells[start].column_start;
    let current_end_cell = &current_cells[current_end.saturating_sub(1)];
    let end_column = current_end_cell.column_end;
    let (start_column, end_column) =
        expand_changed_column_range(previous_spans, spans, start_column, end_column);
    if changed_span_touches_wide_grapheme(&previous_cells, start_column, end_column)
        || changed_span_touches_wide_grapheme(&current_cells, start_column, end_column)
    {
        return None;
    }
    let start_cell = current_cells
        .iter()
        .position(|cell| cell.column_end > start_column)?;
    // When start_column falls inside a wide glyph continuation cell,
    // the position above skips the leading cell. Align start_column
    // back to the leading cell's start so that clipped style spans
    // match the segment text byte offsets.
    let start_column = start_column.min(current_cells[start_cell].column_start);
    let end_cell = current_cells
        .iter()
        .rposition(|cell| cell.column_start < end_column)?;
    let segment = &line[current_cells[start_cell].byte_start..current_cells[end_cell].byte_end];

    let segment_spans = clip_style_spans_to_column_range(spans, start_column, end_column);
    let encoded_segment = encode_styled_terminal_line(segment, &segment_spans);
    let mut span_update =
        format!("\x1b[{row};{}H\x1b[0m", start_column.saturating_add(1)).into_bytes();
    span_update.extend_from_slice(encoded_segment.as_bytes());

    let mut row_update = format!("\x1b[{row};1H\x1b[0m").into_bytes();
    row_update.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
    (span_update.len() < row_update.len()).then_some(span_update)
}

/// Returns whether a changed column range overlaps or abuts any wide grapheme.
///
/// Host terminals can retain a stale continuation cell when a retained-frame
/// diff rewrites only part of a row next to a double-width grapheme. Callers
/// should fall back to a full-row redraw whenever the changed range touches a
/// wide-glyph footprint in either the previous or current row.
fn changed_span_touches_wide_grapheme(
    cells: &[TerminalRowCell<'_>],
    start_column: usize,
    end_column: usize,
) -> bool {
    cells.iter().any(|cell| {
        cell.column_end.saturating_sub(cell.column_start) > 1
            && cell.column_start <= end_column
            && cell.column_end >= start_column
    })
}

/// Expands one changed column range to include any overlapping style spans.
fn expand_changed_column_range(
    previous_spans: &[TerminalStyleSpan],
    spans: &[TerminalStyleSpan],
    start: usize,
    end: usize,
) -> (usize, usize) {
    let mut expanded_start = start;
    let mut expanded_end = end;
    loop {
        let mut changed = false;
        for span in previous_spans.iter().chain(spans.iter()) {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            if span_start < expanded_end && span_end > expanded_start {
                let next_start = expanded_start.min(span_start);
                let next_end = expanded_end.max(span_end);
                if next_start != expanded_start || next_end != expanded_end {
                    expanded_start = next_start;
                    expanded_end = next_end;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (expanded_start, expanded_end)
}

/// Carries one rendered grapheme cell plus the rendition active across it.
#[derive(Debug, Clone, Copy)]
struct TerminalRowCell<'a> {
    /// Source slice for the grapheme occupying this display-cell span.
    text: &'a str,
    /// Inclusive byte offset at which the grapheme begins.
    byte_start: usize,
    /// Exclusive byte offset at which the grapheme ends.
    byte_end: usize,
    /// Inclusive display column at which the grapheme begins.
    column_start: usize,
    /// Exclusive display column at which the grapheme ends.
    column_end: usize,
    /// Active terminal rendition for this grapheme span.
    rendition: GraphicRendition,
}

/// Collects rendered graphemes into display-cell spans with their active style.
fn terminal_row_cells<'a>(line: &'a str, spans: &[TerminalStyleSpan]) -> Vec<TerminalRowCell<'a>> {
    let mut cells = Vec::new();
    let mut search_offset = 0usize;
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let Some(relative_start) = line[search_offset..].find(grapheme) else {
            debug_assert!(
                false,
                "terminal_graphemes produced a grapheme not findable in line at offset {search_offset}"
            );
            return Vec::new();
        };
        let byte_start = search_offset.saturating_add(relative_start);
        let byte_end = byte_start.saturating_add(grapheme.len());
        let width = terminal_grapheme_width(grapheme);
        cells.push(TerminalRowCell {
            text: grapheme,
            byte_start,
            byte_end,
            column_start: column,
            column_end: column.saturating_add(width),
            rendition: rendition_at_column(spans, column),
        });
        search_offset = byte_end;
        column = column.saturating_add(width);
    }
    cells
}

/// Returns whether two rendered grapheme cells are visually identical.
fn terminal_row_cells_match(previous: &TerminalRowCell<'_>, current: &TerminalRowCell<'_>) -> bool {
    previous.text == current.text
        && previous.column_start == current.column_start
        && previous.column_end == current.column_end
        && previous.rendition == current.rendition
}

/// Clips row style spans to a changed column range.
fn clip_style_spans_to_column_range(
    spans: &[TerminalStyleSpan],
    start: usize,
    end: usize,
) -> Vec<TerminalStyleSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            let clipped_start = span_start.max(start);
            let clipped_end = span_end.min(end);
            (clipped_start < clipped_end).then_some(TerminalStyleSpan {
                start: clipped_start.saturating_sub(start),
                length: clipped_end.saturating_sub(clipped_start),
                rendition: span.rendition,
            })
        })
        .collect()
}

/// Runs the terminal line width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_line_width(line: &str) -> usize {
    terminal_text_width(line)
}

/// Defines the fn const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const fn attached_terminal_enter_presentation_frame() -> &'static [u8] {
    b"\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h"
}

/// Returns the host mouse reporting DEC private-mode sequence for a frame.
const fn attached_terminal_mouse_reporting_frame(enabled: bool) -> &'static [u8] {
    if enabled {
        b"\x1b[?1000;1002;1006h"
    } else {
        b"\x1b[?1006l\x1b[?1002l\x1b[?1000l"
    }
}

/// Returns the host bracketed-paste DEC private-mode sequence for a frame.
const fn attached_terminal_bracketed_paste_frame(enabled: bool) -> &'static [u8] {
    if enabled {
        b"\x1b[?2004h"
    } else {
        b"\x1b[?2004l"
    }
}

/// Returns the host focus-event DEC private-mode sequence for a frame.
const fn attached_terminal_focus_events_frame(enabled: bool) -> &'static [u8] {
    if enabled {
        b"\x1b[?1004h"
    } else {
        b"\x1b[?1004l"
    }
}

/// Returns the host alternate-screen DEC private-mode sequence for a frame.
///
/// Attached clients are always rendered on the containing terminal's normal
/// screen. Pane alternate-screen state remains pane-local so the host terminal
/// retains its ordinary scrollback while Mezzanine renders the composed view.
const fn attached_terminal_alternate_screen_frame(enabled: bool) -> &'static [u8] {
    let _ = enabled;
    b"\x1b[?1049l"
}

/// Defines the fn const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const fn attached_terminal_restore_presentation_frame() -> &'static [u8] {
    b"\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b>\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[2J\x1b[H\x1b[?25h\x1b[0 q"
}

/// Runs the cursor presentation sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_presentation_sequence(lines: &[String], modes: AttachedTerminalOutputModes) -> String {
    if !cursor_phase_visible(modes) {
        return "\x1b[?25l\x1b[0m".to_string();
    }
    let row = modes
        .cursor_row
        .min(lines.len().saturating_sub(1))
        .saturating_add(1);
    let frame_width = lines
        .iter()
        .map(|line| terminal_line_width(line))
        .max()
        .unwrap_or(1)
        .max(1);
    let column = modes
        .cursor_column
        .min(frame_width.saturating_sub(1))
        .saturating_add(1);
    let style = modes.cursor_style.decscusr_parameter(false);
    format!("\x1b[?25l\x1b[0m\x1b[{style} q\x1b[{row};{column}H\x1b[?25h")
}

/// Runs the cursor phase visible operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_phase_visible(modes: AttachedTerminalOutputModes) -> bool {
    if !modes.cursor_visible {
        return false;
    }
    if !modes.cursor_blink || modes.cursor_blink_interval_ms == 0 {
        return true;
    }
    let visible_ms = (modes.cursor_blink_interval_ms / 2).max(1);
    modes.cursor_blink_elapsed_ms % modes.cursor_blink_interval_ms < visible_ms
}

/// Runs the encode styled terminal line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_styled_terminal_line(line: &str, style_spans: &[TerminalStyleSpan]) -> String {
    let mut encoded = String::new();
    let mut active = GraphicRendition::default();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let sanitized = sanitize_terminal_output_grapheme(grapheme);
        if sanitized.is_empty() {
            continue;
        }
        let rendition = rendition_at_column(style_spans, column);
        if rendition != active {
            encoded.push_str(&sgr_sequence(rendition));
            active = rendition;
        }
        encoded.push_str(sanitized.as_str());
        column = column.saturating_add(terminal_grapheme_width(grapheme));
    }
    encoded
}

/// Returns terminal-display text for one rendered grapheme cluster.
///
/// Rendered pane text is untrusted by the attached terminal writer. Control
/// bytes that reach this final boundary must be removed so only Mezzanine-owned
/// framing, cursor, and SGR sequences can affect the host terminal.
fn sanitize_terminal_output_grapheme(grapheme: &str) -> String {
    grapheme.chars().filter(|ch| !ch.is_control()).collect()
}

/// Returns the active rendition at a display column.
///
/// Spans must be in composition order so later spans can augment earlier ones.
/// This function folds every covering span in that order, preserving earlier
/// attributes when a later overlay leaves them unspecified. Callers must ensure
/// spans are either from [`terminal_styled_lines_from_canvas`] or from
/// canvas-composed sources where later spans represent later composition
/// layers.
fn rendition_at_column(style_spans: &[TerminalStyleSpan], column: usize) -> GraphicRendition {
    style_spans
        .iter()
        .filter(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .fold(GraphicRendition::default(), |active, span| {
            merge_graphic_renditions(active, span.rendition)
        })
}

/// Merges one later style layer into the accumulated active rendition.
///
/// Terminal style spans act as partial overlays rather than full terminal-state
/// snapshots. Later overlays such as copy-selection highlights should keep an
/// earlier diff or syntax foreground unless they explicitly replace that color.
fn merge_graphic_renditions(
    active: GraphicRendition,
    overlay: GraphicRendition,
) -> GraphicRendition {
    GraphicRendition {
        bold: active.bold || overlay.bold,
        dim: active.dim || overlay.dim,
        italic: active.italic || overlay.italic,
        underline: active.underline
            || overlay.underline
            || active.double_underline
            || overlay.double_underline,
        double_underline: active.double_underline || overlay.double_underline,
        strikethrough: active.strikethrough || overlay.strikethrough,
        inverse: active.inverse || overlay.inverse,
        hidden: active.hidden || overlay.hidden,
        foreground: overlay.foreground.or(active.foreground),
        background: overlay.background.or(active.background),
    }
}

/// Runs the sgr sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sgr_sequence(rendition: GraphicRendition) -> String {
    if rendition == GraphicRendition::default() {
        return "\x1b[0m".to_string();
    }
    let mut codes = vec!["0".to_string()];
    if rendition.bold {
        codes.push("1".to_string());
    }
    if rendition.dim {
        codes.push("2".to_string());
    }
    if rendition.italic {
        codes.push("3".to_string());
    }
    if rendition.underline {
        if rendition.double_underline {
            codes.push("21".to_string());
        } else {
            codes.push("4".to_string());
        }
    }
    if rendition.strikethrough {
        codes.push("9".to_string());
    }
    if rendition.inverse {
        codes.push("7".to_string());
    }
    if rendition.hidden {
        codes.push("8".to_string());
    }
    if let Some(color) = rendition.foreground {
        push_sgr_color_codes(&mut codes, color, false);
    }
    if let Some(color) = rendition.background {
        push_sgr_color_codes(&mut codes, color, true);
    }
    format!("\x1b[{}m", codes.join(";"))
}

/// Runs the push sgr color codes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_sgr_color_codes(codes: &mut Vec<String>, color: TerminalColor, background: bool) {
    match color {
        TerminalColor::Indexed(index) if index < 8 => {
            codes.push((u16::from(index) + if background { 40 } else { 30 }).to_string());
        }
        TerminalColor::Indexed(index) if index < 16 => {
            codes.push((u16::from(index - 8) + if background { 100 } else { 90 }).to_string());
        }
        TerminalColor::Indexed(index) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("5".to_string());
            codes.push(index.to_string());
        }
        TerminalColor::Rgb(red, green, blue) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("2".to_string());
            codes.push(red.to_string());
            codes.push(green.to_string());
            codes.push(blue.to_string());
        }
    }
}
