//! Prompt and pane-local agent footer rendering helpers.
//!
//! This module owns readline prompt presentation, prompt-region overlays,
//! pane-local agent prompt blocks, and live agent footer styling. The parent
//! renderer remains responsible for pane/window composition and calls this
//! module through typed helpers instead of carrying prompt wrapping details
//! inline.

use crate::readline::ReadlinePromptKind;
use crate::terminal::{
    ClientStatusKind, ClientStatusLine, GraphicRendition, ReadlinePrompt,
    ReadlinePromptClientPresentation, ReadlinePromptRegion, ReadlinePromptStatusRow,
    RenderedClientView, Size, TerminalColor, TerminalPaneFrameContext, TerminalStyleSpan,
    TerminalStyledLine, UiColorPair, UiTheme,
};

use super::super::AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS;
use super::style::{
    animated_scan_background, contrasting_binary_foreground, gradient_highlight_for_offset,
    push_or_extend_style_span, terminal_color_contrast_ratio, terminal_color_luminance,
    terminal_color_relative_luminance,
};
use super::text::{
    char_count, fit_width, offset_style_span, terminal_char_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width,
};
use super::{
    AGENT_STATUS_SCAN_BAND_WIDTH, compose_client_presentation_with_styles,
    normalize_overlay_style_spans, overlay_text_style_width, pane_agent_prompt_space_reserved,
};

const MIN_PROMPT_SHADOW_CONTRAST_RATIO: f64 = 4.5;

/// Visual prefix applied to Mezzanine-owned UI lines (status bars, prompts,
/// command overlays) so users can distinguish them from agent-controlled
/// terminal output. Terminal content never receives this prefix.
const MEZ_UI_PREFIX: &str = "▐ ";

/// Clamps a zero-based visible cursor column into the addressable cells of a
/// rendered row. Terminal cursor addressing is one-based and cannot represent a
/// visible insertion point after the final cell without relying on emulator
/// autowrap behavior.
fn clamp_visible_cursor_column(column: usize, width: usize) -> usize {
    column.min(width.saturating_sub(1))
}

/// Runs the render readline prompt status row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn render_readline_prompt_status_row(
    prompt: &ReadlinePrompt,
    width: usize,
) -> ReadlinePromptStatusRow {
    let raw_cursor_column = prompt.rendered_cursor_column();
    let cursor_column = raw_cursor_column
        .saturating_add(2)
        .min(width.saturating_sub(1));
    ReadlinePromptStatusRow {
        status: ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: format!(
                "{MEZ_UI_PREFIX}{}",
                fit_width(&prompt.render_with_shadow_hint(), width.saturating_sub(2))
            ),
        },
        cursor_column,
        cursor_visible: width > 0 && raw_cursor_column <= width.saturating_sub(2),
    }
}

/// Runs the compose readline prompt client presentation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_readline_prompt_client_presentation(
    view: &RenderedClientView,
    prompt: &ReadlinePrompt,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(view.authoritative_size.columns);
    let row = render_readline_prompt_status_row(prompt, width);
    let (lines, mut line_style_spans) =
        compose_client_presentation_with_styles(view, Some(&row.status));
    if let Some(last) = line_style_spans.last_mut() {
        let presentation_width = lines
            .last()
            .map(|line| terminal_text_width(line))
            .unwrap_or(width);
        if prompt.kind == ReadlinePromptKind::Agent && presentation_width > 0 {
            last.clear();
            last.push(TerminalStyleSpan {
                start: 0,
                length: presentation_width,
                rendition: agent_prompt_input_rendition(&view.ui_theme),
            });
        }
        if let Some(span) =
            prompt_shadow_hint_style_span(prompt, 2, presentation_width, &view.ui_theme)
        {
            last.push(span);
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: usize::from(view.authoritative_size.rows.saturating_sub(1)),
        cursor_column: row.cursor_column,
        cursor_visible: row.cursor_visible,
    }
}

/// Runs the compose prompt overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_lines(
    base_lines: &[String],
    prompt: &ReadlinePrompt,
    client_size: Size,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let status_row = render_readline_prompt_status_row(prompt, width);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    if let Some(last) = lines.last_mut() {
        *last = fit_width(&status_row.status.text, width);
    }
    lines
}

/// Runs the compose prompt overlay presentation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_presentation(
    base_lines: &[String],
    prompt: &ReadlinePrompt,
    client_size: Size,
) -> ReadlinePromptClientPresentation {
    compose_prompt_overlay_presentation_with_styles(
        base_lines,
        &[],
        prompt,
        client_size,
        &UiTheme::default(),
    )
}

/// Runs the compose prompt overlay presentation with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_presentation_with_styles(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    prompt: &ReadlinePrompt,
    client_size: Size,
    ui_theme: &UiTheme,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let status_row = render_readline_prompt_status_row(prompt, width);
    let lines = compose_prompt_overlay_lines(base_lines, prompt, client_size);
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    line_style_spans.truncate(rows);
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }
    if let Some(last) = line_style_spans.last_mut() {
        last.clear();
        if width > 0 {
            last.push(TerminalStyleSpan {
                start: 0,
                length: width,
                rendition: prompt_region_rendition(prompt, ui_theme),
            });
            if let Some(span) = prompt_shadow_hint_style_span(prompt, 2, width, ui_theme) {
                last.push(span);
            }
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: rows.saturating_sub(1),
        cursor_column: status_row.cursor_column,
        cursor_visible: status_row.cursor_visible,
    }
}

/// Runs the compose prompt region presentation with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_region_presentation_with_styles(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    prompt: &ReadlinePrompt,
    client_size: Size,
    region: ReadlinePromptRegion,
    ui_theme: &UiTheme,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    line_style_spans.truncate(rows);
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }

    let region = clipped_prompt_region(region, width, rows);
    let Some(region) = region else {
        return ReadlinePromptClientPresentation {
            lines,
            line_style_spans,
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    };
    let layout = render_wrapped_prompt_layout(prompt, region.columns, region.rows.clamp(1, 6));
    let prompt_row_start = if prompt.kind == ReadlinePromptKind::Agent && layout.lines.len() > 1 {
        region.row
    } else {
        region
            .row
            .saturating_add(region.rows.saturating_sub(layout.lines.len()))
    };
    for (offset, prompt_line) in layout.lines.iter().enumerate() {
        let row = prompt_row_start.saturating_add(offset);
        if row >= lines.len() {
            continue;
        }
        write_line_segment(&mut lines[row], region.column, region.columns, prompt_line);
        line_style_spans[row].retain(|span| {
            span.start.saturating_add(span.length) <= region.column
                || span.start >= region.column.saturating_add(region.columns)
        });
        line_style_spans[row].push(TerminalStyleSpan {
            start: region.column,
            length: region.columns,
            rendition: prompt_region_rendition(prompt, ui_theme),
        });
        for shadow_span in layout.shadow_spans.get(offset).into_iter().flatten() {
            if shadow_span.start >= region.columns {
                continue;
            }
            let length = shadow_span
                .length
                .min(region.columns.saturating_sub(shadow_span.start));
            if length == 0 {
                continue;
            }
            line_style_spans[row].push(TerminalStyleSpan {
                start: region.column.saturating_add(shadow_span.start),
                length,
                rendition: prompt_shadow_hint_rendition(prompt, ui_theme),
            });
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: prompt_row_start.saturating_add(layout.cursor_row),
        cursor_column: region.column.saturating_add(layout.cursor_column),
        cursor_visible: layout.cursor_visible,
    }
}

/// Returns the number of pane body rows reserved by the pane-local agent prompt.
pub fn agent_prompt_reserved_line_count(
    width: usize,
    body_rows: usize,
    pane_context: Option<&TerminalPaneFrameContext>,
) -> usize {
    if !pane_agent_prompt_space_reserved(pane_context) {
        return 0;
    }
    render_agent_prompt_block(width, body_rows, pane_context).reserved_line_count()
}

/// Runs the prompt region rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_region_rendition(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> GraphicRendition {
    if prompt.kind == ReadlinePromptKind::Agent {
        agent_prompt_input_rendition(ui_theme)
    } else {
        ui_theme.colors.prompt.rendition()
    }
}

/// Runs the prompt shadow hint rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_shadow_hint_rendition(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> GraphicRendition {
    let mut rendition = prompt_region_rendition(prompt, ui_theme);
    rendition.foreground = Some(prompt_shadow_foreground(prompt, ui_theme));
    rendition.dim = true;
    rendition
}

/// Returns the contrast-managed shadow-hint rendition for pane-local agent prompts.
fn agent_prompt_shadow_hint_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    let background = ui_theme.colors.agent_prompt.background;
    let mut rendition = agent_prompt_input_rendition(ui_theme);
    rendition.foreground = Some(
        readable_prompt_shadow_gray(background).unwrap_or(ui_theme.colors.agent_prompt.foreground),
    );
    rendition.dim = true;
    rendition
}

/// Returns the contrast-managed rendition for pane-local agent prompt input.
pub(super) fn agent_prompt_input_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    let background = ui_theme.colors.agent_prompt.background;
    GraphicRendition {
        foreground: Some(contrasting_binary_foreground(background)),
        background: Some(background),
        ..GraphicRendition::default()
    }
}

/// Returns a readable shaded foreground for completion shadow text.
fn prompt_shadow_foreground(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> TerminalColor {
    let background = if prompt.kind == ReadlinePromptKind::Agent {
        ui_theme.colors.agent_prompt.background
    } else {
        ui_theme.colors.prompt.background
    };
    readable_prompt_shadow_gray(background).unwrap_or_else(|| {
        if prompt.kind == ReadlinePromptKind::Agent {
            ui_theme.colors.agent_prompt.foreground
        } else {
            ui_theme.colors.prompt.foreground
        }
    })
}

/// Returns the lowest-emphasis grey that still reads against a prompt surface.
fn readable_prompt_shadow_gray(background: TerminalColor) -> Option<TerminalColor> {
    let background_luminance = terminal_color_relative_luminance(background)?;
    if background_luminance >= 0.5 {
        for level in (0..=255).rev() {
            let candidate = terminal_gray(level);
            if terminal_color_contrast_ratio(candidate, background)
                .is_some_and(|ratio| ratio >= MIN_PROMPT_SHADOW_CONTRAST_RATIO)
            {
                return Some(candidate);
            }
        }
    } else {
        for level in 0..=255 {
            let candidate = terminal_gray(level);
            if terminal_color_contrast_ratio(candidate, background)
                .is_some_and(|ratio| ratio >= MIN_PROMPT_SHADOW_CONTRAST_RATIO)
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Returns a text-only rendition for Mezzanine-authored pane and overlay text.
///
/// These surfaces should color foreground glyphs without painting a background
/// over terminal content. Interactive controls such as prompts, status bars,
/// buttons, and selectors keep using their full color pair renditions.
fn text_foreground_rendition(pair: UiColorPair) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    }
}

/// Returns the display-overlay foreground rendition used for non-interactive
/// command output, help text, and pane-local reference output.
pub(super) fn display_overlay_text_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    text_foreground_rendition(ui_theme.colors.display_overlay)
}

/// Runs the prompt shadow hint style span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_shadow_hint_style_span(
    prompt: &ReadlinePrompt,
    rendered_column_offset: usize,
    width: usize,
    ui_theme: &UiTheme,
) -> Option<TerminalStyleSpan> {
    let (start, length) = prompt.rendered_shadow_hint_columns()?;
    let start = start.saturating_add(rendered_column_offset);
    let end = start.saturating_add(length).min(width);
    (start < end).then_some(TerminalStyleSpan {
        start,
        length: end.saturating_sub(start),
        rendition: prompt_shadow_hint_rendition(prompt, ui_theme),
    })
}

/// Runs the compose display region overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_region_overlay_lines(
    base_lines: &[String],
    display_lines: &[String],
    client_size: Size,
    region: ReadlinePromptRegion,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    let Some(region) = clipped_prompt_region(region, width, rows) else {
        return lines;
    };
    let display_capacity = region.rows.saturating_sub(1).max(1);
    let visible_count = display_lines.len().min(display_capacity);
    let start = display_lines.len().saturating_sub(visible_count);
    let row_start = region
        .row
        .saturating_add(region.rows.saturating_sub(visible_count.saturating_add(1)));
    for (offset, line) in display_lines
        .iter()
        .skip(start)
        .take(visible_count)
        .enumerate()
    {
        let row = row_start.saturating_add(offset);
        if row < lines.len() {
            write_line_segment(&mut lines[row], region.column, region.columns, line);
        }
    }
    lines
}

/// Runs the compose display region overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_region_overlay_line_style_spans(
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    display_lines: &[String],
    client_size: Size,
    region: ReadlinePromptRegion,
    ui_theme: &UiTheme,
) -> Vec<Vec<TerminalStyleSpan>> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    let Some(region) = clipped_prompt_region(region, width, rows) else {
        return line_style_spans;
    };
    let display_capacity = region.rows.saturating_sub(1).max(1);
    let visible_count = display_lines.len().min(display_capacity);
    let start = display_lines.len().saturating_sub(visible_count);
    let row_start = region
        .row
        .saturating_add(region.rows.saturating_sub(visible_count.saturating_add(1)));
    for offset in 0..visible_count {
        let row = row_start.saturating_add(offset);
        if row >= line_style_spans.len() {
            continue;
        }
        line_style_spans[row].retain(|span| {
            span.start.saturating_add(span.length) <= region.column
                || span.start >= region.column.saturating_add(region.columns)
        });
        let display_line = &display_lines[start + offset];
        let footer_spans =
            agent_live_footer_style_spans(display_line, region.columns, 0, ui_theme, None);
        if footer_spans.is_empty() {
            line_style_spans[row].push(TerminalStyleSpan {
                start: region.column,
                length: overlay_text_style_width(display_line, region.columns),
                rendition: display_overlay_text_rendition(ui_theme),
            });
        } else {
            line_style_spans[row].extend(
                footer_spans
                    .into_iter()
                    .map(|span| offset_style_span(span, region.column)),
            );
        }
    }
    line_style_spans
}

/// Carries Wrapped Prompt Layout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WrappedPromptLayout {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<String>,
    /// Stores the shadow spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    shadow_spans: Vec<Vec<PromptShadowSpan>>,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_visible: bool,
}

/// Carries Prompt Shadow Span state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptShadowSpan {
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the length value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    length: usize,
}

/// Runs the render wrapped prompt layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_wrapped_prompt_layout(
    prompt: &ReadlinePrompt,
    width: usize,
    max_rows: usize,
) -> WrappedPromptLayout {
    if width == 0 || max_rows == 0 {
        return WrappedPromptLayout {
            lines: Vec::new(),
            shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    }
    let raw_line = format!("{MEZ_UI_PREFIX}{}", prompt.render_with_shadow_hint());
    let raw_cursor_index = prompt.rendered_cursor_column().saturating_add(2);
    let raw_shadow_range = prompt
        .rendered_shadow_hint_columns()
        .map(|(start, length)| (start.saturating_add(2), start.saturating_add(2 + length)));
    let continuation_indent =
        if prompt.kind == ReadlinePromptKind::Agent && !prompt.reverse_search_active() {
            terminal_text_width(&format!("{MEZ_UI_PREFIX}mez> ")).min(width.saturating_sub(1))
        } else {
            0
        };
    let (chunks, chunk_shadow_spans, cursor_row, cursor_column) =
        wrap_prompt_line_with_cursor_and_shadow(
            &raw_line,
            raw_cursor_index,
            raw_shadow_range,
            width,
            continuation_indent,
        );
    let first_visible_chunk = chunks.len().saturating_sub(max_rows);
    let visible_chunks = chunks
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    let mut visible_shadow_spans = chunk_shadow_spans
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .cloned()
        .collect::<Vec<_>>();
    let cursor_visible = cursor_row >= first_visible_chunk
        && cursor_row < first_visible_chunk + visible_chunks.len();
    let mut lines = visible_chunks;
    let mut cursor_column = cursor_column;
    if should_show_prompt_length_note(prompt, width, max_rows)
        && let Some(first) = lines.first_mut()
    {
        let note = format!(
            "{MEZ_UI_PREFIX}mez> [{} chars pasted]",
            prompt.buffer.line().chars().count()
        );
        *first = fit_width(&note, width);
        if let Some(first_spans) = visible_shadow_spans.first_mut() {
            first_spans.clear();
        }
        cursor_column = width;
    }
    let cursor_column = clamp_visible_cursor_column(cursor_column, width);
    WrappedPromptLayout {
        lines,
        shadow_spans: visible_shadow_spans,
        cursor_row: cursor_row.saturating_sub(first_visible_chunk),
        cursor_column,
        cursor_visible,
    }
}

/// Runs the should show prompt length note operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn should_show_prompt_length_note(prompt: &ReadlinePrompt, width: usize, max_rows: usize) -> bool {
    prompt.kind == ReadlinePromptKind::Agent
        && char_count(prompt.buffer.line()) > width.saturating_mul(max_rows).max(160)
}

/// Runs the wrap prompt line with cursor and shadow operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wrap_prompt_line_with_cursor_and_shadow(
    value: &str,
    cursor_index: usize,
    shadow_range: Option<(usize, usize)>,
    width: usize,
    continuation_indent: usize,
) -> (Vec<String>, Vec<Vec<PromptShadowSpan>>, usize, usize) {
    let mut chunks = Vec::new();
    let mut chunk_shadow_spans = Vec::new();
    let mut current = String::new();
    let mut current_shadow_spans = Vec::new();
    let mut used = 0usize;
    let mut cursor = None;
    let mut last_space_break: Option<(usize, usize, Vec<PromptShadowSpan>)> = None;
    let continuation_prefix = " ".repeat(continuation_indent);
    for (index, ch) in value.chars().enumerate() {
        if ch == '\n' {
            if cursor.is_none() && index == cursor_index {
                cursor = Some((chunks.len(), used));
            }
            chunks.push(current);
            chunk_shadow_spans.push(current_shadow_spans);
            current = continuation_prefix.clone();
            current_shadow_spans = Vec::new();
            used = continuation_indent;
            last_space_break = None;
            continue;
        }
        let ch_width = terminal_char_width(ch).max(1);
        if used > 0 && used.saturating_add(ch_width) > width {
            if let Some((text_break, consumed_break, spans_at_break)) = last_space_break.take() {
                let consumed_columns = terminal_text_width(&current[..consumed_break]);
                if consumed_columns > continuation_indent {
                    let wrapped = current[..text_break].to_string();
                    let remainder = current[consumed_break..].to_string();
                    chunks.push(wrapped);
                    chunk_shadow_spans.push(spans_at_break);
                    current = format!("{continuation_prefix}{remainder}");
                    current_shadow_spans = prompt_shadow_spans_after_consumed(
                        &current_shadow_spans,
                        consumed_columns,
                        continuation_indent,
                    );
                    used = terminal_text_width(&current);
                } else {
                    chunks.push(current);
                    chunk_shadow_spans.push(current_shadow_spans);
                    current = continuation_prefix.clone();
                    current_shadow_spans = Vec::new();
                    used = continuation_indent;
                }
            } else {
                chunks.push(current);
                chunk_shadow_spans.push(current_shadow_spans);
                current = continuation_prefix.clone();
                current_shadow_spans = Vec::new();
                used = continuation_indent;
            }
        }
        if cursor.is_none() && index == cursor_index {
            cursor = Some((chunks.len(), used));
        }
        let current_byte_len = current.len();
        current.push(ch);
        if shadow_range.is_some_and(|(start, end)| index >= start && index < end) {
            push_prompt_shadow_cell(&mut current_shadow_spans, used, ch_width);
        }
        used = used.saturating_add(ch_width);
        if ch.is_whitespace() && used > 0 {
            last_space_break = Some((
                current_byte_len,
                current.len(),
                current_shadow_spans.clone(),
            ));
        }
    }
    if cursor.is_none() && value.chars().count() == cursor_index {
        cursor = Some((chunks.len(), used));
    }
    chunks.push(current);
    chunk_shadow_spans.push(current_shadow_spans);
    let (cursor_row, cursor_column) = cursor.unwrap_or((chunks.len().saturating_sub(1), 0));
    (chunks, chunk_shadow_spans, cursor_row, cursor_column)
}

/// Returns prompt-shadow spans after one wrapped prefix is consumed.
fn prompt_shadow_spans_after_consumed(
    spans: &[PromptShadowSpan],
    consumed_columns: usize,
    shift_columns: usize,
) -> Vec<PromptShadowSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let end = span.start.saturating_add(span.length);
            if end <= consumed_columns {
                None
            } else {
                Some(PromptShadowSpan {
                    start: span
                        .start
                        .saturating_sub(consumed_columns)
                        .saturating_add(shift_columns),
                    length: end.saturating_sub(consumed_columns.max(span.start)),
                })
            }
        })
        .collect()
}

/// Runs the push prompt shadow cell operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_prompt_shadow_cell(
    current_shadow_spans: &mut Vec<PromptShadowSpan>,
    start: usize,
    length: usize,
) {
    if let Some(last) = current_shadow_spans.last_mut()
        && last.start.saturating_add(last.length) == start
    {
        last.length = last.length.saturating_add(length);
        return;
    }
    current_shadow_spans.push(PromptShadowSpan { start, length });
}

/// Runs the clipped prompt region operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn clipped_prompt_region(
    region: ReadlinePromptRegion,
    client_width: usize,
    client_rows: usize,
) -> Option<ReadlinePromptRegion> {
    if region.row >= client_rows || region.column >= client_width {
        return None;
    }
    let columns = region
        .columns
        .min(client_width.saturating_sub(region.column));
    let rows = region.rows.min(client_rows.saturating_sub(region.row));
    (columns > 0 && rows > 0).then_some(ReadlinePromptRegion {
        row: region.row,
        column: region.column,
        columns,
        rows,
    })
}

/// Runs the write line segment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_line_segment(line: &mut String, column: usize, width: usize, value: &str) {
    if width == 0 {
        return;
    }
    let target_end = column.saturating_add(width);
    let original = line.clone();
    let mut output = String::new();
    let mut current_column = 0usize;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        let next_column = current_column.saturating_add(grapheme_width);
        if next_column <= column {
            output.push_str(grapheme);
            current_column = next_column;
            continue;
        }
        break;
    }
    let output_width = terminal_text_width(&output);
    if output_width < column {
        output.push_str(&" ".repeat(column.saturating_sub(output_width)));
    }
    let fitted = fit_width(value, width);
    output.push_str(&fitted);
    current_column = 0;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if current_column >= target_end {
            output.push_str(grapheme);
        }
        current_column = current_column.saturating_add(grapheme_width);
    }
    *line = output;
}

/// Carries Agent Prompt Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentPromptBlock {
    /// Stores the display lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) display_lines: Vec<String>,
    /// Stores the prompt lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) prompt_lines: Vec<String>,
    /// Stores shadow-completion style spans for each prompt line.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) prompt_shadow_spans: Vec<Vec<PromptShadowSpan>>,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor_visible: bool,
}

impl AgentPromptBlock {
    /// Returns the persistent number of pane rows reserved for prompt input.
    pub(super) fn reserved_line_count(&self) -> usize {
        self.prompt_lines.len()
    }

    /// Returns styled transient display lines for the prompt block.
    pub(super) fn display_styled_lines(
        &self,
        width: usize,
        ui_theme: &UiTheme,
        animation_tick_ms: u64,
    ) -> Vec<TerminalStyledLine> {
        let mut lines = Vec::with_capacity(self.display_lines.len());
        for line in &self.display_lines {
            if agent_live_footer_state_label(line).is_some() {
                lines.push(agent_live_footer_styled_line(
                    line,
                    width,
                    animation_tick_ms,
                    ui_theme,
                ));
            } else {
                lines.push(themed_text_line(
                    line,
                    width,
                    display_overlay_text_rendition(ui_theme),
                ));
            }
        }
        lines
    }

    /// Returns styled persistent prompt-input lines for the prompt block.
    pub(super) fn prompt_styled_lines(
        &self,
        width: usize,
        ui_theme: &UiTheme,
        animation_tick_ms: u64,
    ) -> Vec<TerminalStyledLine> {
        let mut lines = Vec::with_capacity(self.prompt_lines.len());
        for (line_index, line) in self.prompt_lines.iter().enumerate() {
            let mut styled_line =
                themed_full_width_line(line, width, agent_prompt_input_rendition(ui_theme));
            for shadow_span in self
                .prompt_shadow_spans
                .get(line_index)
                .into_iter()
                .flatten()
            {
                if shadow_span.start >= width {
                    continue;
                }
                let length = shadow_span
                    .length
                    .min(width.saturating_sub(shadow_span.start));
                if length == 0 {
                    continue;
                }
                styled_line.style_spans.push(TerminalStyleSpan {
                    start: shadow_span.start,
                    length,
                    rendition: agent_prompt_shadow_hint_rendition(ui_theme),
                });
            }
            if let Some((footer_start, footer_text)) = agent_prompt_line_live_footer_suffix(line) {
                styled_line.style_spans.extend(
                    agent_live_footer_style_spans(
                        footer_text,
                        width.saturating_sub(footer_start),
                        animation_tick_ms,
                        ui_theme,
                        Some(ui_theme.colors.agent_prompt.background),
                    )
                    .into_iter()
                    .map(|span| offset_style_span(span, footer_start)),
                );
            }
            lines.push(styled_line);
        }
        lines
    }

    /// Runs the transparent styled lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn transparent_prompt_styled_lines(&self, width: usize) -> Vec<TerminalStyledLine> {
        (0..self.reserved_line_count())
            .map(|_| TerminalStyledLine::plain(" ".repeat(width)))
            .collect()
    }

    /// Returns plain transient display lines for the prompt block.
    pub(super) fn display_plain_lines(&self) -> Vec<String> {
        self.display_lines.clone()
    }

    /// Returns plain persistent prompt-input lines for the prompt block.
    pub(super) fn prompt_plain_lines(&self) -> Vec<String> {
        self.prompt_lines.clone()
    }

    /// Runs the transparent plain lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn transparent_prompt_plain_lines(&self, width: usize) -> Vec<String> {
        (0..self.reserved_line_count())
            .map(|_| " ".repeat(width))
            .collect()
    }
}

/// Runs the render agent prompt block operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_agent_prompt_block(
    width: usize,
    body_rows: usize,
    pane_context: Option<&TerminalPaneFrameContext>,
) -> AgentPromptBlock {
    if width == 0 || body_rows == 0 {
        return AgentPromptBlock {
            display_lines: Vec::new(),
            prompt_lines: Vec::new(),
            prompt_shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    }
    let prompt = pane_context
        .and_then(|context| context.agent_prompt.clone())
        .unwrap_or_else(|| ReadlinePrompt::new(ReadlinePromptKind::Agent));
    let display_source = pane_context
        .map(|context| context.agent_display_lines.as_slice())
        .unwrap_or(&[]);
    let (display_source, live_footer) = split_agent_live_footer_display_source(display_source);
    let prompt_layout = if prompt_can_show_agent_live_footer(&prompt) {
        live_footer
            .map(|footer| render_agent_live_footer_prompt_layout(&prompt, footer, width))
            .unwrap_or_else(|| render_wrapped_prompt_layout(&prompt, width, body_rows.clamp(1, 6)))
    } else {
        render_wrapped_prompt_layout(&prompt, width, body_rows.clamp(1, 6))
    };
    let display_capacity = body_rows.saturating_sub(prompt_layout.lines.len());
    let display_count = display_source.len().min(display_capacity);
    let display_start = display_source.len().saturating_sub(display_count);
    let display_lines = display_source
        .iter()
        .skip(display_start)
        .take(display_count)
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    AgentPromptBlock {
        display_lines,
        prompt_lines: prompt_layout.lines,
        prompt_shadow_spans: prompt_layout.shadow_spans,
        cursor_row: prompt_layout.cursor_row,
        cursor_column: prompt_layout.cursor_column,
        cursor_visible: prompt_layout.cursor_visible,
    }
}

/// Separates the live agent footer from regular pane-local display rows.
fn split_agent_live_footer_display_source(lines: &[String]) -> (&[String], Option<&str>) {
    match lines.split_last() {
        Some((last, rest)) if agent_live_footer_state_label(last).is_some() => {
            (rest, Some(last.as_str()))
        }
        _ => (lines, None),
    }
}

/// Returns whether the empty prompt row may be used as live status space.
fn prompt_can_show_agent_live_footer(prompt: &ReadlinePrompt) -> bool {
    prompt.kind == ReadlinePromptKind::Agent
        && prompt.buffer.line().is_empty()
        && prompt.selector.is_none()
}

/// Builds a one-row prompt layout that renders live agent status as placeholder text.
fn render_agent_live_footer_prompt_layout(
    prompt: &ReadlinePrompt,
    footer: &str,
    width: usize,
) -> WrappedPromptLayout {
    let prompt_prefix = format!("{MEZ_UI_PREFIX}{}", prompt.render());
    let line = format!("{prompt_prefix}{footer}");
    let cursor_column = prompt.rendered_cursor_column().saturating_add(2);
    WrappedPromptLayout {
        lines: vec![fit_width(&line, width)],
        shadow_spans: vec![Vec::new()],
        cursor_row: 0,
        cursor_column: clamp_visible_cursor_column(cursor_column, width),
        cursor_visible: cursor_column < width,
    }
}

/// Finds a live footer suffix embedded after the agent prompt prefix.
fn agent_prompt_line_live_footer_suffix(line: &str) -> Option<(usize, &str)> {
    for (byte_index, _) in line.char_indices() {
        let suffix = &line[byte_index..];
        if agent_live_footer_state_label(suffix)
            .is_some_and(|label| !label.contains('>') && !label.contains(MEZ_UI_PREFIX.trim()))
        {
            return Some((terminal_text_width(&line[..byte_index]), suffix));
        }
    }
    None
}

/// Runs the themed full width line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn themed_full_width_line(
    line: &str,
    width: usize,
    rendition: GraphicRendition,
) -> TerminalStyledLine {
    TerminalStyledLine {
        text: fit_width(line, width),
        style_spans: (width > 0)
            .then_some(TerminalStyleSpan {
                start: 0,
                length: width,
                rendition,
            })
            .into_iter()
            .collect(),
        copy_text: None,
    }
}

/// Returns a styled line whose Mezzanine-owned text is colored but whose
/// padding cells remain transparent to the terminal background.
fn themed_text_line(line: &str, width: usize, rendition: GraphicRendition) -> TerminalStyledLine {
    let text = fit_width(line, width);
    let length = overlay_text_style_width(line, width);
    TerminalStyledLine {
        text,
        style_spans: (length > 0)
            .then_some(TerminalStyleSpan {
                start: 0,
                length,
                rendition,
            })
            .into_iter()
            .collect(),
        copy_text: None,
    }
}

/// Builds the live agent-turn footer using foreground-only grayscale motion.
fn agent_live_footer_styled_line(
    line: &str,
    width: usize,
    animation_tick_ms: u64,
    ui_theme: &UiTheme,
) -> TerminalStyledLine {
    let text = fit_width(line, width);
    let style_spans =
        agent_live_footer_style_spans(&text, width, animation_tick_ms, ui_theme, None);
    TerminalStyledLine {
        text,
        style_spans,
        copy_text: None,
    }
}

/// Builds foreground-only style spans for the state label and hint in a live footer.
pub(super) fn agent_live_footer_style_spans(
    line: &str,
    width: usize,
    animation_tick_ms: u64,
    ui_theme: &UiTheme,
    background: Option<TerminalColor>,
) -> Vec<TerminalStyleSpan> {
    let text = fit_width(line, width);
    let mut style_spans = Vec::new();
    let visible_width = overlay_text_style_width(&text, width);
    let state_label_width = agent_live_footer_state_label(&text)
        .map(terminal_text_width)
        .unwrap_or(0);
    if state_label_width == 0 || visible_width == 0 {
        return style_spans;
    }
    let base = agent_live_footer_base_gray(ui_theme);
    let palette = agent_live_footer_grayscale_palette(ui_theme);
    let parenthetical_rendition = agent_live_footer_parenthetical_rendition(ui_theme);
    let phase = ((animation_tick_ms / AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS) as usize)
        % state_label_width.saturating_add(AGENT_STATUS_SCAN_BAND_WIDTH);
    let center = phase as isize - (AGENT_STATUS_SCAN_BAND_WIDTH as isize / 2);
    let mut column = 0usize;
    for grapheme in terminal_graphemes(&text) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if grapheme_width == 0 {
            continue;
        }
        if column < state_label_width && !grapheme.chars().all(char::is_whitespace) {
            let offset = column as isize - center;
            let distance = offset.unsigned_abs();
            let intensity = AGENT_STATUS_SCAN_BAND_WIDTH.saturating_sub(distance);
            let highlight = gradient_highlight_for_offset(&palette, offset);
            let foreground =
                animated_scan_background(base, highlight, intensity, AGENT_STATUS_SCAN_BAND_WIDTH);
            push_or_extend_style_span(
                &mut style_spans,
                TerminalStyleSpan {
                    start: column,
                    length: grapheme_width,
                    rendition: GraphicRendition {
                        foreground: Some(foreground),
                        background,
                        ..GraphicRendition::default()
                    },
                },
            );
        } else if column < visible_width && column >= state_label_width {
            let mut rendition = parenthetical_rendition;
            rendition.background = background;
            push_or_extend_style_span(
                &mut style_spans,
                TerminalStyleSpan {
                    start: column,
                    length: grapheme_width.min(visible_width.saturating_sub(column)),
                    rendition,
                },
            );
        }
        column = column.saturating_add(grapheme_width);
    }
    style_spans
}

/// Returns the active state label at the front of a live agent footer.
pub(super) fn agent_live_footer_state_label(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let (state, rest) = line.split_once(" (")?;
    (!state.is_empty() && rest.ends_with(" • esc to interrupt)")).then_some(state)
}

/// Returns the muted rendition used for the timer and interrupt hint.
fn agent_live_footer_parenthetical_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(agent_live_footer_parenthetical_gray(ui_theme)),
        ..GraphicRendition::default()
    }
}

/// Returns a dim neutral gray for the non-animated footer parenthetical.
fn agent_live_footer_parenthetical_gray(ui_theme: &UiTheme) -> TerminalColor {
    let level = i16::from(agent_live_footer_gray_level(ui_theme));
    let background_is_light = agent_live_footer_background_is_light(ui_theme);
    let shift = if background_is_light { 34 } else { -30 };
    let lower = if background_is_light { 0x58 } else { 0x78 };
    let upper = if background_is_light { 0x98 } else { 0xb8 };
    terminal_gray((level + shift).clamp(lower, upper) as u8)
}

/// Returns the theme-relative neutral gray used as the live footer baseline.
fn agent_live_footer_base_gray(ui_theme: &UiTheme) -> TerminalColor {
    let level = agent_live_footer_gray_level(ui_theme);
    terminal_gray(level)
}

/// Returns a grayscale scan palette that mirrors active pane-pill motion.
fn agent_live_footer_grayscale_palette(ui_theme: &UiTheme) -> [TerminalColor; 3] {
    let base = i16::from(agent_live_footer_gray_level(ui_theme));
    if agent_live_footer_background_is_light(ui_theme) {
        [
            terminal_gray((base + 34).clamp(0x30, 0xa8) as u8),
            terminal_gray((base - 18).clamp(0x30, 0xa8) as u8),
            terminal_gray((base - 46).clamp(0x30, 0xa8) as u8),
        ]
    } else {
        [
            terminal_gray((base - 34).clamp(0x68, 0xe8) as u8),
            terminal_gray((base + 22).clamp(0x68, 0xe8) as u8),
            terminal_gray((base + 50).clamp(0x68, 0xe8) as u8),
        ]
    }
}

/// Derives a readable neutral gray from the active display surface.
fn agent_live_footer_gray_level(ui_theme: &UiTheme) -> u8 {
    if agent_live_footer_background_is_light(ui_theme) {
        0x54
    } else {
        0xb0
    }
}

/// Returns whether the footer should use dark grayscale text.
fn agent_live_footer_background_is_light(ui_theme: &UiTheme) -> bool {
    terminal_color_luminance(ui_theme.colors.display_overlay.background)
        .or_else(|| terminal_color_luminance(ui_theme.colors.frame_fill.background))
        .is_some_and(|luminance| luminance >= 140)
}

/// Builds an RGB gray terminal color.
fn terminal_gray(level: u8) -> TerminalColor {
    TerminalColor::Rgb(level, level, level)
}
