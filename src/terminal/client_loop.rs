//! Terminal Client Loop implementation.
//!
//! This module owns the terminal client loop boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::io::ErrorKind;
#[cfg(test)]
use std::time::{Duration, Instant};

use super::keys::classify_prefix_binding;
use super::mouse::mouse_copy_position;
#[cfg(test)]
use super::{
    AttachedTerminalFd, Errno, TerminalFdInterest, UiTheme, poll_attached_terminal_fd_readiness,
    read_attached_terminal_size, rustix_read, rustix_write,
};
use super::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, BorrowedFd, CopyModeKeyAction, KeyChord,
    KeyCode, MezError, MouseAction, MouseEvent, MousePolicy, MuxAction, RawFd, Result, Size,
    TerminalClientLoopConfig, TerminalColor, TerminalCursorStyle, TerminalInputClassification,
    TerminalStyleSpan, WindowFocusTarget, classify_mouse_event,
    classify_terminal_input_with_command_bindings, compose_client_presentation_with_styles,
    key_chord_input_bytes, parse_key_chord_bytes, parse_sgr_mouse, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width,
};

// Attached terminal loop planning and I/O abstraction.

/// Refresh cadence for active agent status animations.
///
/// Renderers advance the scan phase at this interval, and attach clients use
/// the same value to request fresh views only while animation is active.
pub const AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS: u64 = 180;

/// Maximum buffered attached-terminal host bracketed-paste payload.
///
/// Malformed terminal paste frames must not swallow ordinary input forever or
/// grow without bound if the closing delimiter never arrives.
pub const HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES: usize = 1024 * 1024;

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

/// Carries Client View Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientViewRole {
    /// Represents the Primary case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Primary,
    /// Represents the Pending Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PendingObserver,
    /// Represents the Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Observer,
}

/// Carries Rendered Client View state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedClientView {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ClientViewRole,
    /// Stores the authoritative size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authoritative_size: Size,
    /// Stores the client size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_size: Size,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub lines: Vec<String>,
    /// Per-line non-default SGR style spans aligned to `lines`.
    pub line_style_spans: Vec<Vec<super::TerminalStyleSpan>>,
    /// Active copy-mode selection range, including submitted pager search matches.
    pub selection: Option<(super::CopyPosition, super::CopyPosition)>,
    /// Stores the requires client scroll value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requires_client_scroll: bool,
    /// Stores the viewport row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub viewport_row: usize,
    /// Stores the viewport column value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub viewport_column: usize,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_visible: bool,
    /// Stores the cursor style value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_style: TerminalCursorStyle,
    /// Stores the cursor blink value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_blink: bool,
    /// Stores the cursor blink interval ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_blink_interval_ms: u64,
    /// Stores the application keypad value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub application_keypad: bool,
    /// Whether host bracketed paste should be enabled for the attached terminal.
    ///
    /// This mirrors the active pane application mode so host clipboard pastes
    /// arrive with `CSI 200~`/`CSI 201~` delimiters and can be routed opaquely.
    pub bracketed_paste: bool,
    /// Whether the attached terminal should request host mouse reporting.
    ///
    /// This mirrors the foreground client mouse policy so serialized client
    /// views can preserve the same host-mouse capture decision as local frames.
    pub host_mouse_reporting: bool,
    /// Milliseconds between client-requested animation refreshes.
    ///
    /// A zero value means the view does not require animation refreshes.
    pub animation_refresh_interval_ms: u64,
    /// Stores the ui theme value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ui_theme: super::UiTheme,
    /// Stores the agent prompt region value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_prompt_region: Option<ReadlinePromptRegion>,
    /// Stores the primary prompt active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_prompt_active: bool,
}

/// Absolute client-space region where pane-scoped overlays can be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadlinePromptRegion {
    /// Stores the row value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub row: usize,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub column: usize,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub columns: usize,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rows: usize,
}

/// Carries Client Status Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientStatusKind {
    /// Represents the Plain case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Plain,
    /// Represents the Copy Mode case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CopyMode,
    /// Represents the Pending Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PendingObserver,
    /// Represents the Diagnostic case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Diagnostic,
}

/// Carries Client Status Line state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStatusLine {
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: ClientStatusKind,
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub text: String,
}

/// Carries Attached Terminal Output Modes state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedTerminalOutputModes {
    /// Stores the application keypad value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub application_keypad: bool,
    /// Whether the attached terminal should request host bracketed paste.
    ///
    /// When enabled, paste payloads are delimited by the host terminal and can
    /// be forwarded to the pane without interpreting Mezzanine prefix commands
    /// or mouse reports embedded in the pasted bytes.
    pub bracketed_paste: bool,
    /// Whether the attached terminal should request host mouse reporting.
    ///
    /// This mirrors the foreground client's configured mouse policy so
    /// Mezzanine only captures host mouse input when mouse support is enabled.
    pub host_mouse_reporting: bool,
    /// Stores the cursor style value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_style: TerminalCursorStyle,
    /// Stores the cursor blink value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_blink: bool,
    /// Stores the cursor blink interval ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_blink_interval_ms: u64,
    /// Stores the cursor blink elapsed ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_blink_elapsed_ms: u64,
    /// Milliseconds between client-requested animation refreshes.
    ///
    /// A zero value means the view does not require animation refreshes.
    pub animation_refresh_interval_ms: u64,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor_visible: bool,
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
}

impl Default for AttachedTerminalOutputModes {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            application_keypad: false,
            bracketed_paste: false,
            host_mouse_reporting: true,
            cursor_style: TerminalCursorStyle::default(),
            cursor_blink: false,
            cursor_blink_interval_ms: 500,
            cursor_blink_elapsed_ms: 0,
            animation_refresh_interval_ms: 0,
            cursor_visible: false,
            cursor_row: 0,
            cursor_column: 0,
        }
    }
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

/// Carries Attached Terminal Client Step Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedTerminalClientStepPlan {
    /// Stores the actions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub actions: Vec<TerminalClientLoopAction>,
    /// Stores the output lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_lines: Vec<String>,
    /// Stores the output line style spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the input hangup value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input_hangup: bool,
    /// Stores the output hangup value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_hangup: bool,
    /// Stores the error roles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub error_roles: Vec<AttachedTerminalFdRole>,
}

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
}

/// Carries Attached Terminal Fd Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub struct AttachedTerminalFdLoopIo {
    /// Stores the descriptors value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) descriptors: Vec<AttachedTerminalFd>,
    /// Stores the output descriptor value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_descriptor: AttachedTerminalFd,
    /// Stores the input fd value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) input_fd: RawFd,
    /// Stores the output fd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_fd: RawFd,
    /// Stores the poll timeout value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) poll_timeout: Option<Duration>,
    /// Stores the application keypad mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) application_keypad_mode: bool,
    /// Stores the previous output frame value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) previous_output_frame: Option<AttachedTerminalOutputFrameState>,
    /// Stores the presentation active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) presentation_active: bool,
}

/// Last attached-terminal frame content retained for differential redraws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachedTerminalOutputFrameState {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Whether host bracketed paste was enabled for the retained frame.
    bracketed_paste: bool,
    /// Whether host mouse reporting was enabled for the retained frame.
    host_mouse_reporting: bool,
    /// Cursor presentation sequence emitted by the retained frame.
    cursor_presentation: String,
}

impl AttachedTerminalOutputFrameState {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(crate) fn new(lines: &[String], line_style_spans: &[Vec<TerminalStyleSpan>]) -> Self {
        Self::new_with_modes(
            lines,
            line_style_spans,
            AttachedTerminalOutputModes::default(),
        )
    }

    /// Builds retained frame state using the presentation modes emitted with
    /// the frame.
    pub(crate) fn new_with_modes(
        lines: &[String],
        line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> Self {
        Self {
            lines: lines.to_vec(),
            line_style_spans: normalized_style_span_rows(line_style_spans, lines.len()),
            bracketed_paste: modes.bracketed_paste,
            host_mouse_reporting: modes.host_mouse_reporting,
            cursor_presentation: cursor_presentation_sequence(lines, modes),
        }
    }
}

#[cfg(test)]
impl AttachedTerminalFdLoopIo {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        input_fd: RawFd,
        output_fd: RawFd,
        control_fd: Option<RawFd>,
        poll_timeout: Option<Duration>,
    ) -> Result<Self> {
        let input = AttachedTerminalFd::input(input_fd, TerminalFdInterest::read())?;
        let output = AttachedTerminalFd::output(output_fd, TerminalFdInterest::write())?;
        let mut descriptors = vec![input];
        if let Some(control_fd) = control_fd {
            descriptors.push(AttachedTerminalFd::control(
                control_fd,
                TerminalFdInterest::read(),
            )?);
        }
        Ok(Self {
            descriptors,
            output_descriptor: output,
            input_fd,
            output_fd,
            poll_timeout,
            application_keypad_mode: false,
            previous_output_frame: None,
            presentation_active: false,
        })
    }

    /// Enters Mezzanine's foreground presentation surface on the attached TTY.
    pub fn enter_presentation(&mut self) -> Result<()> {
        if self.presentation_active {
            return Ok(());
        }
        write_all_attached_terminal_fd(
            self.output_fd,
            attached_terminal_enter_presentation_frame(),
        )?;
        self.presentation_active = true;
        Ok(())
    }

    /// Restores host-visible terminal presentation state after detaching.
    pub fn restore_presentation(&mut self) -> Result<()> {
        if !self.presentation_active {
            return Ok(());
        }
        match write_all_attached_terminal_fd(
            self.output_fd,
            attached_terminal_restore_presentation_frame(),
        ) {
            Ok(()) => {}
            Err(error) if attached_terminal_output_disconnected(&error) => {}
            Err(error) => return Err(error),
        }
        self.presentation_active = false;
        Ok(())
    }
}

#[cfg(test)]
impl Drop for AttachedTerminalFdLoopIo {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        let _ = self.restore_presentation();
    }
}

#[cfg(test)]
impl AttachedTerminalClientLoopIo for AttachedTerminalFdLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>> {
        let mut readiness =
            poll_attached_terminal_fd_readiness(&self.descriptors, self.poll_timeout)?;
        readiness.extend(poll_attached_terminal_fd_readiness(
            std::slice::from_ref(&self.output_descriptor),
            Some(Duration::ZERO),
        )?);
        Ok(readiness)
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        read_attached_terminal_fd(self.input_fd, max_bytes)
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize> {
        self.write_output_with_modes(lines, AttachedTerminalOutputModes::default())
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size(&mut self) -> Result<Option<Size>> {
        read_attached_terminal_size(self.output_fd)
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame(&mut self) -> Result<()> {
        self.previous_output_frame = None;
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
        modes: AttachedTerminalOutputModes,
    ) -> Result<usize> {
        self.write_styled_output_with_modes(lines, &[], modes)
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes(
        &mut self,
        lines: &[String],
        line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> Result<usize> {
        let keypad_transition = if modes.application_keypad != self.application_keypad_mode {
            self.application_keypad_mode = modes.application_keypad;
            Some(modes.application_keypad)
        } else {
            None
        };
        let frame = encode_attached_terminal_output_update_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
            self.previous_output_frame.as_ref(),
        );
        write_all_attached_terminal_fd(self.output_fd, &frame)?;
        self.previous_output_frame = Some(AttachedTerminalOutputFrameState::new_with_modes(
            lines,
            line_style_spans,
            modes,
        ));
        Ok(frame.len())
    }
}

/// Runs the read attached terminal fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn read_attached_terminal_fd(fd: RawFd, max_bytes: usize) -> Result<Vec<u8>> {
    if max_bytes == 0 {
        return Err(MezError::invalid_args(
            "attached terminal input read limit must be greater than zero",
        ));
    }
    let mut buffer = vec![0u8; max_bytes];
    loop {
        match rustix_read(borrow_raw_fd(fd), buffer.as_mut_slice()) {
            Ok(count) => {
                buffer.truncate(count);
                return Ok(buffer);
            }
            Err(Errno::INTR) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(Vec::new()),
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
}

/// Runs the borrow raw fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn borrow_raw_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: callers pass raw descriptors already validated at API boundaries
    // and the returned borrow is consumed by one immediate rustix syscall.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Runs the write all attached terminal fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn write_all_attached_terminal_fd(fd: RawFd, bytes: &[u8]) -> Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        match rustix_write(borrow_raw_fd(fd), &bytes[written..]) {
            Ok(count) if count > 0 => {
                written = written.saturating_add(count);
            }
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "attached terminal output write made no progress",
                ));
            }
            Err(Errno::INTR) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    Ok(())
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

/// Runs the encode attached terminal output frame with keypad transition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn encode_attached_terminal_output_frame_with_keypad_transition(
    lines: &[String],
    keypad_transition: Option<bool>,
) -> Vec<u8> {
    encode_attached_terminal_output_frame_with_styles(
        lines,
        &[],
        keypad_transition,
        AttachedTerminalOutputModes::default(),
    )
}

/// Runs the encode attached terminal output frame with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn encode_attached_terminal_output_frame_with_styles(
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    keypad_transition: Option<bool>,
    modes: AttachedTerminalOutputModes,
) -> Vec<u8> {
    let mut frame = Vec::new();
    match keypad_transition {
        Some(true) => frame.extend_from_slice(b"\x1b="),
        Some(false) => frame.extend_from_slice(b"\x1b>"),
        None => {}
    }
    frame.extend_from_slice(attached_terminal_enter_presentation_frame());
    frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
        modes.host_mouse_reporting,
    ));
    frame.extend_from_slice(attached_terminal_bracketed_paste_frame(
        modes.bracketed_paste,
    ));
    frame.extend_from_slice(b"\x1b[2J\x1b[H");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            frame.extend_from_slice(b"\r\n\x1b[0m");
        }
        let spans = line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        frame.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
    }
    frame.extend_from_slice(cursor_presentation_sequence(lines, modes).as_bytes());
    frame
}

/// Encodes either a full redraw or a row-differential update for an attached TTY.
///
/// The first frame and row-count changes still get a full redraw. Stable-row
/// updates rewrite only the rows whose text or SGR spans changed before
/// restoring Mezzanine's cursor. Rows that shrink are cleared before their new
/// content is written, avoiding a full-screen clear without relying on
/// erase-after-text behavior at the final terminal column.
pub(crate) fn encode_attached_terminal_output_update_frame_with_styles(
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    keypad_transition: Option<bool>,
    modes: AttachedTerminalOutputModes,
    previous: Option<&AttachedTerminalOutputFrameState>,
) -> Vec<u8> {
    let Some(previous) = previous else {
        return encode_attached_terminal_output_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
        );
    };
    if output_row_count_changed(previous, lines) {
        return encode_attached_terminal_output_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
        );
    }

    let mut frame = Vec::new();
    match keypad_transition {
        Some(true) => frame.extend_from_slice(b"\x1b="),
        Some(false) => frame.extend_from_slice(b"\x1b>"),
        None => {}
    }
    if previous.bracketed_paste != modes.bracketed_paste {
        frame.extend_from_slice(attached_terminal_bracketed_paste_frame(
            modes.bracketed_paste,
        ));
    }
    if previous.host_mouse_reporting != modes.host_mouse_reporting {
        frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
            modes.host_mouse_reporting,
        ));
    }
    let changed_row_count = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            let previous_line = previous.lines.get(*index).map(String::as_str).unwrap_or("");
            let previous_spans = previous
                .line_style_spans
                .get(*index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let spans = line_style_spans
                .get(*index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            line.as_str() != previous_line || spans != previous_spans
        })
        .count();
    let allow_segment_updates = changed_row_count <= 3;
    let mut changed_rows = 0usize;
    let mut presentation_reset_emitted = false;
    for (index, line) in lines.iter().enumerate() {
        let previous_line = previous.lines.get(index).map(String::as_str).unwrap_or("");
        let previous_spans = previous
            .line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let spans = line_style_spans
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if line.as_str() == previous_line && spans == previous_spans {
            continue;
        }
        if !presentation_reset_emitted {
            frame.extend_from_slice(attached_terminal_enter_presentation_frame());
            frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
                modes.host_mouse_reporting,
            ));
            presentation_reset_emitted = true;
        }
        let row = index.saturating_add(1);
        if allow_segment_updates
            && previous_spans.is_empty()
            && spans.is_empty()
            && let Some(span_update) =
                encode_safe_changed_row_span_update(row, previous_line, line, previous_spans, spans)
        {
            frame.extend_from_slice(&span_update);
        } else {
            frame.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
            frame.extend_from_slice(b"\x1b[0m");
            if terminal_line_width(line) < terminal_line_width(previous_line) {
                frame.extend_from_slice(b"\x1b[2K");
            }
            frame.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
        }
        changed_rows = changed_rows.saturating_add(1);
    }
    let cursor_presentation = cursor_presentation_sequence(lines, modes);
    if changed_rows > 0 || cursor_presentation != previous.cursor_presentation {
        if !presentation_reset_emitted {
            frame.extend_from_slice(attached_terminal_enter_presentation_frame());
            frame.extend_from_slice(attached_terminal_mouse_reporting_frame(
                modes.host_mouse_reporting,
            ));
        }
        frame.extend_from_slice(cursor_presentation.as_bytes());
    }
    frame
}

/// Runs the normalized style span rows operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalized_style_span_rows(
    line_style_spans: &[Vec<TerminalStyleSpan>],
    line_count: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    (0..line_count)
        .map(|index| line_style_spans.get(index).cloned().unwrap_or_default())
        .collect()
}
/// Builds style rows for full terminal presentation output lines from the same
/// rendered view that produced the text rows.
///
/// The function intentionally drops style rows unless the caller-provided rows
/// are the complete rendered presentation. Text equality for a partial row slice
/// is not a safe provenance signal: agent output such as apply-patch diff
/// previews can match rows already visible in an unfocused pane, and reusing
/// those render-owned spans can apply hidden or overlay attributes to unrelated
/// output.
#[cfg(test)]
pub(crate) fn compose_terminal_output_style_spans(
    output_lines: &[String],
    rendered: Option<&(RenderedClientView, Option<ClientStatusLine>)>,
) -> Vec<Vec<TerminalStyleSpan>> {
    let Some((view, status)) = rendered else {
        return Vec::new();
    };
    let (styled_lines, line_style_spans) =
        compose_client_presentation_with_styles(view, status.as_ref());
    if styled_lines == output_lines {
        normalized_style_span_rows(&line_style_spans, output_lines.len())
    } else {
        Vec::new()
    }
}
/// Verifies focused render-only style rows are not reused for changed diff text.
///
/// This regression protects apply-patch previews containing Rust option text such
/// as `Some` and `None`. A focused primary view can carry overlay style spans for
/// the rendered rows; if the terminal writer receives a different set of output
/// rows, those spans must be dropped instead of being applied to the new diff
/// text where they can hide otherwise-present symbols.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_drop_focused_overlay_spans_for_mismatched_diff_rows() {
    let hidden_some_span = TerminalStyleSpan {
        start: 9,
        length: 4,
        rendition: super::GraphicRendition {
            hidden: true,
            ..super::GraphicRendition::default()
        },
    };
    let rendered_view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(40, 2).unwrap(),
        client_size: Size::new(40, 2).unwrap(),
        lines: vec![
            "agent output".to_string(),
            "- value: Some(None)".to_string(),
        ],
        line_style_spans: vec![Vec::new(), vec![hidden_some_span]],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: true,
    };
    let output_lines = vec!["+ value: Some(None)".to_string()];

    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));

    assert!(style_spans.is_empty(), "{style_spans:?}");
}

/// Verifies hidden render spans are not reused for matching diff row slices.
///
/// This regression covers apply-patch previews in unfocused panes. The textual
/// diff row can already be present in the rendered view, but matching that text
/// does not prove that the output write owns the render spans. Hidden spans from
/// the stale presentation must not be copied onto the new output where they can
/// make Rust tokens such as `Some` and `None` invisible.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_drop_hidden_spans_for_matching_diff_row_slices() {
    let hidden_some_span = TerminalStyleSpan {
        start: 9,
        length: 4,
        rendition: super::GraphicRendition {
            hidden: true,
            ..super::GraphicRendition::default()
        },
    };
    let rendered_view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(40, 3).unwrap(),
        client_size: Size::new(40, 3).unwrap(),
        lines: vec![
            "agent output".to_string(),
            "+ value: Some(None)".to_string(),
            "done".to_string(),
        ],
        line_style_spans: vec![Vec::new(), vec![hidden_some_span], Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };
    let output_lines = vec!["+ value: Some(None)".to_string()];

    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));

    assert!(style_spans.is_empty(), "{style_spans:?}");
}

/// Verifies later overlay spans preserve earlier diff-token foreground colors.
///
/// This regression covers the focused apply-patch preview path where a later
/// selection or focus overlay contributes background styling on top of an
/// existing syntax or diff foreground. The attached-terminal encoder must merge
/// the overlapping spans instead of letting the later overlay replace the whole
/// rendition, or Rust tokens such as `Some` can become invisible in focused
/// panes.
#[cfg(test)]
#[test]
fn terminal_output_style_spans_merge_overlay_background_with_diff_foreground() {
    let encoded = encode_styled_terminal_line(
        "+ value: Some(None)",
        &[
            TerminalStyleSpan {
                start: 9,
                length: 4,
                rendition: super::GraphicRendition {
                    foreground: Some(super::TerminalColor::Indexed(2)),
                    ..super::GraphicRendition::default()
                },
            },
            TerminalStyleSpan {
                start: 9,
                length: 4,
                rendition: super::GraphicRendition {
                    background: Some(super::TerminalColor::Indexed(4)),
                    ..super::GraphicRendition::default()
                },
            },
        ],
    );

    assert!(
        encoded.contains("+ value: \x1b[0;32;44mSome"),
        "{encoded:?}"
    );
}

/// Runs the output row count changed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn output_row_count_changed(previous: &AttachedTerminalOutputFrameState, lines: &[String]) -> bool {
    previous.lines.len() != lines.len()
}

/// Encodes a bounded row-segment update when the changed text occupies a stable
/// display-column range.
fn encode_safe_changed_row_span_update(
    row: usize,
    previous_line: &str,
    line: &str,
    previous_spans: &[TerminalStyleSpan],
    spans: &[TerminalStyleSpan],
) -> Option<Vec<u8>> {
    if terminal_line_width(previous_line) != terminal_line_width(line) {
        return None;
    }

    let previous_cells = terminal_row_cells(previous_line, previous_spans);
    let current_cells = terminal_row_cells(line, spans);
    if previous_cells.len() != current_cells.len() {
        return None;
    }
    let start = previous_cells
        .iter()
        .zip(current_cells.iter())
        .position(|(previous, current)| !terminal_row_cells_match(previous, current))?;
    let mut previous_end = previous_cells.len();
    let mut current_end = current_cells.len();
    while previous_end > start
        && current_end > start
        && terminal_row_cells_match(
            &previous_cells[previous_end.saturating_sub(1)],
            &current_cells[current_end.saturating_sub(1)],
        )
    {
        previous_end = previous_end.saturating_sub(1);
        current_end = current_end.saturating_sub(1);
    }

    let start_column = current_cells[start].column_start;
    let current_end_cell = &current_cells[current_end.saturating_sub(1)];
    let end_column = current_end_cell.column_end;
    let (start_column, end_column) =
        expand_changed_column_range(previous_spans, spans, start_column, end_column);
    let start_cell = current_cells
        .iter()
        .position(|cell| cell.column_end > start_column)?;
    // When start_column falls inside a wide glyph continuation cell,
    // the position above skips the leading cell. Align start_column
    // back to the leading cell's start so that clipped style spans
    // match the segment text byte offsets.
    let start_column = start_column.min(current_cells[start_cell].column_start);
    let end_cell = current_cells
        .iter()
        .rposition(|cell| cell.column_start < end_column)?;
    let segment = &line[current_cells[start_cell].byte_start..current_cells[end_cell].byte_end];

    let segment_spans = clip_style_spans_to_column_range(spans, start_column, end_column);
    let encoded_segment = encode_styled_terminal_line(segment, &segment_spans);
    let mut span_update =
        format!("\x1b[{row};{}H\x1b[0m", start_column.saturating_add(1)).into_bytes();
    span_update.extend_from_slice(encoded_segment.as_bytes());

    let mut row_update = format!("\x1b[{row};1H\x1b[0m").into_bytes();
    row_update.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
    (span_update.len() < row_update.len()).then_some(span_update)
}

/// Expands one changed column range to include any overlapping style spans.
fn expand_changed_column_range(
    previous_spans: &[TerminalStyleSpan],
    spans: &[TerminalStyleSpan],
    start: usize,
    end: usize,
) -> (usize, usize) {
    let mut expanded_start = start;
    let mut expanded_end = end;
    loop {
        let mut changed = false;
        for span in previous_spans.iter().chain(spans.iter()) {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            if span_start < expanded_end && span_end > expanded_start {
                let next_start = expanded_start.min(span_start);
                let next_end = expanded_end.max(span_end);
                if next_start != expanded_start || next_end != expanded_end {
                    expanded_start = next_start;
                    expanded_end = next_end;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (expanded_start, expanded_end)
}

/// Carries one rendered grapheme cell plus the rendition active across it.
#[derive(Debug, Clone, Copy)]
struct TerminalRowCell<'a> {
    /// Source slice for the grapheme occupying this display-cell span.
    text: &'a str,
    /// Inclusive byte offset at which the grapheme begins.
    byte_start: usize,
    /// Exclusive byte offset at which the grapheme ends.
    byte_end: usize,
    /// Inclusive display column at which the grapheme begins.
    column_start: usize,
    /// Exclusive display column at which the grapheme ends.
    column_end: usize,
    /// Active terminal rendition for this grapheme span.
    rendition: super::GraphicRendition,
}

/// Collects rendered graphemes into display-cell spans with their active style.
fn terminal_row_cells<'a>(line: &'a str, spans: &[TerminalStyleSpan]) -> Vec<TerminalRowCell<'a>> {
    let mut cells = Vec::new();
    let mut search_offset = 0usize;
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let Some(relative_start) = line[search_offset..].find(grapheme) else {
            debug_assert!(
                false,
                "terminal_graphemes produced a grapheme not findable in line at offset {search_offset}"
            );
            return Vec::new();
        };
        let byte_start = search_offset.saturating_add(relative_start);
        let byte_end = byte_start.saturating_add(grapheme.len());
        let width = terminal_grapheme_width(grapheme);
        cells.push(TerminalRowCell {
            text: grapheme,
            byte_start,
            byte_end,
            column_start: column,
            column_end: column.saturating_add(width),
            rendition: rendition_at_column(spans, column),
        });
        search_offset = byte_end;
        column = column.saturating_add(width);
    }
    cells
}

/// Returns whether two rendered grapheme cells are visually identical.
fn terminal_row_cells_match(previous: &TerminalRowCell<'_>, current: &TerminalRowCell<'_>) -> bool {
    previous.text == current.text
        && previous.column_start == current.column_start
        && previous.column_end == current.column_end
        && previous.rendition == current.rendition
}

/// Clips row style spans to a changed column range.
fn clip_style_spans_to_column_range(
    spans: &[TerminalStyleSpan],
    start: usize,
    end: usize,
) -> Vec<TerminalStyleSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            let clipped_start = span_start.max(start);
            let clipped_end = span_end.min(end);
            (clipped_start < clipped_end).then_some(TerminalStyleSpan {
                start: clipped_start.saturating_sub(start),
                length: clipped_end.saturating_sub(clipped_start),
                rendition: span.rendition,
            })
        })
        .collect()
}

/// Runs the terminal line width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_line_width(line: &str) -> usize {
    terminal_text_width(line)
}

/// Defines the fn const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const fn attached_terminal_enter_presentation_frame() -> &'static [u8] {
    b"\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h"
}

/// Returns the host mouse reporting DEC private-mode sequence for a frame.
const fn attached_terminal_mouse_reporting_frame(enabled: bool) -> &'static [u8] {
    if enabled {
        b"\x1b[?1000;1002;1006h"
    } else {
        b"\x1b[?1006l\x1b[?1002l\x1b[?1000l"
    }
}

/// Returns the host bracketed-paste DEC private-mode sequence for a frame.
const fn attached_terminal_bracketed_paste_frame(enabled: bool) -> &'static [u8] {
    if enabled {
        b"\x1b[?2004h"
    } else {
        b"\x1b[?2004l"
    }
}

/// Defines the fn const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const fn attached_terminal_restore_presentation_frame() -> &'static [u8] {
    b"\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b>\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[2J\x1b[H\x1b[?25h\x1b[0 q"
}

/// Runs the cursor presentation sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_presentation_sequence(lines: &[String], modes: AttachedTerminalOutputModes) -> String {
    if !cursor_phase_visible(modes) {
        return "\x1b[?25l\x1b[0m".to_string();
    }
    let row = modes
        .cursor_row
        .min(lines.len().saturating_sub(1))
        .saturating_add(1);
    let frame_width = lines
        .iter()
        .map(|line| terminal_line_width(line))
        .max()
        .unwrap_or(1)
        .max(1);
    let column = modes
        .cursor_column
        .min(frame_width.saturating_sub(1))
        .saturating_add(1);
    let style = modes.cursor_style.decscusr_parameter(false);
    format!("\x1b[?25l\x1b[0m\x1b[{style} q\x1b[{row};{column}H\x1b[?25h")
}

/// Runs the cursor phase visible operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_phase_visible(modes: AttachedTerminalOutputModes) -> bool {
    if !modes.cursor_visible {
        return false;
    }
    if !modes.cursor_blink || modes.cursor_blink_interval_ms == 0 {
        return true;
    }
    let visible_ms = (modes.cursor_blink_interval_ms / 2).max(1);
    modes.cursor_blink_elapsed_ms % modes.cursor_blink_interval_ms < visible_ms
}

/// Runs the cursor blink elapsed ms operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn cursor_blink_elapsed_ms(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Runs the encode styled terminal line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_styled_terminal_line(line: &str, style_spans: &[TerminalStyleSpan]) -> String {
    let mut encoded = String::new();
    let mut active = super::GraphicRendition::default();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let sanitized = sanitize_terminal_output_grapheme(grapheme);
        if sanitized.is_empty() {
            continue;
        }
        let rendition = rendition_at_column(style_spans, column);
        if rendition != active {
            encoded.push_str(&sgr_sequence(rendition));
            active = rendition;
        }
        encoded.push_str(sanitized.as_str());
        column = column.saturating_add(terminal_grapheme_width(grapheme));
    }
    encoded
}

/// Returns terminal-display text for one rendered grapheme cluster.
///
/// Rendered pane text is untrusted by the attached terminal writer. Control
/// bytes that reach this final boundary must be removed so only Mezzanine-owned
/// framing, cursor, and SGR sequences can affect the host terminal.
fn sanitize_terminal_output_grapheme(grapheme: &str) -> String {
    grapheme.chars().filter(|ch| !ch.is_control()).collect()
}

/// Returns the active rendition at a display column.
///
/// Spans must be in composition order so later spans can augment earlier ones.
/// This function folds every covering span in that order, preserving earlier
/// attributes when a later overlay leaves them unspecified. Callers must ensure
/// spans are either from [`terminal_styled_lines_from_canvas`] or from
/// canvas-composed sources where later spans represent later composition
/// layers.
fn rendition_at_column(
    style_spans: &[TerminalStyleSpan],
    column: usize,
) -> super::GraphicRendition {
    style_spans
        .iter()
        .filter(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .fold(super::GraphicRendition::default(), |active, span| {
            merge_graphic_renditions(active, span.rendition)
        })
}

/// Merges one later style layer into the accumulated active rendition.
///
/// Terminal style spans act as partial overlays rather than full terminal-state
/// snapshots. Later overlays such as copy-selection highlights should keep an
/// earlier diff or syntax foreground unless they explicitly replace that color.
fn merge_graphic_renditions(
    active: super::GraphicRendition,
    overlay: super::GraphicRendition,
) -> super::GraphicRendition {
    super::GraphicRendition {
        bold: active.bold || overlay.bold,
        dim: active.dim || overlay.dim,
        italic: active.italic || overlay.italic,
        underline: active.underline
            || overlay.underline
            || active.double_underline
            || overlay.double_underline,
        double_underline: active.double_underline || overlay.double_underline,
        strikethrough: active.strikethrough || overlay.strikethrough,
        inverse: active.inverse || overlay.inverse,
        hidden: active.hidden || overlay.hidden,
        foreground: overlay.foreground.or(active.foreground),
        background: overlay.background.or(active.background),
    }
}

/// Runs the sgr sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sgr_sequence(rendition: super::GraphicRendition) -> String {
    if rendition == super::GraphicRendition::default() {
        return "\x1b[0m".to_string();
    }
    let mut codes = vec!["0".to_string()];
    if rendition.bold {
        codes.push("1".to_string());
    }
    if rendition.dim {
        codes.push("2".to_string());
    }
    if rendition.italic {
        codes.push("3".to_string());
    }
    if rendition.underline {
        if rendition.double_underline {
            codes.push("21".to_string());
        } else {
            codes.push("4".to_string());
        }
    }
    if rendition.strikethrough {
        codes.push("9".to_string());
    }
    if rendition.inverse {
        codes.push("7".to_string());
    }
    if rendition.hidden {
        codes.push("8".to_string());
    }
    if let Some(color) = rendition.foreground {
        push_sgr_color_codes(&mut codes, color, false);
    }
    if let Some(color) = rendition.background {
        push_sgr_color_codes(&mut codes, color, true);
    }
    format!("\x1b[{}m", codes.join(";"))
}

/// Runs the push sgr color codes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_sgr_color_codes(codes: &mut Vec<String>, color: TerminalColor, background: bool) {
    match color {
        TerminalColor::Indexed(index) if index < 8 => {
            codes.push((u16::from(index) + if background { 40 } else { 30 }).to_string());
        }
        TerminalColor::Indexed(index) if index < 16 => {
            codes.push((u16::from(index - 8) + if background { 100 } else { 90 }).to_string());
        }
        TerminalColor::Indexed(index) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("5".to_string());
            codes.push(index.to_string());
        }
        TerminalColor::Rgb(red, green, blue) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("2".to_string());
            codes.push(red.to_string());
            codes.push(green.to_string());
            codes.push(blue.to_string());
        }
    }
}

/// Runs the route client input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
        let Some(event) = parse_sgr_mouse(input)? else {
            return Ok(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
        };
        return route_mouse_event(input, event, config);
    }

    if config.mouse_policy.copy_mode_active {
        let action = classify_copy_mode_key_action(input).unwrap_or(CopyModeKeyAction::Ignore);
        return Ok(TerminalClientLoopAction::HandleCopyMode(action));
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
            MouseAction::FocusPaneOnly(super::CopyPosition {
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
        let Some(special_start) = earliest_sequence_start(
            earliest_sequence_start(paste_start, mouse_start),
            prefix_start,
        ) else {
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
            actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
            break;
        };
        let action = route_client_input(&remaining[..mouse_len], &config)?;
        apply_batched_mouse_action_side_effects(
            &mut config,
            &action,
            BatchedMouseSideEffectMode::ImmediateForwarding,
        );
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
pub(crate) fn route_client_input_actions_with_host_paste_buffer(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_bracketed_paste_active: &mut bool,
    host_bracketed_paste_buffer: &mut Vec<u8>,
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
            host_bracketed_paste_buffer.extend_from_slice(remaining);
            if host_bracketed_paste_buffer.len() > HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES {
                let buffered = std::mem::take(host_bracketed_paste_buffer);
                *host_bracketed_paste_active = false;
                actions.push(TerminalClientLoopAction::ForwardToPane(buffered));
                return Ok(actions);
            }
            let Some(end_start) =
                input_sequence_start(host_bracketed_paste_buffer, HOST_BRACKETED_PASTE_END)
            else {
                return Ok(actions);
            };
            let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
            let suffix = host_bracketed_paste_buffer[consumed..].to_vec();
            actions.push(TerminalClientLoopAction::ForwardToPane(
                host_bracketed_paste_buffer[..consumed].to_vec(),
            ));
            host_bracketed_paste_buffer.clear();
            *host_bracketed_paste_active = false;
            if !suffix.is_empty() {
                actions.extend(route_client_input_actions_with_host_paste_buffer(
                    &suffix,
                    &config,
                    host_bracketed_paste_active,
                    host_bracketed_paste_buffer,
                )?);
            }
            return Ok(actions);
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
            host_bracketed_paste_buffer.extend_from_slice(remaining);
            if host_bracketed_paste_buffer.len() > HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES {
                let buffered = std::mem::take(host_bracketed_paste_buffer);
                actions.push(TerminalClientLoopAction::ForwardToPane(buffered));
                break;
            }
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
        let Some(special_start) = earliest_sequence_start(
            earliest_sequence_start(paste_start, mouse_start),
            prefix_start,
        ) else {
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
            actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
            break;
        };
        let action = route_client_input(&remaining[..mouse_len], &config)?;
        apply_batched_mouse_action_side_effects(
            &mut config,
            &action,
            BatchedMouseSideEffectMode::BufferedPaste,
        );
        actions.push(action);
        remaining = &remaining[mouse_len..];
    }

    Ok(actions)
}

/// Selects compatibility behavior for batched mouse side-effect tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchedMouseSideEffectMode {
    /// Applies the full historical side-effect set used by the immediate
    /// forwarding router.
    ImmediateForwarding,
    /// Preserves the narrower historical side-effect set used by the paste
    /// buffering router.
    BufferedPaste,
}

/// Applies routing state transitions for mouse actions emitted from a batched
/// attached-terminal input scan.
fn apply_batched_mouse_action_side_effects(
    config: &mut TerminalClientLoopConfig,
    action: &TerminalClientLoopAction,
    _mode: BatchedMouseSideEffectMode,
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

/// Runs the earliest sequence start operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn earliest_sequence_start(first: Option<usize>, second: Option<usize>) -> Option<usize> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(first), None) => Some(first),
        (None, Some(second)) => Some(second),
        (None, None) => None,
    }
}

/// Runs the input sequence start operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn input_sequence_start(input: &[u8], sequence: &[u8]) -> Option<usize> {
    if sequence.is_empty() || sequence.len() > input.len() {
        return None;
    }
    input
        .windows(sequence.len())
        .position(|window| window == sequence)
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
    if !input.starts_with(prefix) {
        return Ok(None);
    }
    if input.len() == prefix.len() {
        return Ok(Some((route_client_input(input, config)?, prefix.len())));
    }
    let after_prefix = &input[prefix.len()..];
    let Some((_, second_len)) = parse_key_chord_bytes(after_prefix) else {
        return Ok(Some((route_client_input(prefix, config)?, prefix.len())));
    };
    let consumed = prefix.len().saturating_add(second_len);
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

/// Runs the application mouse forwarding bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn application_mouse_forwarding_bytes(
    event: MouseEvent,
    region: &super::MousePaneRegion,
) -> Option<Vec<u8>> {
    let local_column = event.column.checked_sub(region.column)?.saturating_add(1);
    let local_row = event.row.checked_sub(region.row)?.saturating_add(1);
    if region.application_sgr_mouse_mode {
        return Some(encode_sgr_mouse_event(event, local_column, local_row).into_bytes());
    }
    encode_legacy_xterm_mouse_event(event, local_column, local_row)
}

/// Runs the encode sgr mouse event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_sgr_mouse_event(event: MouseEvent, column: u16, row: u16) -> String {
    let code = mouse_event_code(event);
    let final_byte = if event.kind == super::MouseEventKind::Release {
        'm'
    } else {
        'M'
    };
    format!("\x1b[<{code};{column};{row}{final_byte}")
}

/// Runs the encode legacy xterm mouse event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_legacy_xterm_mouse_event(event: MouseEvent, column: u16, row: u16) -> Option<Vec<u8>> {
    let code = match event.kind {
        super::MouseEventKind::Release => 3u16.saturating_add(mouse_modifier_code(event)),
        _ => mouse_event_code(event),
    };
    let encoded_code = u8::try_from(code.saturating_add(32)).ok()?;
    let encoded_column = legacy_mouse_coordinate(column)?;
    let encoded_row = legacy_mouse_coordinate(row)?;
    Some(vec![
        b'\x1b',
        b'[',
        b'M',
        encoded_code,
        encoded_column,
        encoded_row,
    ])
}

/// Runs the legacy mouse coordinate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn legacy_mouse_coordinate(value: u16) -> Option<u8> {
    if value == 0 || value > 223 {
        return None;
    }
    u8::try_from(value.saturating_add(32)).ok()
}

/// Runs the mouse event code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mouse_event_code(event: MouseEvent) -> u16 {
    let button = match event.button {
        super::MouseButton::Left => 0,
        super::MouseButton::Middle => 1,
        super::MouseButton::Right => 2,
        super::MouseButton::WheelUp => 64,
        super::MouseButton::WheelDown => 65,
        super::MouseButton::Other(code) => code,
    };
    let drag = u16::from(matches!(event.kind, super::MouseEventKind::Drag)).saturating_mul(32);
    button
        .saturating_add(drag)
        .saturating_add(mouse_modifier_code(event))
}

/// Runs the mouse modifier code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mouse_modifier_code(event: MouseEvent) -> u16 {
    u16::from(event.modifiers.shift).saturating_mul(4)
        + u16::from(event.modifiers.alt).saturating_mul(8)
        + u16::from(event.modifiers.ctrl).saturating_mul(16)
}

/// Runs the sgr mouse sequence start operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sgr_mouse_sequence_start(input: &[u8]) -> Option<usize> {
    input.windows(3).position(|window| window == b"\x1b[<")
}

/// Runs the sgr mouse sequence len operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sgr_mouse_sequence_len(input: &[u8]) -> Option<usize> {
    if !input.starts_with(b"\x1b[<") {
        return None;
    }
    input
        .iter()
        .position(|byte| matches!(byte, b'M' | b'm'))
        .map(|index| index.saturating_add(1))
}

/// Runs the classify copy mode key action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn classify_copy_mode_key_action(input: &[u8]) -> Option<CopyModeKeyAction> {
    if input == b"\x1b" {
        return Some(CopyModeKeyAction::Cancel);
    }
    if input == b"\x03" {
        return Some(CopyModeKeyAction::Ignore);
    }
    let (chord, consumed) = parse_key_chord_bytes(input)?;
    if consumed != input.len() {
        return None;
    }
    match chord.code {
        KeyCode::Up if chord.modifiers.ctrl => Some(CopyModeKeyAction::MoveUpFast),
        KeyCode::Up => Some(CopyModeKeyAction::MoveUp),
        KeyCode::Down if chord.modifiers.ctrl => Some(CopyModeKeyAction::MoveDownFast),
        KeyCode::Down => Some(CopyModeKeyAction::MoveDown),
        KeyCode::Left if chord.modifiers.ctrl || chord.modifiers.alt => {
            Some(CopyModeKeyAction::MoveWordLeft)
        }
        KeyCode::Left => Some(CopyModeKeyAction::MoveLeft),
        KeyCode::Right if chord.modifiers.ctrl || chord.modifiers.alt => {
            Some(CopyModeKeyAction::MoveWordRight)
        }
        KeyCode::Right => Some(CopyModeKeyAction::MoveRight),
        KeyCode::PageUp => Some(CopyModeKeyAction::PageUp),
        KeyCode::PageDown => Some(CopyModeKeyAction::PageDown),
        KeyCode::Home if chord.modifiers.ctrl => Some(CopyModeKeyAction::Top),
        KeyCode::Home => Some(CopyModeKeyAction::LineStart),
        KeyCode::End if chord.modifiers.ctrl => Some(CopyModeKeyAction::Bottom),
        KeyCode::End => Some(CopyModeKeyAction::LineEnd),
        KeyCode::Char(' ') => Some(CopyModeKeyAction::BeginSelection),
        _ => None,
    }
}

/// Runs the application cursor forwarding bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn application_cursor_forwarding_bytes(
    input: &[u8],
    policy: MousePolicy,
) -> Option<Vec<u8>> {
    if !policy.pane_application_cursor_mode {
        return None;
    }
    match input {
        b"\x1b[A" => Some(b"\x1bOA".to_vec()),
        b"\x1b[B" => Some(b"\x1bOB".to_vec()),
        b"\x1b[C" => Some(b"\x1bOC".to_vec()),
        b"\x1b[D" => Some(b"\x1bOD".to_vec()),
        _ => None,
    }
}

/// Runs the plan attached terminal client step operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn plan_attached_terminal_client_step(
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    view: Option<&RenderedClientView>,
    status: Option<&ClientStatusLine>,
    config: &TerminalClientLoopConfig,
) -> Result<AttachedTerminalClientStepPlan> {
    let mut host_bracketed_paste_active = config.host_bracketed_paste_active;
    let mut host_bracketed_paste_buffer = config.host_bracketed_paste_buffer.clone();
    plan_attached_terminal_client_step_with_host_paste_buffer(
        readiness,
        input,
        view,
        status,
        config,
        &mut host_bracketed_paste_active,
        &mut host_bracketed_paste_buffer,
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
    host_bracketed_paste_active: &mut bool,
    host_bracketed_paste_buffer: &mut Vec<u8>,
) -> Result<AttachedTerminalClientStepPlan> {
    let input_readable = readiness
        .iter()
        .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable);
    let output_writable = readiness
        .iter()
        .any(|ready| ready.role == AttachedTerminalFdRole::Output && ready.writable);
    let input_hangup = readiness
        .iter()
        .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.hangup);
    let output_hangup = readiness
        .iter()
        .any(|ready| ready.role == AttachedTerminalFdRole::Output && ready.hangup);
    let error_roles = readiness
        .iter()
        .filter_map(|ready| ready.error.then_some(ready.role))
        .collect::<Vec<_>>();

    let mut actions = Vec::new();
    if input_readable
        && let Some(input) = input
        && !input.is_empty()
    {
        if view.is_some_and(|view| view.primary_prompt_active) {
            actions.push(TerminalClientLoopAction::ForwardToPane(input.to_vec()));
        } else {
            actions.extend(route_client_input_actions_with_host_paste_buffer(
                input,
                config,
                host_bracketed_paste_active,
                host_bracketed_paste_buffer,
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

    let (output_lines, output_line_style_spans) = if output_writable {
        view.map(|view| compose_client_presentation_with_styles(view, status))
            .unwrap_or_default()
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(AttachedTerminalClientStepPlan {
        actions,
        output_lines,
        output_line_style_spans,
        input_hangup,
        output_hangup,
        error_roles,
    })
}

/// Runs the run attached terminal client loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn run_attached_terminal_client_loop<I, V>(
    io: &mut I,
    mut view_provider: V,
    terminal_config: &TerminalClientLoopConfig,
    loop_config: AttachedTerminalClientLoopConfig,
) -> Result<AttachedTerminalClientLoopReport>
where
    I: AttachedTerminalClientLoopIo,
    V: FnMut() -> Result<Option<(RenderedClientView, Option<ClientStatusLine>)>>,
{
    if loop_config.max_iterations == 0 {
        return Err(MezError::invalid_args(
            "attached terminal client loop max_iterations must be greater than zero",
        ));
    }
    if loop_config.max_input_bytes == 0 {
        return Err(MezError::invalid_args(
            "attached terminal client loop max_input_bytes must be greater than zero",
        ));
    }

    let mut report = AttachedTerminalClientLoopReport {
        iterations: 0,
        actions: Vec::new(),
        output_frames: 0,
        bytes_written: 0,
        partial_writes: 0,
        pending_output_bytes: 0,
        input_hangups: 0,
        output_hangups: 0,
        error_roles: Vec::new(),
        host_bracketed_paste_active: terminal_config.host_bracketed_paste_active,
        host_bracketed_paste_buffer: terminal_config.host_bracketed_paste_buffer.clone(),
    };
    let cursor_blink_epoch = Instant::now();
    let mut host_bracketed_paste_active = terminal_config.host_bracketed_paste_active;
    let mut host_bracketed_paste_buffer = terminal_config.host_bracketed_paste_buffer.clone();

    for _ in 0..loop_config.max_iterations {
        let readiness = io.poll_readiness()?;
        let input = if readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable)
        {
            Some(io.read_input(loop_config.max_input_bytes)?)
        } else {
            None
        };
        let rendered = if readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Output && ready.writable)
        {
            view_provider()?
        } else {
            None
        };
        let (view, status) = match rendered.as_ref() {
            Some((view, status)) => (Some(view), status.as_ref()),
            None => (None, None),
        };
        let step = plan_attached_terminal_client_step_with_host_paste_buffer(
            &readiness,
            input.as_deref(),
            view,
            status,
            terminal_config,
            &mut host_bracketed_paste_active,
            &mut host_bracketed_paste_buffer,
        )?;
        report.host_bracketed_paste_active = host_bracketed_paste_active;
        report.host_bracketed_paste_buffer = host_bracketed_paste_buffer.clone();

        if !step.output_lines.is_empty() {
            let output_modes = AttachedTerminalOutputModes {
                application_keypad: terminal_config.mouse_policy.pane_application_keypad_mode,
                bracketed_paste: terminal_config.pane_bracketed_paste_mode,
                host_mouse_reporting: terminal_config.mouse_policy.enabled,
                cursor_style: terminal_config.cursor_style,
                cursor_blink: terminal_config.cursor_blink,
                cursor_blink_interval_ms: terminal_config.cursor_blink_interval_ms,
                cursor_blink_elapsed_ms: cursor_blink_elapsed_ms(cursor_blink_epoch),
                animation_refresh_interval_ms: view
                    .map(|view| view.animation_refresh_interval_ms)
                    .unwrap_or(0),
                cursor_visible: view.is_some_and(|view| view.cursor_visible),
                cursor_row: view.map(|view| view.cursor_row).unwrap_or(0),
                cursor_column: view.map(|view| view.cursor_column).unwrap_or(0),
            };
            report.bytes_written =
                report
                    .bytes_written
                    .saturating_add(io.write_styled_output_with_modes(
                        &step.output_lines,
                        &step.output_line_style_spans,
                        output_modes,
                    )?);
            report.output_frames = report.output_frames.saturating_add(1);
        }
        report.actions.extend(step.actions);
        if step.input_hangup {
            report.input_hangups = report.input_hangups.saturating_add(1);
        }
        if step.output_hangup {
            report.output_hangups = report.output_hangups.saturating_add(1);
        }
        report.error_roles.extend(step.error_roles);
        report.iterations = report.iterations.saturating_add(1);

        if report.input_hangups > 0 || report.output_hangups > 0 || !report.error_roles.is_empty() {
            break;
        }
    }

    Ok(report)
}
