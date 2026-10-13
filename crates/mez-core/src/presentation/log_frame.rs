//! Pure cell geometry for optional structured-log decoration.
//!
//! The cap bounds the entire framed row, not merely authored prose. This owner
//! knows neither terminal styles nor clocks. A timestamp margin is all-or-none:
//! eight ASCII cells and one separator require the two-cell gutter and at least
//! ten body cells for the longest fixed thinking label. Canonical terminal rows
//! omit the optional margin; visual rows
//! and copy snapshots carry this geometry alongside their existing row identity.

use std::ops::Range;

/// Immutable framing policy captured with a screen or projection worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogFramePolicy {
    /// Whether proven structured entries show a timestamp margin.
    pub timestamps: bool,
    /// Maximum total framed-row width, including margin and gutter.
    pub wrap_cap: usize,
}

/// Exact cell ranges for one structured visual row at bounded pane geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFrame {
    /// Eight timestamp cells, absent when disabled or too narrow.
    pub timestamp: Option<Range<usize>>,
    /// One separator cell immediately after the timestamp.
    pub separator: Option<Range<usize>>,
    /// Existing two-cell transcript gutter, clipped only at unusable widths.
    pub gutter: Range<usize>,
    /// First body cell in the visual row.
    pub body_origin: usize,
    /// Cells available for authored body presentation; zero means no body fits.
    pub body_budget: usize,
    /// Total pane/cap-bounded cells, never a saturated fictitious body cell.
    pub total_width: usize,
}

impl LogFramePolicy {
    /// Computes ranges without side effects. Zero cap means the pane width;
    /// callers normally provide the validated product cap.
    pub fn frame(self, pane_width: usize) -> LogFrame {
        let total_width = if self.wrap_cap == 0 {
            pane_width
        } else {
            pane_width.min(self.wrap_cap)
        };
        let margin = usize::from(self.timestamps && total_width >= 21) * 9;
        let body_origin = (margin + 2).min(total_width);
        LogFrame {
            timestamp: (margin != 0).then_some(0..8),
            separator: (margin != 0).then_some(8..9),
            gutter: margin..body_origin,
            body_origin,
            body_budget: total_width - body_origin,
            total_width,
        }
    }
}

impl LogFrame {
    /// Number of visual cells absent from canonical terminal/source rows.
    pub fn margin_width(&self) -> usize {
        self.separator.as_ref().map_or(0, |range| range.end)
    }

    /// Canonical gutter-plus-body capacity used by tagged soft wrapping.
    pub fn canonical_width(&self) -> usize {
        self.total_width - self.margin_width()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Suppression is based on the entire capped row. The boundary is exact:
    /// twenty cells suppress all nine optional cells, twenty-one admit ten body
    /// cells, and zero/unusable widths never manufacture capacity or overflow.
    #[test]
    fn log_frame_ranges_respect_cap_and_narrow_threshold() {
        for total in 0..=23 {
            let frame = LogFramePolicy {
                timestamps: true,
                wrap_cap: total,
            }
            .frame(total);
            assert_eq!(frame.total_width, total);
            assert_eq!(frame.margin_width(), if total >= 21 { 9 } else { 0 });
            assert_eq!(frame.body_origin + frame.body_budget, total);
            assert!(frame.gutter.end <= total);
        }
        let policy = LogFramePolicy {
            timestamps: true,
            wrap_cap: 21,
        };
        assert_eq!(policy.frame(80).body_budget, 10);
        assert_eq!(policy.frame(80).timestamp, Some(0..8));
        assert_eq!(policy.frame(20).margin_width(), 0);
        assert_eq!(
            LogFramePolicy {
                timestamps: false,
                wrap_cap: 21
            }
            .frame(80)
            .body_budget,
            19
        );
    }
}
