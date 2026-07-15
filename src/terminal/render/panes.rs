//! Window and pane composition for terminal rendering.
//!
//! This module owns conversion from pane screen content into rendered window
//! rows. It keeps zoom handling, rendered pane geometry, divider overlays, and
//! styled/plain pane composition in one place so the facade does not carry the
//! whole rendering pipeline.

use super::*;
/// Runs the draw window from screens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
pub fn render_window_with_pane_frame_template(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    frame_context: &TerminalFrameContext,
    window_frame: TerminalFrameRenderOptions<'_>,
    pane_frame: TerminalFrameRenderOptions<'_>,
) -> Result<Vec<String>> {
    if window.panes().is_empty() {
        return Err(MezError::invalid_state(
            "cannot render a window with no panes",
        ));
    }
    let body_size = window_body_size(window.size, window_frame.enabled)?;

    if let Some(rendered) =
        zoomed_pane_render_input(window, pane_inputs, frame_context, pane_frame, body_size)
    {
        let zoomed_geometry = zoomed_pane_geometry(window.active_pane_index(), body_size);
        let mut lines = render_panes_by_geometry(
            body_size,
            &[zoomed_geometry],
            &[rendered],
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
        return Ok(lines);
    }

    let geometries = rendered_pane_geometries(window, window_frame.enabled)?;
    let rendered_panes = geometries
        .iter()
        .map(|geometry| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)
                .unwrap_or_else(|| window.active_pane());
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = pane_render_region_size_for_geometry(geometry, &geometries)?;
            let merges = pane_frame.enabled
                && pane_frame_merges_into_divider(geometry, &geometries, pane_frame.position);
            Ok(render_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                merges,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_panes_by_geometry(
        body_size,
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
    if window.panes().is_empty() {
        return Err(MezError::invalid_state(
            "cannot render a window with no panes",
        ));
    }
    let body_size = window_body_size(window.size, window_frame.enabled)?;

    if let Some(rendered) = zoomed_styled_pane_render_input(
        window,
        pane_inputs,
        frame_context,
        pane_frame,
        body_size,
        ui_theme,
    ) {
        let zoomed_geometry = zoomed_pane_geometry(window.active_pane_index(), body_size);
        let mut lines = render_styled_panes_by_geometry(
            body_size,
            &[zoomed_geometry],
            &[rendered],
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
        return Ok(lines);
    }

    let geometries = rendered_pane_geometries(window, window_frame.enabled)?;
    let rendered_panes = geometries
        .iter()
        .map(|geometry| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)
                .unwrap_or_else(|| window.active_pane());
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = pane_render_region_size_for_geometry(geometry, &geometries)?;
            let merges = pane_frame.enabled
                && pane_frame_merges_into_divider(geometry, &geometries, pane_frame.position);
            Ok(render_styled_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                merges,
                ui_theme,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_styled_panes_by_geometry(
        body_size,
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

/// Runs the zoomed pane render input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn zoomed_pane_render_input(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    body_size: Size,
) -> Option<Vec<String>> {
    let zoomed_id = window.zoomed_pane_id()?;
    let pane = window
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() == zoomed_id.as_str())?;
    let lines = pane_inputs
        .iter()
        .find(|input| input.pane_id == pane.id.to_string())
        .map(|input| input.lines.as_slice())
        .unwrap_or(&[]);
    let mut display_pane = pane.clone();
    display_pane.size = body_size;
    Some(render_pane_lines(
        window,
        &display_pane,
        frame_context,
        lines,
        pane_frame,
        false,
    ))
}

/// Runs the zoomed styled pane render input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn zoomed_styled_pane_render_input(
    window: &Window,
    pane_inputs: &[StyledPaneRenderInput],
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    body_size: Size,
    ui_theme: &UiTheme,
) -> Option<Vec<TerminalStyledLine>> {
    let zoomed_id = window.zoomed_pane_id()?;
    let pane = window
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() == zoomed_id.as_str())?;
    let lines = pane_inputs
        .iter()
        .find(|input| input.pane_id == pane.id.to_string())
        .map(|input| input.lines.as_slice())
        .unwrap_or(&[]);
    let mut display_pane = pane.clone();
    display_pane.size = body_size;
    Some(render_styled_pane_lines(
        window,
        &display_pane,
        frame_context,
        lines,
        pane_frame,
        false,
        ui_theme,
    ))
}

/// Runs the zoomed pane geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn zoomed_pane_geometry(index: usize, size: Size) -> PaneGeometry {
    PaneGeometry {
        index,
        column: 0,
        row: 0,
        columns: size.columns,
        rows: size.rows,
    }
}

/// Returns the drawable window body after reserving mux-managed window frames.
pub fn rendered_window_body_size(size: Size, window_frames_enabled: bool) -> Result<Size> {
    Ok(mez_mux::presentation::rendered_window_body_size(
        size,
        window_frames_enabled,
    ))
}

/// Runs the window body size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_body_size(size: Size, window_frames_enabled: bool) -> Result<Size> {
    rendered_window_body_size(size, window_frames_enabled)
}

/// Returns pane rectangles apportioned within the rendered window body.
pub fn rendered_pane_geometries(
    window: &Window,
    window_frames_enabled: bool,
) -> Result<Vec<PaneGeometry>> {
    let body_size = rendered_window_body_size(window.size, window_frames_enabled)?;
    Ok(window.pane_geometries_for_size(body_size))
}

/// Returns the visible pane region after reserving shared divider cells.
pub fn pane_render_region_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> Result<Size> {
    Ok(mez_mux::presentation::pane_render_region_size_for_geometry(
        geometry, geometries,
    ))
}

/// Returns the pane body size available to the pane's PTY primary process.
///
/// When `pane_frames_enabled` is true and the frame is adjacent to a shared
/// divider, the frame is rendered in the divider row instead of consuming a
/// separate pane row, so the content size does not subtract that frame row.
pub fn pane_content_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    pane_frames_enabled: bool,
    pane_frame_position: TerminalFramePosition,
) -> Result<Size> {
    Ok(mez_mux::presentation::pane_content_size_for_geometry(
        geometry,
        geometries,
        pane_frames_enabled,
        pane_frame_position,
    ))
}

/// Runs the render panes by geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
