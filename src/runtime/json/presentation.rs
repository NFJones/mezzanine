//! Rendered client view, theme, cursor, and terminal-style JSON projection.

use super::{
    ClientViewRole, GraphicRendition, RenderedClientView, TerminalColor, TerminalCursorStyle,
    TerminalStyleSpan, UiColorPair, UiTheme, compose_client_presentation_with_styles, json_escape,
    max_viewport_column, max_viewport_row,
};

/// Runs the rendered client view json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn rendered_client_view_json(view: &RenderedClientView) -> String {
    let (presentation_lines, presentation_spans) =
        compose_client_presentation_with_styles(view, None);
    let lines = presentation_lines
        .iter()
        .map(|line| format!(r#""{}""#, json_escape(line)))
        .collect::<Vec<_>>();
    let line_style_spans = terminal_line_style_spans_json(&presentation_spans);
    let agent_prompt_region = view
        .agent_prompt_region
        .map(|region| {
            format!(
                r#"{{"row":{},"column":{},"columns":{},"rows":{}}}"#,
                region.row, region.column, region.columns, region.rows
            )
        })
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"role":"{}","authoritative_size":{{"columns":{},"rows":{}}},"client_size":{{"columns":{},"rows":{}}},"requires_client_scroll":{},"viewport":{{"row":{},"column":{},"max_row":{},"max_column":{}}},"cursor":{{"row":{},"column":{},"visible":{},"style":"{}","blink":{},"blink_interval_ms":{}}},"output_modes":{{"application_keypad":{},"bracketed_paste":{},"host_mouse_reporting":{},"animation_refresh_interval_ms":{}}},"agent_prompt_region":{},"lines":[{}],"line_style_spans":{}}}"#,
        client_view_role_name(view.role),
        view.authoritative_size.columns,
        view.authoritative_size.rows,
        view.client_size.columns,
        view.client_size.rows,
        view.requires_client_scroll,
        view.viewport_row,
        view.viewport_column,
        max_viewport_row(view),
        max_viewport_column(view),
        view.cursor_row,
        view.cursor_column,
        view.cursor_visible,
        terminal_cursor_style_name(view.cursor_style),
        view.cursor_blink,
        view.cursor_blink_interval_ms,
        view.application_keypad,
        view.bracketed_paste,
        view.host_mouse_reporting,
        view.animation_refresh_interval_ms,
        agent_prompt_region,
        lines.join(","),
        line_style_spans
    )
}

/// Runs the ui theme json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ui_theme_json(theme: &UiTheme) -> String {
    format!(
        r#"{{"name":"{}","colors":{},"prompt":{},"agent_prompt":{},"display_overlay":{}}}"#,
        json_escape(&theme.name),
        ui_theme_colors_json(theme),
        ui_color_pair_json(theme.colors.prompt),
        ui_color_pair_json(theme.colors.agent_prompt),
        ui_color_pair_json(theme.colors.display_overlay)
    )
}

/// Runs the ui theme colors json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ui_theme_colors_json(theme: &UiTheme) -> String {
    format!(
        r#"{{"window_frame":{},"window_active":{},"window_inactive":{},"pane_frame_active":{},"pane_frame_inactive":{},"pane_border_active":{},"pane_border_inactive":{},"pane_divider":{},"frame_fill":{},"scroll_indicator":{},"pane_pwd":{},"window_status_uptime":{},"window_status_datetime":{},"prompt":{},"agent_prompt":{},"agent_transcript_user":{},"agent_transcript_assistant":{},"agent_transcript_status":{},"agent_transcript_error":{},"agent_transcript_command":{},"agent_model":{},"agent_reasoning":{},"agent_status_idle":{},"agent_status_running":{},"agent_status_blocked":{},"agent_status_failed":{},"display_overlay":{},"copy_selection":{}}}"#,
        ui_color_pair_json(theme.colors.window_frame),
        ui_color_pair_json(theme.colors.window_active),
        ui_color_pair_json(theme.colors.window_inactive),
        ui_color_pair_json(theme.colors.pane_frame_active),
        ui_color_pair_json(theme.colors.pane_frame_inactive),
        ui_color_pair_json(theme.colors.pane_border_active),
        ui_color_pair_json(theme.colors.pane_border_inactive),
        ui_color_pair_json(theme.colors.pane_divider),
        ui_color_pair_json(theme.colors.frame_fill),
        ui_color_pair_json(theme.colors.scroll_indicator),
        ui_color_pair_json(theme.colors.pane_pwd),
        ui_color_pair_json(theme.colors.window_status_uptime),
        ui_color_pair_json(theme.colors.window_status_datetime),
        ui_color_pair_json(theme.colors.prompt),
        ui_color_pair_json(theme.colors.agent_prompt),
        ui_color_pair_json(theme.colors.agent_transcript_user),
        ui_color_pair_json(theme.colors.agent_transcript_assistant),
        ui_color_pair_json(theme.colors.agent_transcript_status),
        ui_color_pair_json(theme.colors.agent_transcript_error),
        ui_color_pair_json(theme.colors.agent_transcript_command),
        ui_color_pair_json(theme.colors.agent_model),
        ui_color_pair_json(theme.colors.agent_reasoning),
        ui_color_pair_json(theme.colors.agent_status_idle),
        ui_color_pair_json(theme.colors.agent_status_running),
        ui_color_pair_json(theme.colors.agent_status_blocked),
        ui_color_pair_json(theme.colors.agent_status_failed),
        ui_color_pair_json(theme.colors.display_overlay),
        ui_color_pair_json(theme.colors.copy_selection)
    )
}

/// Runs the ui color pair json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ui_color_pair_json(pair: UiColorPair) -> String {
    format!(
        r#"{{"foreground":{},"background":{}}}"#,
        terminal_color_json(Some(pair.foreground)),
        terminal_color_json(Some(pair.background))
    )
}

/// Runs the terminal cursor style name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_cursor_style_name(style: TerminalCursorStyle) -> &'static str {
    match style {
        TerminalCursorStyle::Block => "block",
        TerminalCursorStyle::Underline => "underline",
        TerminalCursorStyle::Bar => "bar",
    }
}

/// Runs the terminal line style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn terminal_line_style_spans_json(
    line_spans: &[Vec<TerminalStyleSpan>],
) -> String {
    let lines = line_spans
        .iter()
        .map(|spans| terminal_style_spans_json(spans))
        .collect::<Vec<_>>();
    format!("[{}]", lines.join(","))
}

/// Runs the terminal style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_style_spans_json(spans: &[TerminalStyleSpan]) -> String {
    let spans = spans
        .iter()
        .map(|span| {
            format!(
                r#"{{"start":{},"length":{},"rendition":{}}}"#,
                span.start,
                span.length,
                terminal_rendition_json(span.rendition)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", spans.join(","))
}

/// Runs the terminal rendition json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_rendition_json(rendition: GraphicRendition) -> String {
    format!(
        r#"{{"bold":{},"dim":{},"italic":{},"underline":{},"double_underline":{},"strikethrough":{},"inverse":{},"hidden":{},"foreground":{},"background":{}}}"#,
        rendition.bold,
        rendition.dim,
        rendition.italic,
        rendition.underline,
        rendition.double_underline,
        rendition.strikethrough,
        rendition.inverse,
        rendition.hidden,
        terminal_color_json(rendition.foreground),
        terminal_color_json(rendition.background)
    )
}

/// Runs the terminal color json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_color_json(color: Option<TerminalColor>) -> String {
    match color {
        Some(TerminalColor::Indexed(index)) => {
            format!(r#"{{"kind":"indexed","index":{index}}}"#)
        }
        Some(TerminalColor::Rgb(red, green, blue)) => {
            format!(r#"{{"kind":"rgb","red":{red},"green":{green},"blue":{blue}}}"#)
        }
        None => "null".to_string(),
    }
}

/// Runs the client view role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn client_view_role_name(role: ClientViewRole) -> &'static str {
    match role {
        ClientViewRole::Primary => "primary",
        ClientViewRole::Observer => "observer",
        ClientViewRole::PendingObserver => "pending_observer",
    }
}
