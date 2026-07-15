//! Product pane-divider presentation adapters.
//!
//! `mez-mux` owns divider geometry and glyph connectivity. This module maps
//! lower divider cells to product mouse-hit records and applies configured
//! Mezzanine theme renditions to merged frame boundaries.

use mez_mux::input::MouseBorderCell;
use mez_mux::layout::PaneGeometry;
use mez_mux::theme::UiTheme;
use mez_terminal::{GraphicRendition, TerminalStyleSpan};

use mez_mux::presentation::pane_divider_cells;

/// Returns the rendered cells occupied by mux-managed pane dividers.
pub fn pane_border_cells_for_geometries(
    geometries: &[PaneGeometry],
    row_offset: u16,
) -> Vec<MouseBorderCell> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .map(|cell| MouseBorderCell {
            column: cell.column,
            row: cell.row.saturating_add(row_offset),
        })
        .collect()
}

/// Builds style spans for divider junctions that bound a merged pane status row.
pub(super) fn merged_pane_frame_boundary_style_spans(
    geometries: &[PaneGeometry],
    row: u16,
    column_start: usize,
    width: usize,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    mez_mux::render::merged_pane_frame_boundary_style_spans(
        geometries,
        row,
        column_start,
        width,
        pane_divider_rendition(ui_theme),
    )
}

/// Returns the stable divider rendition used for merged pane-frame boundary
/// caps.
pub(super) fn pane_divider_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(ui_theme.colors.pane_divider.foreground),
        background: None,
        ..GraphicRendition::default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::pane_border_rendition;
    use super::*;
    use mez_core::ids::IdFactory;
    use mez_mux::layout::SplitDirection;
    use mez_mux::layout::{Size, Window};
    use mez_mux::render::blank_render_cells;

    /// Verifies pane divider styling still uses the active border palette when
    /// a divider cell touches the active pane border.
    #[test]
    fn styled_pane_dividers_highlight_active_pane_border() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(8, 4).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        let geometries = window.pane_geometries();
        let rows = usize::from(window.size.rows);
        let columns = usize::from(window.size.columns);
        let mut text_canvas = blank_render_cells(rows, columns, ' ');
        let mut style_canvas = vec![Vec::new(); rows];
        let ui_theme = UiTheme::default();

        mez_mux::render::draw_styled_pane_dividers(
            &mut text_canvas,
            &mut style_canvas,
            &geometries,
            true,
            window.active_pane_index(),
            pane_border_rendition(true, &ui_theme),
            pane_divider_rendition(&ui_theme),
        );

        let active = pane_border_rendition(true, &ui_theme);
        assert!(
            style_canvas
                .iter()
                .flatten()
                .any(|span| span.rendition == active)
        );
    }

    /// Verifies merged pane-frame boundary caps keep the stable divider
    /// palette even when pane focus moves between panes.
    #[test]
    fn merged_pane_frame_boundaries_use_focus_stable_rendition() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        window
            .split_active(&mut ids, SplitDirection::Horizontal)
            .unwrap();
        let ui_theme = UiTheme::default();
        let stable = pane_divider_rendition(&ui_theme);

        let geometries = window.pane_geometries();
        let target = geometries
            .iter()
            .max_by_key(|geometry| (geometry.row, geometry.column))
            .copied()
            .expect("split window should produce pane geometries");
        let row = target.row.saturating_sub(1);
        let column_start = usize::from(target.column);
        let width = usize::from(target.columns);

        let focused_boundary_spans = merged_pane_frame_boundary_style_spans(
            &geometries,
            row,
            column_start,
            width,
            &ui_theme,
        );

        window.select_pane("0").unwrap();
        let unfocused_boundary_spans = merged_pane_frame_boundary_style_spans(
            &geometries,
            row,
            column_start,
            width,
            &ui_theme,
        );

        assert!(!focused_boundary_spans.is_empty());
        assert!(
            focused_boundary_spans
                .iter()
                .all(|span| span.length == 1 && span.rendition == stable)
        );
        assert_eq!(focused_boundary_spans, unfocused_boundary_spans);
    }

    /// Verifies neutral divider cells honor the dedicated divider palette
    /// instead of falling back to the inactive pane-border colors.
    #[test]
    fn styled_pane_dividers_use_dedicated_divider_palette_for_neutral_cells() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        window
            .split_active(&mut ids, SplitDirection::Horizontal)
            .unwrap();
        let geometries = window.pane_geometries();
        let rows = usize::from(window.size.rows);
        let columns = usize::from(window.size.columns);
        let mut text_canvas = blank_render_cells(rows, columns, ' ');
        let mut style_canvas = vec![Vec::new(); rows];
        let ui_theme = UiTheme::default();
        let divider = pane_divider_rendition(&ui_theme);

        mez_mux::render::draw_styled_pane_dividers(
            &mut text_canvas,
            &mut style_canvas,
            &geometries,
            true,
            window.active_pane_index(),
            pane_border_rendition(true, &ui_theme),
            pane_divider_rendition(&ui_theme),
        );

        assert!(
            style_canvas
                .iter()
                .flatten()
                .any(|span| span.rendition == divider)
        );
    }
}
