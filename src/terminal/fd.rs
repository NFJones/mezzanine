//! Terminal Fd implementation.
//!
//! This module owns the terminal fd boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    BTreeMap, BorrowedFd, CopyPosition, Errno, KeyBindings, KeyChord, MezError, MouseBorderCell,
    MousePaneAgentSelectorCell, MousePaneAgentStatusCell, MousePaneRegion, MousePolicy,
    MouseWindowActionFrameCell, MouseWindowFrameCell, MouseWindowGroupFrameCell, OptionalActions,
    RawFd, Result, Size, Termios, UiTheme, WindowFrameAction, borrow_raw_fd, fcntl_getfl,
    tcgetattr, tcgetwinsize, tcsetattr,
};
use crate::readline::ReadlinePrompt;
#[cfg(test)]
use rustix::event::{PollFd as RustixPollFd, PollFlags, Timespec, poll as rustix_poll};
#[cfg(test)]
use std::time::{Duration, Instant};

// Raw mode and attached terminal file-descriptor helpers.

/// Carries Pane Render Input state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRenderInput {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub lines: Vec<String>,
}

/// Runtime metadata made available to terminal frame template rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalFrameContext {
    /// Stable session identity shown by `session.id`.
    pub session_id: Option<String>,
    /// Active approval or permission mode shown by `policy.mode`.
    pub policy_mode: Option<String>,
    /// Number of pending observer attach requests shown by `observer.pending_count`.
    pub pending_observer_count: usize,
    /// Running-agent counts keyed by stable window id for `agent.active_count`.
    pub window_agent_active_counts: BTreeMap<String, usize>,
    /// Unread local message counts keyed by stable window id for `message.unread_count`.
    pub window_unread_message_counts: BTreeMap<String, usize>,
    /// Ordered window summaries used by the default all-window frame header.
    pub windows: Vec<TerminalWindowFrameContext>,
    /// Ordered group summaries used by the conditional top group bar.
    pub groups: Vec<TerminalWindowGroupFrameContext>,
    /// Pane status-bar action currently held by the mouse, if any.
    pub pressed_window_action: Option<WindowFrameAction>,
    /// Monotonic-ish wall-clock tick used by animated frame elements.
    pub animation_tick_ms: u64,
    /// Whether optional frame/status animations should render as static UI.
    pub reduced_motion: bool,
    /// Right-side status fields rendered into the active pane frame.
    pub window_status: Option<TerminalWindowStatusContext>,
    /// Per-pane runtime metadata keyed by stable pane id.
    pub panes: BTreeMap<String, TerminalPaneFrameContext>,
}

/// Runtime fields available to the right side of the window status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowStatusContext {
    /// Named-field template for the right status line.
    pub template: String,
    /// Home-relative active pane working directory shown by `pane.pwd`.
    pub active_pane_working_directory: Option<String>,
    /// Human-readable system uptime shown by `system.uptime`.
    pub system_uptime: String,
    /// Human-readable local datetime shown by `datetime.local`.
    pub datetime_local: String,
}

/// Runtime window metadata made available to default window-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowFrameContext {
    /// Stable window identity.
    pub id: String,
    /// Display index in the session window list.
    pub index: usize,
    /// User-facing title or name for the window.
    pub title: String,
    /// Whether this window is currently focused.
    pub active: bool,
    /// Whether this window is dedicated to spawned subagent panes.
    pub subagent: bool,
}

/// Runtime window-group metadata made available to group-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowGroupFrameContext {
    /// Stable group identity.
    pub id: String,
    /// Display index in the session group list.
    pub index: usize,
    /// User-facing title or name for the group.
    pub title: String,
    /// Whether this group is currently focused.
    pub active: bool,
}

/// Runtime metadata made available to pane frame template rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalPaneFrameContext {
    /// Primary process id shown by `pane.primary_pid`.
    pub primary_pid: Option<u32>,
    /// Primary process name shown by `pane.process_name` when known.
    pub process_name: Option<String>,
    /// Primary process exit status shown by `pane.exit_status` when known.
    pub exit_status: Option<String>,
    /// Home-relative current working directory shown by `pane.pwd` when known.
    pub current_working_directory: Option<String>,
    /// Current pane interaction mode shown by `pane.mode`.
    pub mode: Option<String>,
    /// Agent identity associated with the pane, shown by `agent.id`.
    pub agent_id: Option<String>,
    /// Human-readable agent display name shown by `agent.name`.
    pub agent_name: Option<String>,
    /// Agent state shown by `agent.status`.
    pub agent_status: Option<String>,
    /// Active provider model name shown by `agent.model`.
    pub agent_model: Option<String>,
    /// Active reasoning profile or effort shown by `agent.reasoning`.
    pub agent_reasoning: Option<String>,
    /// Pane-local routing state shown by `agent.routing`.
    pub agent_routing: Option<String>,
    /// Active latency preference shown by `agent.latency`.
    pub agent_latency: Option<String>,
    /// Active model preset name shown by `agent.preset`.
    pub agent_preset: Option<String>,
    /// Last known provider input context usage shown by `agent.context_usage`.
    pub agent_context_usage: Option<String>,
    /// Scrollback position shown by `history.position` when not at the live bottom.
    pub history_position: Option<String>,
    /// Current pane-local agent prompt buffer when the pane is in agent mode.
    pub agent_prompt: Option<ReadlinePrompt>,
    /// Pane-local agent progress and response lines shown above the prompt.
    pub agent_display_lines: Vec<String>,
}

/// Placement of a one-row terminal frame within its owning region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFramePosition {
    /// Render the frame before region body content.
    #[default]
    Top,
    /// Render the frame after region body content.
    Bottom,
}

/// Style applied to a rendered frame row when styled terminal output is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFrameStyle {
    /// Leave frame text unstyled.
    #[default]
    Default,
    /// Render the frame with bold/intense text.
    Bold,
    /// Render the frame with underline text.
    Underline,
    /// Render the frame with inverse video.
    Inverse,
}

/// Cursor shape used when Mezzanine presents the active interactive surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalCursorStyle {
    /// A full-cell block cursor.
    #[default]
    Block,
    /// An underline cursor.
    Underline,
    /// A vertical bar cursor.
    Bar,
}

impl TerminalCursorStyle {
    /// Returns the DECSCUSR parameter for this shape and blink behavior.
    pub const fn decscusr_parameter(self, blink: bool) -> u8 {
        match (self, blink) {
            (Self::Block, true) => 1,
            (Self::Block, false) => 2,
            (Self::Underline, true) => 3,
            (Self::Underline, false) => 4,
            (Self::Bar, true) => 5,
            (Self::Bar, false) => 6,
        }
    }
}

/// Carries Terminal Client Loop Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalClientLoopConfig {
    /// Stores the bindings value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bindings: KeyBindings,
    /// Stores the command bindings value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command_bindings: BTreeMap<KeyChord, String>,
    /// Whether the next key should be interpreted through the prefix table.
    ///
    /// A lone configured escape key enters this transient state. The next key
    /// consumes the state and is matched against prefix bindings instead of
    /// direct accelerators or pane forwarding.
    pub prefix_key_pending: bool,
    /// Stores the mouse policy value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_policy: MousePolicy,
    /// Stores the mouse selection active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_selection_active: bool,
    /// Stores the mouse selection autoscroll position value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_selection_autoscroll_position: Option<CopyPosition>,
    /// Whether a full-window command display overlay currently owns primary
    /// mouse input.
    ///
    /// When set, mouse clicks and wheel events are routed to the overlay before
    /// pane, frame, or application hit testing.
    pub primary_display_overlay_active: bool,
    /// Stores the mouse border cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_border_cells: Vec<MouseBorderCell>,
    /// Stores the mouse window frame cells value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_window_frame_cells: Vec<MouseWindowFrameCell>,
    /// Stores the mouse window action frame cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_window_action_frame_cells: Vec<MouseWindowActionFrameCell>,
    /// Stores the mouse window group frame cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_window_group_frame_cells: Vec<MouseWindowGroupFrameCell>,
    /// Stores the mouse pane agent status cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_pane_agent_status_cells: Vec<MousePaneAgentStatusCell>,
    /// Stores the mouse pane agent selector cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_pane_agent_selector_cells: Vec<MousePaneAgentSelectorCell>,
    /// Stores the mouse pane regions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_pane_regions: Vec<MousePaneRegion>,
    /// Stores the frame context value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub frame_context: TerminalFrameContext,
    /// Stores the window frames enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frames_enabled: bool,
    /// Stores the window frame template value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frame_template: String,
    /// Stores the window frame position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frame_position: TerminalFramePosition,
    /// Stores the window frame style value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frame_style: TerminalFrameStyle,
    /// Stores the window frame visible fields value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frame_visible_fields: Vec<String>,
    /// Stores the pane frames enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frames_enabled: bool,
    /// Stores the pane frame template value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frame_template: String,
    /// Stores the pane frame position value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frame_position: TerminalFramePosition,
    /// Stores the pane frame style value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frame_style: TerminalFrameStyle,
    /// Stores the pane frame visible fields value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frame_visible_fields: Vec<String>,
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
    /// Whether the active pane application has requested bracketed paste.
    ///
    /// Attached terminals mirror this state into the host terminal and use the
    /// paired host paste delimiters to forward pasted payloads without treating
    /// their bytes as Mezzanine key bindings.
    pub pane_bracketed_paste_mode: bool,
    /// Whether the attached terminal is currently inside a host bracketed paste
    /// payload whose closing delimiter has not yet arrived.
    ///
    /// This state is carried between terminal-read batches so large clipboard
    /// pastes cannot expose middle chunks to mux-prefix or mouse parsing.
    pub host_bracketed_paste_active: bool,
    /// Buffered host bracketed-paste bytes whose closing delimiter has not yet
    /// arrived.
    ///
    /// The buffer is carried with `host_bracketed_paste_active` so large
    /// terminal pastes are forwarded to the pane as one ordered payload instead
    /// of trickling partial heredoc contents ahead of the paste terminator.
    pub host_bracketed_paste_buffer: Vec<u8>,
    /// Stores the resize debounce ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub resize_debounce_ms: u64,
    /// Maximum foreground render frames per second during bursty invalidations.
    ///
    /// A value of zero disables rate limiting. Nonzero values coalesce repeated
    /// render invalidations while preserving one trailing frame after a burst.
    pub render_rate_limit_fps: u64,
    /// Stores the ui theme value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ui_theme: UiTheme,
}

impl Default for TerminalClientLoopConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            bindings: KeyBindings::default(),
            command_bindings: BTreeMap::new(),
            prefix_key_pending: false,
            mouse_policy: MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
            mouse_selection_active: false,
            mouse_selection_autoscroll_position: None,
            primary_display_overlay_active: false,
            mouse_border_cells: Vec::new(),
            mouse_window_frame_cells: Vec::new(),
            mouse_window_action_frame_cells: Vec::new(),
            mouse_window_group_frame_cells: Vec::new(),
            mouse_pane_agent_status_cells: Vec::new(),
            mouse_pane_agent_selector_cells: Vec::new(),
            mouse_pane_regions: Vec::new(),
            frame_context: TerminalFrameContext::default(),
            window_frames_enabled: true,
            window_frame_template: super::render::DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
            window_frame_position: TerminalFramePosition::Bottom,
            window_frame_style: TerminalFrameStyle::Default,
            window_frame_visible_fields: super::render::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            pane_frames_enabled: true,
            pane_frame_template: super::render::DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            pane_frame_position: TerminalFramePosition::Top,
            pane_frame_style: TerminalFrameStyle::Default,
            pane_frame_visible_fields: super::render::DEFAULT_PANE_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            cursor_style: TerminalCursorStyle::Block,
            cursor_blink: false,
            cursor_blink_interval_ms: 500,
            pane_bracketed_paste_mode: false,
            host_bracketed_paste_active: false,
            host_bracketed_paste_buffer: Vec::new(),
            resize_debounce_ms: 200,
            render_rate_limit_fps: 5,
            ui_theme: UiTheme::default(),
        }
    }
}

/// Role of an attached-terminal file descriptor in the future client loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedTerminalFdRole {
    /// Represents the Input case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Input,
    /// Represents the Output case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Output,
    /// Represents the Control case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Control,
}

impl AttachedTerminalFdRole {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::Control => "control",
        }
    }
}

/// Read and write readiness interests for an attached-terminal file descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalFdInterest {
    /// Stores the read value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub read: bool,
    /// Stores the write value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub write: bool,
}

impl TerminalFdInterest {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn read() -> Self {
        Self {
            read: true,
            write: false,
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn write() -> Self {
        Self {
            read: false,
            write: true,
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn read_write() -> Self {
        Self {
            read: true,
            write: true,
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn is_empty(self) -> bool {
        !self.read && !self.write
    }
}

/// Restores a terminal file descriptor after temporarily enabling raw mode.
#[derive(Debug)]
pub struct TerminalRawModeGuard {
    /// Stores the fd value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) fd: RawFd,
    /// Stores the original value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) original: Option<Termios>,
}

impl TerminalRawModeGuard {
    /// Runs the enable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enable(fd: RawFd) -> Result<Self> {
        if fd < 0 {
            return Err(MezError::invalid_args(
                "terminal raw mode file descriptor is invalid",
            ));
        }
        let borrowed = borrow_raw_terminal_fd(fd);
        let original = tcgetattr(borrowed).map_err(raw_mode_io_error)?;
        let mut raw = original.clone();
        raw.make_raw();
        tcsetattr(borrowed, OptionalActions::Now, &raw).map_err(raw_mode_io_error)?;
        Ok(Self {
            fd,
            original: Some(original),
        })
    }

    /// Runs the restore operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn restore(&mut self) -> Result<()> {
        let Some(original) = self.original.take() else {
            return Ok(());
        };
        tcsetattr(
            borrow_raw_terminal_fd(self.fd),
            OptionalActions::Now,
            &original,
        )
        .map_err(raw_mode_io_error)
    }
}

impl Drop for TerminalRawModeGuard {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// A validated descriptor that the attached-terminal client loop may poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedTerminalFd {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) role: AttachedTerminalFdRole,
    /// Stores the fd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) fd: RawFd,
    /// Stores the interest value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) interest: TerminalFdInterest,
}

impl AttachedTerminalFd {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        role: AttachedTerminalFdRole,
        fd: RawFd,
        interest: TerminalFdInterest,
    ) -> Result<Self> {
        validate_attached_terminal_fd(role, fd, interest)?;
        Ok(Self { role, fd, interest })
    }

    /// Runs the input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn input(fd: RawFd, interest: TerminalFdInterest) -> Result<Self> {
        Self::new(AttachedTerminalFdRole::Input, fd, interest)
    }

    /// Runs the output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn output(fd: RawFd, interest: TerminalFdInterest) -> Result<Self> {
        Self::new(AttachedTerminalFdRole::Output, fd, interest)
    }

    /// Runs the control operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn control(fd: RawFd, interest: TerminalFdInterest) -> Result<Self> {
        Self::new(AttachedTerminalFdRole::Control, fd, interest)
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn role(self) -> AttachedTerminalFdRole {
        self.role
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn raw_fd(self) -> RawFd {
        self.fd
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn interest(self) -> TerminalFdInterest {
        self.interest
    }
}

/// Readiness flags returned for one attached-terminal file descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedTerminalFdReadiness {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: AttachedTerminalFdRole,
    /// Stores the fd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub fd: RawFd,
    /// Stores the interest value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interest: TerminalFdInterest,
    /// Stores the readable value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub readable: bool,
    /// Stores the writable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writable: bool,
    /// Stores the hangup value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hangup: bool,
    /// Stores the error value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub error: bool,
}

impl AttachedTerminalFdReadiness {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn is_ready(self) -> bool {
        self.readable || self.writable || self.hangup || self.error
    }
}

/// Poll attached-terminal descriptors for readiness.
///
/// A `None` timeout waits indefinitely. A zero duration performs a nonblocking
/// readiness check. The returned vector preserves descriptor order.
#[cfg(test)]
pub fn poll_attached_terminal_fd_readiness(
    descriptors: &[AttachedTerminalFd],
    timeout: Option<Duration>,
) -> Result<Vec<AttachedTerminalFdReadiness>> {
    let mut poll_fds = descriptors
        .iter()
        .map(|descriptor| {
            validate_attached_terminal_fd(descriptor.role, descriptor.fd, descriptor.interest)?;
            Ok(RustixPollFd::from_borrowed_fd(
                borrow_raw_attached_terminal_fd(*descriptor),
                poll_events_for_interest(descriptor.interest),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    poll_posix(&mut poll_fds, timeout)?;

    descriptors
        .iter()
        .zip(poll_fds.iter())
        .map(|(descriptor, poll_fd)| readiness_from_poll_fd(*descriptor, poll_fd))
        .collect()
}

/// Reads the current terminal size from a terminal file descriptor.
pub fn read_attached_terminal_size(fd: RawFd) -> Result<Option<Size>> {
    validate_terminal_size_fd(fd)?;
    match tcgetwinsize(borrow_raw_terminal_fd(fd)) {
        Ok(size) if size.ws_col > 0 && size.ws_row > 0 => {
            Ok(Some(Size::new(size.ws_col, size.ws_row)?))
        }
        Ok(_) => Ok(None),
        Err(Errno::NOTTY) => Ok(None),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

/// Runs the validate terminal size fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_terminal_size_fd(fd: RawFd) -> Result<()> {
    if fd < 0 {
        return Err(MezError::invalid_args(
            "attached terminal size file descriptor is invalid",
        ));
    }
    match fcntl_getfl(borrow_raw_fd(fd)) {
        Ok(_) => Ok(()),
        Err(Errno::BADF) => Err(MezError::invalid_args(
            "attached terminal size file descriptor is invalid",
        )),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

/// Runs the validate attached terminal fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_attached_terminal_fd(
    role: AttachedTerminalFdRole,
    fd: RawFd,
    interest: TerminalFdInterest,
) -> Result<()> {
    if fd < 0 {
        return Err(invalid_attached_terminal_fd_error(role));
    }
    match fcntl_getfl(borrow_raw_fd(fd)) {
        Ok(_) => {}
        Err(Errno::BADF) => return Err(invalid_attached_terminal_fd_error(role)),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    if interest.is_empty() {
        return Err(MezError::invalid_args(format!(
            "attached terminal {} fd interest must include read or write",
            role.as_str()
        )));
    }
    Ok(())
}

/// Runs the invalid attached terminal fd error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn invalid_attached_terminal_fd_error(role: AttachedTerminalFdRole) -> MezError {
    MezError::invalid_args(format!(
        "attached terminal {} file descriptor is invalid",
        role.as_str()
    ))
}

/// Runs the raw mode io error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn raw_mode_io_error(error: rustix::io::Errno) -> MezError {
    MezError::new(
        crate::error::MezErrorKind::Io,
        format!("terminal raw mode failed: {error}"),
    )
}

/// Runs the borrow raw terminal fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn borrow_raw_terminal_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: Callers validate that the raw fd is non-negative and keep using
    // process-owned standard terminal descriptors for the duration of the call.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Runs the borrow raw attached terminal fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn borrow_raw_attached_terminal_fd(
    descriptor: AttachedTerminalFd,
) -> BorrowedFd<'static> {
    // SAFETY: `AttachedTerminalFd` values are validated before construction and
    // are only borrowed for the duration of one immediate rustix syscall.
    unsafe { BorrowedFd::borrow_raw(descriptor.fd) }
}

/// Runs the poll events for interest operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn poll_events_for_interest(interest: TerminalFdInterest) -> PollFlags {
    let mut events = PollFlags::empty();
    if interest.read {
        events |= PollFlags::IN;
    }
    if interest.write {
        events |= PollFlags::OUT;
    }
    events
}

/// Runs the readiness from poll fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn readiness_from_poll_fd(
    descriptor: AttachedTerminalFd,
    poll_fd: &RustixPollFd<'_>,
) -> Result<AttachedTerminalFdReadiness> {
    let revents = poll_fd.revents();
    if revents.contains(PollFlags::NVAL) {
        return Err(invalid_attached_terminal_fd_error(descriptor.role));
    }
    Ok(AttachedTerminalFdReadiness {
        role: descriptor.role,
        fd: descriptor.fd,
        interest: descriptor.interest,
        readable: revents.contains(PollFlags::IN),
        writable: revents.contains(PollFlags::OUT),
        hangup: revents.contains(PollFlags::HUP),
        error: revents.contains(PollFlags::ERR),
    })
}

/// Runs the poll posix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn poll_posix(fds: &mut [RustixPollFd<'_>], timeout: Option<Duration>) -> Result<()> {
    if fds.is_empty() {
        return Ok(());
    }

    let started = Instant::now();
    let mut remaining = timeout;
    loop {
        let timeout_spec = remaining.map(duration_to_timespec).transpose()?;
        match rustix_poll(fds, timeout_spec.as_ref()) {
            Ok(_) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }

        if let Some(timeout) = timeout {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(());
            }
            remaining = Some(timeout - elapsed);
        }
    }
}

/// Runs the duration to timespec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn duration_to_timespec(duration: Duration) -> Result<Timespec> {
    Timespec::try_from(duration)
        .map_err(|_| MezError::invalid_args("attached terminal poll timeout is too large"))
}
