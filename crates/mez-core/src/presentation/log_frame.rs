//! Pure cell geometry for optional structured-log decoration.
//!
//! The cap bounds the entire framed row, not merely authored prose. This owner
//! knows neither terminal styles nor clocks. A timestamp margin is all-or-none:
//! validated ASCII cells and one separator require the two-cell gutter and at least
//! ten body cells for the longest fixed thinking label. Canonical terminal rows
//! omit the optional margin; visual rows
//! and copy snapshots carry this geometry alongside their existing row identity.

use super::LogTimestampFormat;
use std::ops::Range;

/// Immutable framing policy captured with a screen or projection worker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogFramePolicy {
    /// Whether proven structured entries show a timestamp margin.
    pub timestamps: bool,
    /// Maximum total framed-row width, including margin and gutter.
    pub wrap_cap: usize,
    /// Small immutable format capture; geometry consumes only its proven width.
    pub timestamp_format: LogTimestampFormat,
}

/// Exact cell ranges for one structured visual row at bounded pane geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFrame {
    /// Validated timestamp cells, absent when disabled or too narrow.
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
    pub fn frame(&self, pane_width: usize) -> LogFrame {
        let total_width = if self.wrap_cap == 0 {
            pane_width
        } else {
            pane_width.min(self.wrap_cap)
        };
        let width = self.timestamp_format.width();
        let margin = usize::from(self.timestamps && total_width >= width + 13) * (width + 1);
        let body_origin = (margin + 2).min(total_width);
        LogFrame {
            timestamp: (margin != 0).then_some(0..width),
            separator: (margin != 0).then_some(width..width + 1),
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
                ..LogFramePolicy::default()
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
            ..LogFramePolicy::default()
        };
        assert_eq!(policy.frame(80).body_budget, 10);
        assert_eq!(policy.frame(80).timestamp, Some(0..8));
        assert_eq!(policy.frame(20).margin_width(), 0);
        assert_eq!(
            LogFramePolicy {
                timestamps: false,
                wrap_cap: 21,
                ..LogFramePolicy::default()
            }
            .frame(80)
            .body_budget,
            19
        );
    }

    /// Custom widths move the all-or-none threshold by exactly their fixed
    /// delta. Pane and cap bounds remain interchangeable, with no fake body
    /// capacity or partial timestamp margin at either side of the boundary.
    #[test]
    fn log_timestamp_format_geometry_uses_validated_width_and_whole_cap() {
        for width in [1, 5, 8, 31, 32] {
            let policy = LogFramePolicy {
                timestamps: true,
                wrap_cap: 0,
                timestamp_format: LogTimestampFormat::compile(&"a".repeat(width)).unwrap(),
            };
            for total in 0..=width + 14 {
                let frame = policy.frame(total);
                assert_eq!(
                    frame.margin_width(),
                    if total >= width + 13 { width + 1 } else { 0 }
                );
                assert_eq!(frame.body_origin + frame.body_budget, total);
                assert!(frame.gutter.end <= total);
                let capped = LogFramePolicy {
                    wrap_cap: total,
                    ..policy.clone()
                };
                if total > 0 {
                    assert_eq!(capped.frame(100), frame);
                }
            }
            assert_eq!(policy.frame(width + 13).body_budget, 10);
        }
    }
}
