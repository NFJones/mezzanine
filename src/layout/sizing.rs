//! Pane sizing calculations for split and layout operations.
//!
//! This module owns dimension arithmetic and minimum-size enforcement. Window
//! mutation code calls into it instead of duplicating layout constraints.

use super::{
    LayoutPolicy, MIN_PANE_COLUMNS, MIN_PANE_ROWS, MezError, PaneSizeSpec, ResizeAxis,
    ResizeDirection, Result, Size, SplitDirection,
};

/// Preferred width for one pane in an automatically balanced grid layout.
///
/// The value is intentionally larger than the hard pane minimum. It keeps
/// terminal-oriented panes wide enough for readable command output while the
/// layout engine decides whether to spend extra space on rows or columns.
pub(crate) const EVEN_GRID_TARGET_COLUMNS: u16 = 40;

/// Preferred height for one pane in an automatically balanced grid layout.
///
/// This gives stacked panes enough scroll context to remain useful without
/// preventing compact terminals from falling back to the hard pane minimum.
pub(crate) const EVEN_GRID_TARGET_ROWS: u16 = 8;

/// Runs the split size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn split_size(size: Size, direction: SplitDirection) -> Result<(Size, Size)> {
    match direction {
        SplitDirection::Vertical => {
            if size.columns < MIN_PANE_COLUMNS.saturating_mul(2) {
                return Err(MezError::invalid_state(
                    "cannot split vertically with fewer than 2 columns",
                ));
            }
            let first = size.columns / 2 + size.columns % 2;
            let second = size.columns / 2;
            Ok((
                Size {
                    columns: first,
                    rows: size.rows,
                },
                Size {
                    columns: second,
                    rows: size.rows,
                },
            ))
        }
        SplitDirection::Horizontal => {
            if size.rows < MIN_PANE_ROWS.saturating_mul(2) {
                return Err(MezError::invalid_state(
                    "cannot split horizontally with fewer than 2 rows",
                ));
            }
            let first = size.rows / 2 + size.rows % 2;
            let second = size.rows / 2;
            Ok((
                Size {
                    columns: size.columns,
                    rows: first,
                },
                Size {
                    columns: size.columns,
                    rows: second,
                },
            ))
        }
    }
}

/// Runs the split size with spec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn split_size_with_spec(
    size: Size,
    direction: SplitDirection,
    spec: PaneSizeSpec,
) -> Result<(Size, Size)> {
    let (_, default_created) = split_size(size, direction)?;
    let requested = split_requested_size(size, default_created, spec)?;
    match direction {
        SplitDirection::Vertical => {
            if requested.rows != size.rows {
                return Err(MezError::invalid_args(
                    "vertical split size must preserve the pane row count",
                ));
            }
            let existing_columns =
                size.columns.checked_sub(requested.columns).ok_or_else(|| {
                    MezError::invalid_args("vertical split size would overlap the existing pane")
                })?;
            if requested.columns < MIN_PANE_COLUMNS || existing_columns < MIN_PANE_COLUMNS {
                return Err(MezError::invalid_args(
                    "vertical split size is below the minimum pane dimensions",
                ));
            }
            Ok((
                Size {
                    columns: existing_columns,
                    rows: size.rows,
                },
                requested,
            ))
        }
        SplitDirection::Horizontal => {
            if requested.columns != size.columns {
                return Err(MezError::invalid_args(
                    "horizontal split size must preserve the pane column count",
                ));
            }
            let existing_rows = size.rows.checked_sub(requested.rows).ok_or_else(|| {
                MezError::invalid_args("horizontal split size would overlap the existing pane")
            })?;
            if requested.rows < MIN_PANE_ROWS || existing_rows < MIN_PANE_ROWS {
                return Err(MezError::invalid_args(
                    "horizontal split size is below the minimum pane dimensions",
                ));
            }
            Ok((
                Size {
                    columns: size.columns,
                    rows: existing_rows,
                },
                requested,
            ))
        }
    }
}

/// Runs the split requested size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn split_requested_size(original: Size, default_created: Size, spec: PaneSizeSpec) -> Result<Size> {
    match spec {
        PaneSizeSpec::Cells { columns, rows } => Size::new(
            columns.unwrap_or(default_created.columns),
            rows.unwrap_or(default_created.rows),
        ),
        PaneSizeSpec::Percent { percent, axis } => {
            if percent == 0 {
                return Err(MezError::invalid_args(
                    "percent split size requires a positive percent",
                ));
            }
            let columns = if matches!(axis, ResizeAxis::Columns | ResizeAxis::Both) {
                scaled_dimension(original.columns, percent, "columns")?
            } else {
                default_created.columns
            };
            let rows = if matches!(axis, ResizeAxis::Rows | ResizeAxis::Both) {
                scaled_dimension(original.rows, percent, "rows")?
            } else {
                default_created.rows
            };
            Size::new(columns, rows)
        }
        PaneSizeSpec::Delta { direction, amount }
        | PaneSizeSpec::Edge {
            edge: direction,
            amount,
        } => split_size_from_direction(default_created, direction, amount),
    }
}

/// Runs the split size from direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn split_size_from_direction(
    current: Size,
    direction: ResizeDirection,
    amount: u16,
) -> Result<Size> {
    if amount == 0 {
        return Err(MezError::invalid_args(
            "directional split size amount must be positive",
        ));
    }
    match direction {
        ResizeDirection::Left => Size::new(
            current.columns.checked_sub(amount).ok_or_else(|| {
                MezError::invalid_args("split size would reduce columns below zero")
            })?,
            current.rows,
        ),
        ResizeDirection::Right => Size::new(
            current
                .columns
                .checked_add(amount)
                .ok_or_else(|| MezError::invalid_args("split size columns are out of range"))?,
            current.rows,
        ),
        ResizeDirection::Up => Size::new(
            current.columns,
            current
                .rows
                .checked_sub(amount)
                .ok_or_else(|| MezError::invalid_args("split size would reduce rows below zero"))?,
        ),
        ResizeDirection::Down => Size::new(
            current.columns,
            current
                .rows
                .checked_add(amount)
                .ok_or_else(|| MezError::invalid_args("split size rows are out of range"))?,
        ),
    }
}

/// Runs the scaled dimension operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn scaled_dimension(total: u16, percent: u16, axis: &'static str) -> Result<u16> {
    let scaled = u32::from(total)
        .saturating_mul(u32::from(percent))
        .saturating_add(99)
        / 100;
    u16::try_from(scaled.max(1))
        .map_err(|_| MezError::invalid_args(format!("percent split size {axis} is out of range")))
}

/// Runs the split dimension evenly operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_dimension_evenly(total: u16, count: usize) -> Vec<u16> {
    let count = count.max(1);
    let base = usize::from(total) / count;
    let remainder = usize::from(total) % count;
    (0..count)
        .map(|index| (base + usize::from(index < remainder)).max(1) as u16)
        .collect()
}

/// Returns the row and column count used by the even-grid layout policy.
///
/// Candidates are scored against terminal-oriented preferred pane dimensions
/// and then by empty-cell count and pane area. The result keeps small pane sets
/// simple, but moves to true grids once a single vertical or horizontal axis
/// would make panes noticeably less useful.
pub(crate) fn even_grid_dimensions(size: Size, pane_count: usize) -> (usize, usize) {
    let pane_count = pane_count.max(1);
    let mut best = None;
    for columns in 1..=pane_count {
        let rows = pane_count.div_ceil(columns);
        let min_columns = minimum_even_dimension(size.columns, columns);
        let min_rows = minimum_even_dimension(size.rows, rows);
        let empty_cells = columns.saturating_mul(rows).saturating_sub(pane_count);
        let preferred_axes = u8::from(min_columns >= EVEN_GRID_TARGET_COLUMNS)
            + u8::from(min_rows >= EVEN_GRID_TARGET_ROWS);
        let min_ratio = std::cmp::min(
            ratio_millis(min_columns, EVEN_GRID_TARGET_COLUMNS),
            ratio_millis(min_rows, EVEN_GRID_TARGET_ROWS),
        );
        let area = u32::from(min_columns).saturating_mul(u32::from(min_rows));
        let score = (
            preferred_axes,
            min_ratio,
            area,
            std::cmp::Reverse(empty_cells),
        );
        if best
            .as_ref()
            .is_none_or(|(best_score, _, _)| score > *best_score)
        {
            best = Some((score, columns, rows));
        }
    }
    best.map(|(_, columns, rows)| (columns, rows))
        .unwrap_or((pane_count, 1))
}

/// Estimates the smallest pane produced by a self-rebalancing layout policy.
pub(crate) fn even_layout_minimum_pane_size(
    policy: LayoutPolicy,
    size: Size,
    pane_count: usize,
) -> Size {
    let pane_count = pane_count.max(1);
    match policy {
        LayoutPolicy::Tiled | LayoutPolicy::EvenVertical => Size {
            columns: minimum_even_dimension(size.columns, pane_count),
            rows: size.rows,
        },
        LayoutPolicy::EvenHorizontal => Size {
            columns: size.columns,
            rows: minimum_even_dimension(size.rows, pane_count),
        },
        LayoutPolicy::EvenGrid => {
            let (columns, rows) = even_grid_dimensions(size, pane_count);
            Size {
                columns: minimum_even_dimension(size.columns, columns),
                rows: minimum_even_dimension(size.rows, rows),
            }
        }
    }
}

/// Returns the smallest cell allocation made by an even split on one axis.
fn minimum_even_dimension(total: u16, count: usize) -> u16 {
    let count = count.max(1);
    (usize::from(total) / count).max(1) as u16
}

/// Returns a fixed-point ratio suitable for integer candidate scoring.
fn ratio_millis(value: u16, target: u16) -> u32 {
    u32::from(value).saturating_mul(1000) / u32::from(target.max(1))
}
