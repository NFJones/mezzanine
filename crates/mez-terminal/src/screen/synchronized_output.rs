//! Transient synchronized-output presentation state.
//!
//! This module separates the authoritative terminal model from the projection
//! published to attached clients while an application brackets a redraw with
//! synchronized-output markers. The state is deliberately screen-local and is
//! not included in durable mode or parser snapshots.

use super::*;

/// Summary of synchronized-output transitions observed while feeding bytes.
///
/// Callers that split one process event across multiple feed methods can merge
/// these outcomes before deciding whether to defer, publish, or rearm recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SynchronizedOutputFeedOutcome {
    /// Most recent begin epoch observed by this feed.
    pub begin_epoch: Option<u64>,
    /// Whether a begin marker requires bounded-recovery rearming.
    pub rearm_timeout: bool,
    /// Whether this feed released a previously frozen projection.
    pub released: bool,
    /// Whether release requires invalidating retained client rendering.
    pub full_redraw: bool,
}

impl SynchronizedOutputFeedOutcome {
    /// Merges a later feed result into this result in byte-stream order.
    pub fn merge(&mut self, later: Self) {
        if later.begin_epoch.is_some() {
            self.begin_epoch = later.begin_epoch;
        }
        self.rearm_timeout |= later.rearm_timeout;
        self.released |= later.released;
        self.full_redraw |= later.full_redraw;
    }
}

/// Frozen visual projection retained during one synchronized transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenPresentation {
    styled_lines: Vec<TerminalStyledLine>,
    cursor: TerminalCursorState,
    cursor_visible: bool,
    alternate_screen_active: bool,
    render_generation: u64,
}

/// Transient synchronization state owned by one terminal screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SynchronizedOutputState {
    active: bool,
    begin_epoch: u64,
    frozen_presentation: Option<FrozenPresentation>,
    full_redraw_required: bool,
}

impl TerminalScreen {
    /// Starts or rearms synchronized output while preserving the first frame.
    pub(super) fn begin_synchronized_output(&mut self) {
        self.synchronized_output.begin_epoch = self.synchronized_output.begin_epoch.wrapping_add(1);
        if !self.synchronized_output.active {
            self.synchronized_output.active = true;
            self.synchronized_output.frozen_presentation = Some(FrozenPresentation {
                styled_lines: self.visible_styled_lines(),
                cursor: self.cursor_state(),
                cursor_visible: self.cursor_visible,
                alternate_screen_active: self.alternate.active(),
                render_generation: next_render_generation().0,
            });
        }
        self.synchronized_output_outcome.begin_epoch = Some(self.synchronized_output.begin_epoch);
        self.synchronized_output_outcome.rearm_timeout = true;
    }

    /// Refreshes the frozen projection after a hidden protocol feed restores its display.
    pub(super) fn refresh_synchronized_output_presentation(&mut self) {
        if self.synchronized_output.active {
            self.synchronized_output.frozen_presentation = Some(FrozenPresentation {
                styled_lines: self.visible_styled_lines(),
                cursor: self.cursor_state(),
                cursor_visible: self.cursor_visible,
                alternate_screen_active: self.alternate.active(),
                render_generation: next_render_generation().0,
            });
        }
    }

    /// Releases synchronized output and exposes the current authoritative view.
    pub(super) fn end_synchronized_output(&mut self) {
        if !self.synchronized_output.active {
            return;
        }
        self.synchronized_output.active = false;
        self.synchronized_output.frozen_presentation = None;
        self.synchronized_output_outcome.released = true;
        self.synchronized_output_outcome.full_redraw =
            self.synchronized_output.full_redraw_required;
        self.synchronized_output.full_redraw_required = false;
    }

    /// Marks a mutation that cannot safely reuse an attached-client baseline.
    pub(super) fn mark_synchronized_output_full_redraw(&mut self) {
        if self.synchronized_output.active {
            self.synchronized_output.full_redraw_required = true;
        }
    }

    /// Returns whether synchronized output is currently freezing presentation.
    pub fn synchronized_output_active(&self) -> bool {
        self.synchronized_output.active
    }

    /// Returns the current synchronization begin epoch, when a transaction is open.
    pub fn synchronized_output_begin_epoch(&self) -> Option<u64> {
        self.synchronized_output
            .active
            .then_some(self.synchronized_output.begin_epoch)
    }

    /// Idempotently releases a transaction for timeout or lifecycle recovery.
    pub fn force_release_synchronized_output(&mut self) -> bool {
        if !self.synchronized_output.active {
            return false;
        }
        self.synchronized_output.full_redraw_required = true;
        self.end_synchronized_output();
        true
    }

    /// Returns rows from the frozen projection while synchronization is active.
    pub fn presentation_visible_styled_lines(&self) -> Vec<TerminalStyledLine> {
        self.synchronized_output
            .frozen_presentation
            .as_ref()
            .map(|presentation| presentation.styled_lines.clone())
            .unwrap_or_else(|| self.visible_styled_lines())
    }

    /// Returns the cursor from the visual projection used for client rendering.
    pub fn presentation_cursor_state(&self) -> TerminalCursorState {
        self.synchronized_output
            .frozen_presentation
            .as_ref()
            .map(|presentation| presentation.cursor)
            .unwrap_or_else(|| self.cursor_state())
    }

    /// Returns cursor visibility from the visual projection used for client rendering.
    pub fn presentation_cursor_visible(&self) -> bool {
        self.synchronized_output
            .frozen_presentation
            .as_ref()
            .map(|presentation| presentation.cursor_visible)
            .unwrap_or(self.cursor_visible)
    }

    /// Returns alternate-buffer state from the visual projection used for rendering.
    pub fn presentation_alternate_screen_active(&self) -> bool {
        self.synchronized_output
            .frozen_presentation
            .as_ref()
            .map(|presentation| presentation.alternate_screen_active)
            .unwrap_or_else(|| self.alternate.active())
    }

    /// Returns the generation identifying the current visual projection.
    pub fn presentation_render_generation(&self) -> u64 {
        self.synchronized_output
            .frozen_presentation
            .as_ref()
            .map(|presentation| presentation.render_generation)
            .unwrap_or(self.render_generation.0)
    }
}
