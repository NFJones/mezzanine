//! Terminal Client Loop implementation.
//!
//! This module owns the terminal client loop boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::io::ErrorKind;
use std::time::Instant;

use super::mouse::mouse_copy_position;
use super::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, BorrowedFd, MezError, MouseAction,
    MouseEvent, RawFd, Result, TerminalClientLoopConfig, TerminalStyleSpan, classify_mouse_event,
    parse_sgr_mouse,
};
use mez_mux::attached_client::{
    application_cursor_forwarding_bytes, application_mouse_forwarding_bytes,
    earliest_sequence_start, input_sequence_start, malformed_sgr_mouse_prefix_len,
    prefix_sequence_len, sgr_mouse_sequence_len, sgr_mouse_sequence_start,
};
use mez_mux::copy::{CopyModeKeyAction, classify_copy_mode_key_action};
use mez_mux::input::{
    KeyChord, MuxAction, TerminalInputClassification, WindowFocusTarget, classify_prefix_binding,
    classify_terminal_input_with_command_bindings, key_chord_input_bytes, parse_key_chord_bytes,
};
#[cfg(test)]
use mez_mux::layout::Size;
#[cfg(test)]
use mez_mux::presentation::AttachedTerminalOutputModes;
use mez_mux::presentation::{
    ClientStatusLine, RenderedClientView, compose_client_presentation_with_styles,
};

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
    pub const fn completed(bytes_written: usize) -> Self {
        Self {
            bytes_written,
            completed: true,
            pending_bytes: 0,
        }
    }

    /// Returns whether this write left bytes pending.
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
    fn pending_output_bytes(&self) -> usize {
        0
    }

    /// Flushes retained output bytes without accepting a new rendered frame.
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

pub(super) fn borrow_raw_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: callers pass raw descriptors already validated at API boundaries
    // and the returned borrow is consumed by one immediate rustix syscall.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Returns true when a terminal output write failed only because the attached
/// foreground terminal endpoint has already gone away.
pub fn attached_terminal_output_disconnected(error: &MezError) -> bool {
    matches!(
        error.io_kind(),
        Some(
            ErrorKind::BrokenPipe
                | ErrorKind::ConnectionAborted
                | ErrorKind::ConnectionReset
                | ErrorKind::NotConnected
        )
    )
}

/// Routes one host-input unit into a product terminal-loop action.
pub fn route_client_input(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<TerminalClientLoopAction> {
    if config.prefix_key_pending {
        let Some((action, _)) = route_pending_prefix_client_input_action(input, config)? else {
            return Ok(TerminalClientLoopAction::ReportUnboundPrefix(
                config.bindings.escape,
            ));
        };
        return Ok(action);
    }

    if input.starts_with(b"\x1b[<") {
        let Some(event) = parse_sgr_mouse(input) else {
            return Ok(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
        };
        return route_mouse_event(input, event, config);
    }

    if config.mouse_policy.copy_mode_active {
        if config.scrollback_copy_mode_active {
            if let Some(action) = classify_copy_mode_key_action(input) {
                return Ok(TerminalClientLoopAction::HandleCopyMode(action));
            }
        } else {
            let action = classify_copy_mode_key_action(input).unwrap_or(CopyModeKeyAction::Ignore);
            return Ok(TerminalClientLoopAction::HandleCopyMode(action));
        }
    }
    match classify_terminal_input_with_command_bindings(
        input,
        &config.bindings,
        &config.command_bindings,
    )? {
        TerminalInputClassification::ForwardToPane => Ok(TerminalClientLoopAction::ForwardToPane(
            application_cursor_forwarding_bytes(input, config.mouse_policy)
                .unwrap_or_else(|| input.to_vec()),
        )),
        TerminalInputClassification::PrefixKeyMode => {
            Ok(TerminalClientLoopAction::EnterPrefixKeyMode)
        }
        TerminalInputClassification::UnboundPrefix(chord) => {
            Ok(TerminalClientLoopAction::ReportUnboundPrefix(chord))
        }
        TerminalInputClassification::Mouse(event) => route_mouse_event(input, event, config),
        TerminalInputClassification::CommandBinding(command) => {
            Ok(TerminalClientLoopAction::ExecuteCommand(command))
        }
        TerminalInputClassification::Mux(action) => {
            Ok(TerminalClientLoopAction::ExecuteMux(action))
        }
    }
}

/// Runs the route mouse event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn route_mouse_event(
    input: &[u8],
    event: MouseEvent,
    config: &TerminalClientLoopConfig,
) -> Result<TerminalClientLoopAction> {
    if config.primary_display_overlay_active {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Press, super::MouseButton::Left) => {
                    MouseAction::BeginDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Drag, super::MouseButton::Left) => {
                    MouseAction::UpdateDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::FinishDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelUp) => {
                    MouseAction::ScrollDisplayOverlay { lines: -3 }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelDown) => {
                    MouseAction::ScrollDisplayOverlay { lines: 3 }
                }
                _ => MouseAction::Ignore,
            },
        ));
    }

    let mut policy = config.mouse_policy;
    let pane_region = config
        .mouse_pane_regions
        .iter()
        .find(|region| region.contains(event.column, event.row));
    if let Some(region) = pane_region {
        policy.pane_application_mouse_mode = region.application_mouse_mode;
        policy.pane_sgr_mouse_mode = region.application_sgr_mouse_mode;
        policy.copy_mode_active = config.mouse_selection_active || region.copy_mode_active;
    } else {
        policy.pane_application_mouse_mode = false;
        policy.pane_sgr_mouse_mode = false;
        policy.copy_mode_active = config.mouse_selection_active;
    }
    policy.over_pane_border |= config
        .mouse_border_cells
        .iter()
        .any(|cell| cell.column == event.column && cell.row == event.row);
    if let Some(cell) = config
        .mouse_pane_agent_selector_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Scroll, super::MouseButton::WheelUp) => {
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        lines: -3,
                    }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelDown) => {
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        lines: 3,
                    }
                }
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        item_index: cell.item_index,
                    }
                }
                (
                    super::MouseEventKind::Press | super::MouseEventKind::Drag,
                    super::MouseButton::Left,
                ) => MouseAction::HoverPaneAgentStatusSelector {
                    pane_index: cell.pane_index,
                    field: cell.field,
                    item_index: cell.item_index,
                },
                _ => MouseAction::Ignore,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_pane_agent_status_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                    }
                }
                _ => MouseAction::Ignore,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_action_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::PressWindowAction {
                action: cell.action.clone(),
            },
        ));
    }
    if config.frame_context.pressed_window_action.is_some()
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Release, super::MouseButton::Left)
        )
    {
        let action = config
            .mouse_window_action_frame_cells
            .iter()
            .find(|cell| cell.column == event.column && cell.row == event.row)
            .map(|cell| cell.action.clone());
        return Ok(TerminalClientLoopAction::HandleMouse(
            if let Some(action) = action
                && Some(action.clone()) == config.frame_context.pressed_window_action
            {
                MouseAction::ReleaseWindowAction { action }
            } else {
                MouseAction::CancelWindowAction
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_group_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusGroup {
                index: cell.group_index,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusWindow {
                index: cell.window_index,
            },
        ));
    }
    if !config.mouse_pane_agent_selector_cells.is_empty()
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::ClosePaneAgentStatusSelector,
        ));
    }
    policy.over_window_frame |= config
        .mouse_window_frame_cells
        .iter()
        .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_window_action_frame_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_window_group_frame_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_pane_agent_status_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_pane_agent_selector_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row);
    if let Some(region) = pane_region
        && region.application_mouse_mode
        && !region.active
        && !policy.pane_resize_active
        && !policy.copy_mode_active
        && !policy.over_window_frame
        && !policy.over_pane_border
        && matches!(event.kind, super::MouseEventKind::Press)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusPaneOnly(mez_mux::copy::CopyPosition {
                line: usize::from(event.row),
                column: usize::from(event.column),
            }),
        ));
    }
    let action = classify_mouse_event(event, policy);
    if action == MouseAction::ForwardToPane {
        if let Some(region) = pane_region {
            if let Some(input) = application_mouse_forwarding_bytes(event, region) {
                Ok(TerminalClientLoopAction::ForwardMouseToPane {
                    pane_id: region.pane_id.clone(),
                    input,
                })
            } else {
                Ok(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore))
            }
        } else {
            Ok(TerminalClientLoopAction::ForwardToPane(input.to_vec()))
        }
    } else {
        Ok(TerminalClientLoopAction::HandleMouse(action))
    }
}

/// Splits a raw attached-terminal input buffer into mux, prompt, mouse, and
/// pane-forwarding actions without letting batched bytes hide a prefix command.
pub(crate) fn route_client_input_actions(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut host_bracketed_paste_active = config.host_bracketed_paste_active;
    route_client_input_actions_with_host_paste_state(
        input,
        config,
        &mut host_bracketed_paste_active,
    )
}

/// Splits attached-terminal input while preserving host bracketed paste state.
///
/// Pasted payloads must be treated as opaque bytes. Otherwise a large clipboard
/// paste can accidentally trigger mux-prefix commands or mouse handling when a
/// payload chunk happens to contain the configured prefix or SGR-shaped text.
pub(crate) fn route_client_input_actions_with_host_paste_state(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_bracketed_paste_active: &mut bool,
) -> Result<Vec<TerminalClientLoopAction>> {
    const HOST_BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
    const HOST_BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

    let mut remaining = input;
    let mut actions = Vec::new();
    let mut config = config.clone();
    let prefix = key_chord_input_bytes(config.bindings.escape);

    while !remaining.is_empty() {
        if *host_bracketed_paste_active {
            config.prefix_key_pending = false;
            if let Some(end_start) = input_sequence_start(remaining, HOST_BRACKETED_PASTE_END) {
                let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
                actions.push(TerminalClientLoopAction::ForwardToPane(
                    remaining[..consumed].to_vec(),
                ));
                *host_bracketed_paste_active = false;
                remaining = &remaining[consumed..];
                continue;
            }
            actions.push(TerminalClientLoopAction::ForwardToPane(remaining.to_vec()));
            break;
        }

        let paste_start = input_sequence_start(remaining, HOST_BRACKETED_PASTE_START);
        if paste_start == Some(0) {
            config.prefix_key_pending = false;
            if let Some(end_start) = input_sequence_start(remaining, HOST_BRACKETED_PASTE_END) {
                let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
                actions.push(TerminalClientLoopAction::ForwardToPane(
                    remaining[..consumed].to_vec(),
                ));
                remaining = &remaining[consumed..];
                continue;
            }
            actions.push(TerminalClientLoopAction::ForwardToPane(remaining.to_vec()));
            *host_bracketed_paste_active = true;
            break;
        }

        if config.prefix_key_pending {
            let Some((action, consumed)) =
                route_pending_prefix_client_input_action(remaining, &config)?
            else {
                actions.push(TerminalClientLoopAction::ReportUnboundPrefix(
                    config.bindings.escape,
                ));
                config.prefix_key_pending = false;
                break;
            };
            let enters_prompt = action_enters_client_prompt(&action);
            actions.push(action);
            config.prefix_key_pending = false;
            remaining = &remaining[consumed..];
            if enters_prompt {
                break;
            }
            continue;
        }

        let mouse_start = sgr_mouse_sequence_start(remaining);
        let prefix_start = prefix
            .as_deref()
            .and_then(|prefix| input_sequence_start(remaining, prefix));
        let Some(special_start) = earliest_sequence_start([paste_start, mouse_start, prefix_start])
        else {
            actions.push(route_client_input(remaining, &config)?);
            break;
        };

        if special_start > 0 {
            actions.push(route_client_input(&remaining[..special_start], &config)?);
            remaining = &remaining[special_start..];
            continue;
        }

        let prefix_first = prefix_start == Some(0) && mouse_start != Some(0);
        if prefix_first && let Some(prefix) = prefix.as_deref() {
            let Some((action, consumed)) =
                route_prefix_client_input_action(remaining, prefix, &config)?
            else {
                actions.push(route_client_input(remaining, &config)?);
                break;
            };
            let enters_prompt = action_enters_client_prompt(&action);
            let enters_prefix_key_mode =
                matches!(action, TerminalClientLoopAction::EnterPrefixKeyMode);
            actions.push(action);
            if enters_prefix_key_mode {
                config.prefix_key_pending = true;
            }
            remaining = &remaining[consumed..];
            if enters_prompt {
                break;
            }
            continue;
        }

        let Some(mouse_len) = sgr_mouse_sequence_len(remaining) else {
            if let Some(malformed_mouse_prefix_len) = malformed_sgr_mouse_prefix_len(remaining) {
                actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
                remaining = &remaining[malformed_mouse_prefix_len..];
                continue;
            }
            actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
            break;
        };
        let action = route_client_input(&remaining[..mouse_len], &config)?;
        apply_batched_mouse_action_side_effects(&mut config, &action);
        actions.push(action);
        remaining = &remaining[mouse_len..];
    }

    Ok(actions)
}

/// Splits attached-terminal input while buffering incomplete host pastes.
///
/// Unlike `route_client_input_actions_with_host_paste_state`, this path waits
/// for the closing bracketed-paste delimiter before forwarding the payload.
/// That preserves shell heredoc ordering for very large pastes by preventing a
/// partial clipboard body from entering the pane before the terminal has
/// delivered the complete paste frame.
#[cfg(test)]
pub(crate) fn route_client_input_actions_with_host_paste_buffer(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_bracketed_paste_active: &mut bool,
    host_bracketed_paste_buffer: &mut Vec<u8>,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut host_bracketed_paste_started_at = config.host_bracketed_paste_started_at;
    let mut host_paste = HostBracketedPasteBufferState {
        active: host_bracketed_paste_active,
        buffer: host_bracketed_paste_buffer,
        started_at: &mut host_bracketed_paste_started_at,
    };
    route_client_input_actions_with_host_paste_buffer_state(input, config, &mut host_paste)
}

/// Splits attached-terminal input while carrying buffered host paste timing.
fn route_client_input_actions_with_host_paste_buffer_state(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_paste: &mut HostBracketedPasteBufferState<'_>,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut decoder = mez_mux::host_input::HostBracketedPasteDecoder::from_parts(
        *host_paste.active,
        host_paste.buffer.clone(),
        *host_paste.started_at,
    );
    let segments = decoder.decode_at(input, Instant::now());
    *host_paste.active = decoder.active();
    host_paste.buffer.clear();
    host_paste.buffer.extend_from_slice(decoder.buffer());
    *host_paste.started_at = decoder.started_at();

    let mut actions = Vec::new();
    for segment in segments {
        match segment {
            mez_mux::host_input::HostInputSegment::BracketedPaste(bytes) => {
                actions.push(TerminalClientLoopAction::ForwardToPane(bytes));
            }
            mez_mux::host_input::HostInputSegment::Ordinary(bytes) => {
                let mut paste_active = false;
                actions.extend(route_client_input_actions_with_host_paste_state(
                    &bytes,
                    config,
                    &mut paste_active,
                )?);
            }
        }
    }
    Ok(actions)
}

/// Applies routing state transitions for mouse actions emitted from a batched
/// attached-terminal input scan.
fn apply_batched_mouse_action_side_effects(
    config: &mut TerminalClientLoopConfig,
    action: &TerminalClientLoopAction,
) {
    match action {
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { .. }) => {
            config.mouse_policy.pane_resize_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPaneOnly(position)) => {
            set_mouse_region_active_at(config, position.column, position.line);
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionStart(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionUpdate(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane) => {
            config.mouse_policy.pane_resize_active = false;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(_)) => {
            config.mouse_selection_active = false;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::PressWindowAction { action }) => {
            config.frame_context.pressed_window_action = Some(action.clone());
        }
        TerminalClientLoopAction::HandleMouse(
            MouseAction::ReleaseWindowAction { .. } | MouseAction::CancelWindowAction,
        ) => {
            config.frame_context.pressed_window_action = None;
        }
        _ => {}
    }
}

/// Runs the route prefix client input action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn route_prefix_client_input_action(
    input: &[u8],
    prefix: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Option<(TerminalClientLoopAction, usize)>> {
    let Some(consumed) = prefix_sequence_len(input, prefix) else {
        return Ok(None);
    };
    Ok(Some((
        route_client_input(&input[..consumed], config)?,
        consumed,
    )))
}

/// Routes the next key through the prefix table.
///
/// # Parameters
/// - `input`: The raw input beginning with the key that should consume the
///   pending prefix state.
/// - `config`: The active client loop routing configuration.
fn route_pending_prefix_client_input_action(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Option<(TerminalClientLoopAction, usize)>> {
    let Some((chord, consumed)) = parse_key_chord_bytes(input) else {
        return Ok(None);
    };
    if let Some(command) = config.command_bindings.get(&chord) {
        return Ok(Some((
            TerminalClientLoopAction::ExecuteCommand(command.to_string()),
            consumed,
        )));
    }
    let action = classify_prefix_binding(chord, &config.bindings)
        .map(TerminalClientLoopAction::ExecuteMux)
        .unwrap_or(TerminalClientLoopAction::ReportUnboundPrefix(chord));
    Ok(Some((action, consumed)))
}

/// Runs the action enters client prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn action_enters_client_prompt(action: &TerminalClientLoopAction) -> bool {
    matches!(
        action,
        TerminalClientLoopAction::ExecuteMux(
            MuxAction::EnterCommandPrompt
                | MuxAction::RenameWindow
                | MuxAction::KillWindowAfterConfirmation
                | MuxAction::KillPaneAfterConfirmation
                | MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex)
                | MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex)
        )
    )
}

/// Runs the set mouse region active at operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_mouse_region_active_at(config: &mut TerminalClientLoopConfig, column: usize, row: usize) {
    let Ok(column) = u16::try_from(column) else {
        return;
    };
    let Ok(row) = u16::try_from(row) else {
        return;
    };
    let active_pane_id = config
        .mouse_pane_regions
        .iter()
        .find(|region| region.contains(column, row))
        .map(|region| region.pane_id.clone());
    if let Some(active_pane_id) = active_pane_id {
        for region in &mut config.mouse_pane_regions {
            region.active = region.pane_id == active_pane_id;
        }
    }
}

/// Plans one host-terminal client step over neutral mux presentation state.
pub fn plan_attached_terminal_client_step(
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    view: Option<&RenderedClientView>,
    status: Option<&ClientStatusLine>,
    config: &TerminalClientLoopConfig,
) -> Result<AttachedTerminalClientStepPlan> {
    let mut host_bracketed_paste_active = config.host_bracketed_paste_active;
    let mut host_bracketed_paste_buffer = config.host_bracketed_paste_buffer.clone();
    let mut host_bracketed_paste_started_at = config.host_bracketed_paste_started_at;
    let mut host_paste = HostBracketedPasteBufferState {
        active: &mut host_bracketed_paste_active,
        buffer: &mut host_bracketed_paste_buffer,
        started_at: &mut host_bracketed_paste_started_at,
    };
    plan_attached_terminal_client_step_with_host_paste_buffer(
        readiness,
        input,
        view,
        status,
        config,
        &mut host_paste,
    )
}

/// Plans one attached-terminal client step while buffering incomplete host
/// bracketed paste payloads across terminal-read chunks.
pub(crate) fn plan_attached_terminal_client_step_with_host_paste_buffer(
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    view: Option<&RenderedClientView>,
    status: Option<&ClientStatusLine>,
    config: &TerminalClientLoopConfig,
    host_paste: &mut HostBracketedPasteBufferState<'_>,
) -> Result<AttachedTerminalClientStepPlan> {
    let readiness =
        mez_mux::presentation::classify_attached_client_readiness(readiness.iter().map(|ready| {
            mez_mux::presentation::AttachedClientEndpointReadiness {
                role: ready.role,
                input: ready.role == AttachedTerminalFdRole::Input,
                output: ready.role == AttachedTerminalFdRole::Output,
                readable: ready.readable,
                writable: ready.writable,
                hangup: ready.hangup,
                error: ready.error,
            }
        }));

    let mut actions = Vec::new();
    if readiness.input_readable
        && let Some(input) = input
        && !input.is_empty()
    {
        if view.is_some_and(|view| view.primary_prompt_active) {
            actions.push(TerminalClientLoopAction::ForwardToPane(input.to_vec()));
        } else {
            actions.extend(route_client_input_actions_with_host_paste_buffer_state(
                input, config, host_paste,
            )?);
        }
    }
    if actions.is_empty()
        && let Some(position) = config.mouse_selection_autoscroll_position
    {
        actions.push(TerminalClientLoopAction::HandleMouse(
            MouseAction::CopySelectionUpdate(position),
        ));
    }

    let output = view.map(|view| compose_client_presentation_with_styles(view, status));

    Ok(mez_mux::presentation::plan_attached_client_step(
        readiness, actions, output,
    ))
}
