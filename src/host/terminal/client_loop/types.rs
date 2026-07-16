//! Product terminal-loop actions, configuration, reports, and host I/O test contracts.

use std::time::Instant;

#[cfg(test)]
use super::{AttachedTerminalFdReadiness, Result};
use super::{AttachedTerminalFdRole, MouseAction};
use mez_mux::copy::CopyModeKeyAction;
use mez_mux::input::{KeyChord, MuxAction};
#[cfg(test)]
use mez_mux::layout::Size;
#[cfg(test)]
use mez_mux::presentation::AttachedTerminalOutputModes;
use mez_mux::presentation::ClientStatusLine;
use mez_terminal::TerminalStyleSpan;

// Attached terminal loop planning and I/O abstraction.

/// Refresh cadence for active agent status animations.
///
/// Renderers advance the scan phase at this interval, and attach clients use
/// the same value to request fresh views only while animation is active.
pub const AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS: u64 = 180;

/// Carries Terminal Client Loop Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalClientLoopAction {
    /// Represents the Forward To Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ForwardToPane(Vec<u8>),
    /// Represents the Forward Mouse To Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ForwardMouseToPane {
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: Vec<u8>,
    },
    /// Represents the Execute Mux case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExecuteMux(MuxAction),
    /// Represents the Execute Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExecuteCommand(String),
    /// Represents the Handle Mouse case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HandleMouse(MouseAction),
    /// Represents the Handle Copy Mode case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HandleCopyMode(CopyModeKeyAction),
    /// Represents the Enter Prefix Key Mode case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnterPrefixKeyMode,
    /// Represents the Report Unbound Prefix case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ReportUnboundPrefix(KeyChord),
}

/// Carries Readline Prompt Status Row state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePromptStatusRow {
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: ClientStatusLine,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_visible: bool,
}

/// Carries Readline Prompt Client Presentation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePromptClientPresentation {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub lines: Vec<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_visible: bool,
}

/// Product-specialized attached-client step result owned by the mux.
pub type AttachedTerminalClientStepPlan =
    mez_mux::presentation::AttachedClientStepPlan<TerminalClientLoopAction, AttachedTerminalFdRole>;

/// Carries Attached Terminal Client Loop Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedTerminalClientLoopConfig {
    /// Stores the max iterations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_iterations: usize,
    /// Stores the max input bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_input_bytes: usize,
}

impl Default for AttachedTerminalClientLoopConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_iterations: 128,
            max_input_bytes: 1024 * 1024,
        }
    }
}

/// Result of one bounded attached-terminal output write attempt.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub struct AttachedTerminalOutputWriteReport {
    /// Bytes written during this attempt.
    pub bytes_written: usize,
    /// Whether the output frame was fully written.
    pub completed: bool,
    /// Bytes still retained for later output flush attempts.
    pub pending_bytes: usize,
}

#[cfg(test)]
impl AttachedTerminalOutputWriteReport {
    /// Returns a completed write report.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub const fn completed(bytes_written: usize) -> Self {
        Self {
            bytes_written,
            completed: true,
            pending_bytes: 0,
        }
    }

    /// Returns whether this write left bytes pending.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub const fn is_partial(self) -> bool {
        !self.completed || self.pending_bytes > 0
    }
}

/// Carries Attached Terminal Client Loop Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedTerminalClientLoopReport {
    /// Stores the iterations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub iterations: u64,
    /// Stores the actions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub actions: Vec<TerminalClientLoopAction>,
    /// Stores the output frames value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_frames: u64,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_written: usize,
    /// Number of bounded output writes that left bytes pending.
    pub partial_writes: u64,
    /// Bytes retained by the attached terminal endpoint after this loop.
    pub pending_output_bytes: usize,
    /// Stores the input hangups value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input_hangups: u64,
    /// Stores the output hangups value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_hangups: u64,
    /// Stores the error roles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub error_roles: Vec<AttachedTerminalFdRole>,
    /// Whether the attached client finished this loop while still inside a
    /// host bracketed paste payload.
    pub host_bracketed_paste_active: bool,
    /// Buffered host bracketed-paste bytes retained for the next loop.
    pub host_bracketed_paste_buffer: Vec<u8>,
    /// Monotonic time at which the current buffered host paste began.
    pub host_bracketed_paste_started_at: Option<Instant>,
}

/// Mutable host bracketed-paste state carried while routing terminal input.
pub(crate) struct HostBracketedPasteBufferState<'a> {
    /// Whether the router is currently buffering a host bracketed-paste frame.
    pub active: &'a mut bool,
    /// Bytes retained until the paste close delimiter arrives or recovery runs.
    pub buffer: &'a mut Vec<u8>,
    /// Monotonic start time for stale malformed-paste recovery.
    pub started_at: &'a mut Option<Instant>,
}

/// Defines the Attached Terminal Client Loop Io behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
#[cfg(test)]
pub trait AttachedTerminalClientLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>>;
    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>>;
    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize>;

    /// Returns retained output bytes awaiting a later writable terminal pass.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    fn pending_output_bytes(&self) -> usize {
        0
    }

    /// Flushes retained output bytes without accepting a new rendered frame.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    fn flush_pending_output(
        &mut self,
        _max_bytes: usize,
    ) -> Result<AttachedTerminalOutputWriteReport> {
        Ok(AttachedTerminalOutputWriteReport::completed(0))
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size(&mut self) -> Result<Option<Size>> {
        Ok(None)
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame(&mut self) -> Result<()> {
        Ok(())
    }

    /// Runs the write output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output_with_modes(
        &mut self,
        lines: &[String],
        _modes: AttachedTerminalOutputModes,
    ) -> Result<usize> {
        self.write_output(lines)
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes(
        &mut self,
        lines: &[String],
        _line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> Result<usize> {
        self.write_output_with_modes(lines, modes)
    }

    /// Writes at most `max_bytes` of one styled terminal frame.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    fn write_styled_output_with_modes_bounded(
        &mut self,
        lines: &[String],
        line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
        _max_bytes: usize,
    ) -> Result<AttachedTerminalOutputWriteReport> {
        let bytes_written = self.write_styled_output_with_modes(lines, line_style_spans, modes)?;
        Ok(AttachedTerminalOutputWriteReport::completed(bytes_written))
    }
}
