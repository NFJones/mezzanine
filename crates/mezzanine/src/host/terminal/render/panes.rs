//! Window and pane composition for terminal rendering.
//!
//! This module owns conversion from pane screen content into rendered window
//! rows. It keeps zoom handling, rendered pane geometry, divider overlays, and
//! styled/plain pane composition in one place so the facade does not carry the
//! whole rendering pipeline.

use std::collections::BTreeMap;

#[cfg(test)]
use super::{
    DEFAULT_PANE_FRAME_TEMPLATE, DEFAULT_WINDOW_FRAME_TEMPLATE, PaneRenderInput,
    TerminalFramePosition, fit_width, render_window_frame_text,
};
use super::{
    MezError, Result, TerminalClientLoopConfig, TerminalFrameContext, TerminalFrameRenderOptions,
    TerminalScreen, TerminalStyledLine, WindowPresentationOptions, pane_border_rendition,
    pane_divider_rendition, place_group_frame, place_window_frame, plan_window_presentation,
    render_styled_pane_lines, styled_group_frame_line, styled_window_frame_line,
    window_with_group_frame_space, write_styled_merged_pane_frames_on_dividers,
};
#[cfg(test)]
use super::{group_frame_text, render_pane_lines, write_merged_pane_frames_on_dividers};
use mez_mux::layout::{PaneGeometry, Size, Window};
use mez_mux::theme::UiTheme;
/// Runs the draw window from screens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn draw_window_from_screens(
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
) -> Result<Vec<String>> {
    let render_window = window_with_group_frame_space(window, config)?;
    let pane_inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: screens
                .get(&pane.id.to_string())
                .map(TerminalScreen::visible_lines)
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    render_window_with_pane_frame_template(
        &render_window,
        &pane_inputs,
        &config.frame_context,
        TerminalFrameRenderOptions::plain(
            config.window_frames_enabled,
            &config.window_frame_template,
            config.window_frame_position,
        ),
        TerminalFrameRenderOptions::plain(
            config.pane_frames_enabled,
            &config.pane_frame_template,
            config.pane_frame_position,
        ),
    )
    .map(|mut lines| {
        if let Some(frame) =
            group_frame_text(&config.frame_context, usize::from(window.size.columns))
        {
            place_group_frame(&mut lines, frame, window.size.rows);
        }
        lines
    })
}

/// Renders a window while preserving pane SGR style spans in terminal-cell coordinates.
pub fn draw_styled_window_from_screens(
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
) -> Result<Vec<TerminalStyledLine>> {
    let render_window = window_with_group_frame_space(window, config)?;
    let pane_inputs = window
        .panes()
        .iter()
        .map(|pane| StyledPaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: screens
                .get(&pane.id.to_string())
                .map(TerminalScreen::visible_styled_lines)
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut lines = render_styled_window_with_pane_frame_template(
        &render_window,
        &pane_inputs,
        &config.frame_context,
        TerminalFrameRenderOptions::styled(
            config.window_frames_enabled,
            &config.window_frame_template,
            config.window_frame_position,
            config.window_frame_style,
        ),
        TerminalFrameRenderOptions::styled(
            config.pane_frames_enabled,
            &config.pane_frame_template,
            config.pane_frame_position,
            config.pane_frame_style,
        ),
        &config.ui_theme,
    )?;
    if let Some(frame) = styled_group_frame_line(
        &config.frame_context,
        usize::from(window.size.columns),
        config.window_frame_style,
        &config.ui_theme,
    ) {
        place_group_frame(&mut lines, frame, window.size.rows);
    }
    Ok(lines)
}

/// Runs the render window operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn render_window(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    pane_frames_enabled: bool,
) -> Result<Vec<String>> {
    render_window_with_pane_frame_template(
        window,
        pane_inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            false,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(
            pane_frames_enabled,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
}

/// Runs the render window with pane frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn render_window_with_pane_frame_template(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    frame_context: &TerminalFrameContext,
    window_frame: TerminalFrameRenderOptions<'_>,
    pane_frame: TerminalFrameRenderOptions<'_>,
) -> Result<Vec<String>> {
    let plan = plan_window_presentation(
        window,
        WindowPresentationOptions {
            group_frame_visible: false,
            window_frame_visible: window_frame.enabled,
            window_frame_position: window_frame.position,
            pane_frames_visible: pane_frame.enabled,
            pane_frame_position: pane_frame.position,
        },
    )
    .ok_or_else(|| MezError::invalid_state("cannot render a window with no panes"))?;
    let geometries = plan
        .panes
        .iter()
        .map(|pane| pane.geometry)
        .collect::<Vec<_>>();
    let rendered_panes = plan
        .panes
        .iter()
        .map(|render_plan| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == render_plan.source_index)
                .ok_or_else(|| {
                    MezError::invalid_state("presentation plan references a missing pane")
                })?;
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = render_plan.render_region_size;
            Ok(render_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                render_plan.frame_merges_into_divider,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_panes_by_geometry(
        plan.body_size,
        &geometries,
        &rendered_panes,
        window,
        frame_context,
        pane_frame,
    );
    if window_frame.enabled {
        let frame = fit_width(
            &render_window_frame_text(
                window,
                frame_context,
                window_frame.template,
                usize::from(window.size.columns),
            ),
            usize::from(window.size.columns),
        );
        place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
    }
    Ok(lines)
}

/// Carries Styled Pane Render Input state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StyledPaneRenderInput {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<TerminalStyledLine>,
}

/// Runs the render styled window with pane frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_styled_window_with_pane_frame_template(
    window: &Window,
    pane_inputs: &[StyledPaneRenderInput],
    frame_context: &TerminalFrameContext,
    window_frame: TerminalFrameRenderOptions<'_>,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) -> Result<Vec<TerminalStyledLine>> {
    let plan = plan_window_presentation(
        window,
        WindowPresentationOptions {
            group_frame_visible: false,
            window_frame_visible: window_frame.enabled,
            window_frame_position: window_frame.position,
            pane_frames_visible: pane_frame.enabled,
            pane_frame_position: pane_frame.position,
        },
    )
    .ok_or_else(|| MezError::invalid_state("cannot render a window with no panes"))?;
    let geometries = plan
        .panes
        .iter()
        .map(|pane| pane.geometry)
        .collect::<Vec<_>>();
    let rendered_panes = plan
        .panes
        .iter()
        .map(|render_plan| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == render_plan.source_index)
                .ok_or_else(|| {
                    MezError::invalid_state("presentation plan references a missing pane")
                })?;
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = render_plan.render_region_size;
            Ok(render_styled_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                render_plan.frame_merges_into_divider,
                ui_theme,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_styled_panes_by_geometry(
        plan.body_size,
        &geometries,
        &rendered_panes,
        window,
        frame_context,
        pane_frame,
        ui_theme,
    );
    if window_frame.enabled {
        let frame = styled_window_frame_line(
            window,
            frame_context,
            window_frame.template,
            usize::from(window.size.columns),
            window_frame.style,
            ui_theme,
        );
        place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
    }
    Ok(lines)
}

/// Runs the render panes by geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn render_panes_by_geometry(
    size: Size,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<String>],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
) -> Vec<String> {
    mez_mux::render::compose_plain_pane_rows(size, geometries, rendered_panes, |canvas| {
        write_merged_pane_frames_on_dividers(canvas, geometries, window, frame_context, pane_frame);
    })
}

/// Runs the render styled panes by geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_styled_panes_by_geometry(
    size: Size,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<TerminalStyledLine>],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyledLine> {
    mez_mux::render::compose_styled_pane_rows(
        size,
        geometries,
        rendered_panes,
        window.active_pane_index(),
        pane_border_rendition(true, ui_theme),
        pane_divider_rendition(ui_theme),
        |text_canvas, style_canvas| {
            write_styled_merged_pane_frames_on_dividers(
                text_canvas,
                style_canvas,
                geometries,
                window,
                frame_context,
                pane_frame,
                ui_theme,
            );
        },
    )
}
