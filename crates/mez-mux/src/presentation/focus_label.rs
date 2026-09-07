//! Fixed title-only overlay anchors in existing window presentation geometry.
//!
//! This policy never reserves rows or changes content/hit regions. Coordinates
//! remain authoritative; the ordinary client viewport performs clipping later.
//! Product callers own identity text, lifetime, style and overlay precedence.

use super::{
    PresentationRegion, TerminalFramePosition, WindowPresentationPlan, pane_divider_cells,
    pane_frame_merges_into_divider,
};

/// Identity scope whose single fixed overlay location is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusLabelScope {
    /// Current group at the canvas top-left.
    Group,
    /// Current window at the canvas bottom-left.
    Window,
    /// Current active pane at its top-left or eligible shared top divider.
    Pane,
}

/// Returns a one-row title budget without modifying the supplied zen plan.
///
/// Empty bounds yield no location. Divider titles stop before junctions and
/// retain a trailing structural cell; anchors are never moved to fit a viewport.
pub fn focus_label_region(
    plan: &WindowPresentationPlan,
    scope: FocusLabelScope,
) -> Option<PresentationRegion> {
    let size = plan.authoritative_size;
    if size.rows == 0 || size.columns == 0 {
        return None;
    }
    let mut region = PresentationRegion {
        row: 0,
        column: 0,
        columns: size.columns,
        rows: 1,
    };
    match scope {
        FocusLabelScope::Group => {}
        FocusLabelScope::Window => region.row = size.rows - 1,
        FocusLabelScope::Pane => {
            let pane = plan.panes.iter().find(|pane| pane.active)?;
            if pane.content_region.is_empty() {
                return None;
            }
            region = PresentationRegion {
                rows: 1,
                ..pane.content_region
            };
            let geometries = plan.pane_geometries();
            if pane_frame_merges_into_divider(
                &pane.geometry,
                &geometries,
                TerminalFramePosition::Top,
            ) {
                region.row = plan
                    .body_row_offset
                    .saturating_add(pane.geometry.row.saturating_sub(1));
                let cells = pane_divider_cells(&geometries, true);
                let body_row = region.row.saturating_sub(plan.body_row_offset);
                let uninterrupted = (0..region.columns)
                    .take_while(|offset| {
                        cells.iter().any(|cell| {
                            cell.row == body_row
                                && cell.column == region.column.saturating_add(*offset)
                                && cell.glyph == '─'
                        })
                    })
                    .count();
                region.columns = u16::try_from(uninterrupted)
                    .unwrap_or(u16::MAX)
                    .saturating_sub(1);
                if region.columns < 3 {
                    region = PresentationRegion {
                        rows: 1,
                        ..pane.content_region
                    };
                }
            }
        }
    }
    region.columns = region
        .columns
        .min(size.columns.saturating_sub(region.column));
    (!region.is_empty() && region.row < size.rows).then_some(region)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneGeometry;
    use crate::presentation::PanePresentationPlan;
    use mez_terminal::TerminalSize;

    /// Builds explicit geometry fixtures without any product configuration.
    fn plan(geometries: &[PaneGeometry], active: usize) -> WindowPresentationPlan {
        let size = TerminalSize {
            columns: 20,
            rows: 10,
        };
        WindowPresentationPlan {
            authoritative_size: size,
            display_size: size,
            body_size: size,
            group_frame_row: None,
            window_frame_row: None,
            body_row_offset: 0,
            panes: geometries
                .iter()
                .map(|g| {
                    let region = PresentationRegion {
                        row: g.row,
                        column: g.column,
                        columns: g.columns,
                        rows: g.rows,
                    };
                    PanePresentationPlan {
                        source_index: g.index,
                        active: g.index == active,
                        geometry: *g,
                        render_region_size: size,
                        render_region: region,
                        content_size: size,
                        content_region: region,
                        frame_row: None,
                        frame_merges_into_divider: false,
                    }
                })
                .collect(),
        }
    }

    /// Group/window anchors never depend on selected indices, and a standalone
    /// pane overlays content without mutating the authoritative geometry.
    #[test]
    fn zen_focus_fixed_canvas_anchors_preserve_plan() {
        let p = plan(
            &[PaneGeometry {
                index: 7,
                row: 0,
                column: 0,
                columns: 20,
                rows: 10,
            }],
            7,
        );
        let before = p.clone();
        assert_eq!(
            focus_label_region(&p, FocusLabelScope::Group).unwrap().row,
            0
        );
        assert_eq!(
            focus_label_region(&p, FocusLabelScope::Window).unwrap().row,
            9
        );
        assert_eq!(
            focus_label_region(&p, FocusLabelScope::Pane).unwrap(),
            PresentationRegion {
                row: 0,
                column: 0,
                columns: 20,
                rows: 1
            }
        );
        assert_eq!(p, before);
    }

    /// Stacked titles reuse the top divider while preserving its trailing cell;
    /// mixed upper splits shorten the title before the intervening junction.
    #[test]
    fn zen_focus_divider_anchor_stops_before_junction() {
        let top = PaneGeometry {
            index: 0,
            row: 0,
            column: 0,
            columns: 20,
            rows: 5,
        };
        let bottom = PaneGeometry {
            index: 1,
            row: 5,
            column: 0,
            columns: 20,
            rows: 5,
        };
        let p = plan(&[top, bottom], 1);
        let anchor = focus_label_region(&p, FocusLabelScope::Pane).unwrap();
        assert_eq!(anchor.row, 4);
        assert!(anchor.columns < 20);
        let p = plan(
            &[
                PaneGeometry { columns: 10, ..top },
                PaneGeometry {
                    index: 2,
                    column: 10,
                    columns: 10,
                    ..top
                },
                bottom,
            ],
            1,
        );
        assert!(
            focus_label_region(&p, FocusLabelScope::Pane)
                .unwrap()
                .columns
                < 10
        );
    }

    /// Zero bounds omit titles and one-row canvases keep coincident anchors for
    /// product precedence rather than relocating a lower-priority title.
    #[test]
    fn zen_focus_empty_and_one_row_bounds() {
        let mut p = plan(&[], 0);
        p.authoritative_size.rows = 1;
        assert_eq!(
            focus_label_region(&p, FocusLabelScope::Group),
            focus_label_region(&p, FocusLabelScope::Window)
        );
        p.authoritative_size.columns = 0;
        assert!(focus_label_region(&p, FocusLabelScope::Group).is_none());
    }
}
