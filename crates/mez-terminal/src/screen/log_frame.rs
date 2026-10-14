//! Optional typed decoration geometry for producer-proven terminal rows.
//!
//! The product formats and paints timestamps above this crate. This module only
//! reserves canonical capacity during scoped tagged feed/reflow and exposes
//! immutable policy to explicit visual/copy projection. Raw PTY writes, parser
//! coordinates, screen dimensions, saved states and canonical text are unchanged.
//! Disabled policy uses the original pane-width soft wrapping, even above a cap.

use super::*;

/// Canonical row capacity shared by tagged print, reflow and cursor mapping.
/// Truly untagged or disabled rows retain the full physical terminal width.
pub(super) fn log_frame_columns(
    entry: Option<&PresentationEntry>,
    columns: usize,
    policy: &LogFramePolicy,
) -> usize {
    if policy.timestamps && entry.is_some_and(PresentationEntry::is_valid) {
        policy.frame(columns).canonical_width().max(1)
    } else {
        columns.max(1)
    }
}

impl TerminalScreen {
    /// Returns immutable geometry and supplied framing style for visual copies.
    pub fn log_frame_settings(&self) -> (LogFramePolicy, GraphicRendition) {
        (self.log_frame_policy.clone(), self.log_timestamp_rendition)
    }

    /// Replaces optional projection policy without changing physical PTY size.
    /// Synchronization is released before settings change. Only the live rows
    /// reflow synchronously; retained history is never rebuilt on resize/paint.
    pub fn set_log_frame_settings(&mut self, policy: LogFramePolicy, rendition: GraphicRendition) {
        if self.log_frame_policy == policy && self.log_timestamp_rendition == rendition {
            return;
        }
        if self.log_frame_policy.timestamps || policy.timestamps {
            self.force_release_synchronized_output();
        }
        self.preserve_presentation_copy_associations |= policy.timestamps;
        let geometry_changed = self.log_frame_policy.timestamps != policy.timestamps
            || (policy.timestamps
                && (self.log_frame_policy.wrap_cap != policy.wrap_cap
                    || self.log_frame_policy.timestamp_format.width()
                        != policy.timestamp_format.width()));
        self.log_frame_policy = policy;
        self.log_timestamp_rendition = rendition;
        if geometry_changed && !self.alternate.active() {
            self.resize_normal_screen_reflowing(self.size);
        }
        self.mark_render_changed();
    }

    /// Builds canonical copy rows at the captured geometry. Only retained
    /// history is reflowed here; live rows remain exactly the visual viewport.
    /// This explicit copy operation is not called from resize or paint. Owned
    /// wrap flags and first-row metadata survive, including a pruned boundary.
    pub fn normal_log_frame_copy_rows(
        &self,
        continuation_copy: impl Fn(&str) -> String,
    ) -> Vec<TerminalStyledLine> {
        if !self.log_frame_policy.timestamps || self.alternate.active() {
            return self.normal_styled_content_lines();
        }
        let mut history = self
            .history
            .styled_lines()
            .enumerate()
            .map(|(index, line)| PhysicalStyledLine {
                line,
                wraps_to_next: self.history.line_wraps_to_next(index),
            })
            .collect::<Vec<_>>();
        // A copy payload is a source association, not a string fragment to
        // concatenate with the next row. Preserve one opaque owner per logical
        // row; the product supplies continuation references without this crate
        // understanding any product-specific encoding or marker text.
        let mut unfinished = false;
        for row in &mut history {
            if unfinished {
                row.line.copy_text = None;
            }
            unfinished = row.wraps_to_next;
        }
        let logical = merge_wrapped_physical_lines(
            &history,
            self.wrap_continuation_prefix.as_deref(),
            self.emoji_width,
            true,
        );
        let mut rows = Vec::new();
        for line in logical {
            // The merge owns invalidation of both typed and legacy source.
            // Never reattach a payload saved independently before that check.
            let source = line.copy_text.clone();
            let mut physical = Vec::new();
            reflow_one_logical_line(
                &line,
                usize::from(self.size.columns),
                self.wrap_continuation_prefix.as_deref(),
                self.emoji_width,
                &self.log_frame_policy,
                true,
                &mut physical,
            );
            for (index, mut row) in physical.into_iter().enumerate() {
                row.line.copy_text = source.as_deref().map(|text| {
                    if index == 0 {
                        text.to_string()
                    } else {
                        continuation_copy(text)
                    }
                });
                rows.push(row.line);
            }
        }
        if history.is_empty() {
            rows.clear();
        }
        rows.extend(self.visible_styled_lines());
        rows
    }

    /// Effective right edge for printable scalars within a scoped tagged feed.
    /// CSI, raw output and other control-directed edits use physical max_column.
    pub(super) fn print_max_column(&self) -> usize {
        log_frame_columns(
            self.active_presentation.as_ref(),
            usize::from(self.size.columns),
            &self.log_frame_policy,
        ) - 1
    }
}
