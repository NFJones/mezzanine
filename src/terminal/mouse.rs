//! Terminal Mouse implementation.
//!
//! This module owns the terminal mouse boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{CopyPosition, Result};

// Mouse event parsing and policy classification.

/// Carries Mouse Button state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Represents the Left case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Left,
    /// Represents the Middle case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Middle,
    /// Represents the Right case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Right,
    /// Represents the Wheel Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    WheelUp,
    /// Represents the Wheel Down case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    WheelDown,
    /// Represents the Other case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Other(u16),
}

/// Carries Mouse Event Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    /// Represents the Press case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Press,
    /// Represents the Release case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Release,
    /// Represents the Drag case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Drag,
    /// Represents the Scroll case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Scroll,
}

/// Carries Mouse Modifiers state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseModifiers {
    /// Stores the shift value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shift: bool,
    /// Stores the alt value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub alt: bool,
    /// Stores the ctrl value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ctrl: bool,
}

/// Carries Mouse Event state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: MouseEventKind,
    /// Stores the button value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub button: MouseButton,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub column: u16,
    /// Stores the row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub row: u16,
    /// Stores the modifiers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modifiers: MouseModifiers,
}

/// Carries Mouse Policy state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MousePolicy {
    /// Stores the enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enabled: bool,
    /// Stores the pane application mouse mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_application_mouse_mode: bool,
    /// Stores the pane sgr mouse mode value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_sgr_mouse_mode: bool,
    /// Stores the pane application cursor mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_application_cursor_mode: bool,
    /// Stores the pane application keypad mode value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_application_keypad_mode: bool,
    /// Stores the pane resize active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_resize_active: bool,
    /// Stores the over pane border value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub over_pane_border: bool,
    /// Stores the over window frame value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub over_window_frame: bool,
    /// Stores the copy mode active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub copy_mode_active: bool,
}

/// A zero-based terminal cell occupied by a mux-managed pane border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseBorderCell {
    /// The zero-based rendered terminal column for the border cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the border cell.
    pub row: u16,
}

/// A zero-based terminal cell occupied by a rendered window-frame pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseWindowFrameCell {
    /// The zero-based rendered terminal column for the frame cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the frame cell.
    pub row: u16,
    /// The session window index targeted by this frame cell.
    pub window_index: usize,
}

/// Kind of command executed by a window status-bar action button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WindowFrameCommandKind {
    /// Run the command through the terminal `:` command dispatcher.
    Terminal,
    /// Run the command through the focused pane's agent shell dispatcher.
    Agent,
}

/// Built-in and user-templated actions rendered in the window status bar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum WindowFrameAction {
    /// Split the active window by creating a new pane beside the active pane.
    NewPane,
    /// Create a new shell window in the active group.
    NewWindow,
    /// Create a new window group with a landing shell window.
    NewGroup,
    /// Show or hide the focused pane's agent shell.
    AgentShell,
    /// Toggle automatic model/reasoning sizing for the focused pane's agent.
    Routing,
    /// Run an arbitrary terminal command from a templated status button.
    TerminalCommand {
        /// Stable identity for press/release matching.
        id: String,
        /// Single-cell icon rendered inside the button pill.
        icon: String,
        /// Terminal `:` command text to execute.
        command: String,
    },
    /// Run an arbitrary agent slash command from a templated status button.
    AgentCommand {
        /// Stable identity for press/release matching.
        id: String,
        /// Single-cell icon rendered inside the button pill.
        icon: String,
        /// Agent slash command text to execute.
        command: String,
    },
}

impl WindowFrameAction {
    /// Builds a templated terminal-command status button.
    ///
    /// # Parameters
    /// - `icon`: The icon rendered inside the pill.
    /// - `command`: The terminal command run when the button is released.
    pub fn terminal_button(icon: impl Into<String>, command: impl Into<String>) -> Self {
        let icon = icon.into();
        let command = command.into();
        Self::TerminalCommand {
            id: format!("terminal:{icon}:{command}"),
            icon,
            command,
        }
    }

    /// Builds a templated agent-command status button.
    ///
    /// # Parameters
    /// - `icon`: The icon rendered inside the pill.
    /// - `command`: The agent slash command run when the button is released.
    pub fn agent_button(icon: impl Into<String>, command: impl Into<String>) -> Self {
        let icon = icon.into();
        let command = command.into();
        Self::AgentCommand {
            id: format!("agent:{icon}:{command}"),
            icon,
            command,
        }
    }

    /// Returns the single-cell icon rendered for this status-bar action.
    pub fn icon(&self) -> &str {
        match self {
            Self::NewPane => "+",
            Self::NewWindow => "□",
            Self::NewGroup => "⊕",
            Self::AgentShell => "λ",
            Self::Routing => "Δ",
            Self::TerminalCommand { icon, .. } | Self::AgentCommand { icon, .. } => icon,
        }
    }

    /// Returns the stable action name used in render metadata and tests.
    pub fn name(&self) -> &str {
        match self {
            Self::NewPane => "new-pane",
            Self::NewWindow => "new-window",
            Self::NewGroup => "new-group",
            Self::AgentShell => "agent-shell",
            Self::Routing => "routing",
            Self::TerminalCommand { id, .. } | Self::AgentCommand { id, .. } => id,
        }
    }

    /// Returns the command dispatcher kind for this status-bar action.
    pub const fn command_kind(&self) -> WindowFrameCommandKind {
        match self {
            Self::Routing | Self::AgentCommand { .. } => WindowFrameCommandKind::Agent,
            Self::NewPane
            | Self::NewWindow
            | Self::NewGroup
            | Self::AgentShell
            | Self::TerminalCommand { .. } => WindowFrameCommandKind::Terminal,
        }
    }

    /// Returns the command text executed by this status-bar action.
    pub fn command(&self) -> &str {
        match self {
            Self::NewPane => "split-window",
            Self::NewWindow => "new-window",
            Self::NewGroup => "new-group",
            Self::AgentShell => "agent-shell",
            Self::Routing => "/routing toggle",
            Self::TerminalCommand { command, .. } | Self::AgentCommand { command, .. } => command,
        }
    }

    /// Returns all default window status-bar actions in display order.
    pub fn all() -> Vec<Self> {
        vec![
            Self::NewPane,
            Self::NewWindow,
            Self::NewGroup,
            Self::AgentShell,
            Self::Routing,
        ]
    }
}

/// A zero-based terminal cell occupied by a rendered window status action pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseWindowActionFrameCell {
    /// The zero-based rendered terminal column for the action cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the action cell.
    pub row: u16,
    /// The built-in action targeted by this frame cell.
    pub action: WindowFrameAction,
}

/// A zero-based terminal cell occupied by a rendered window-group pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseWindowGroupFrameCell {
    /// The zero-based rendered terminal column for the frame cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the frame cell.
    pub row: u16,
    /// The session group index targeted by this frame cell.
    pub group_index: usize,
}

/// Clickable pane-frame agent status fields that expose selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAgentStatusField {
    /// Active provider model shown in the pane-frame status pills.
    Model,
    /// Active reasoning profile or effort shown in the pane-frame status pills.
    Reasoning,
    /// Provider thinking-mode state shown in the pane-frame status pills.
    Thinking,
    /// Pane-local routing state shown in the pane-frame status pills.
    Routing,
    /// Active approval mode shown in the pane-frame status pills.
    ApprovalPolicy,
    /// Active latency preference shown in the pane-frame status pills.
    Latency,
    /// Active model preset shown in the pane-frame status pills.
    Preset,
}

impl PaneAgentStatusField {
    /// Returns the pane-frame template field associated with this selector.
    pub const fn frame_field(self) -> &'static str {
        match self {
            Self::Model => "agent.model",
            Self::Reasoning => "agent.reasoning",
            Self::Thinking => "agent.thinking",
            Self::Routing => "agent.routing",
            Self::ApprovalPolicy => "policy.mode",
            Self::Latency => "agent.latency",
            Self::Preset => "agent.preset",
        }
    }
}

/// A zero-based terminal cell occupied by a selectable pane agent status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MousePaneAgentStatusCell {
    /// The zero-based rendered terminal column for the status pill cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the status pill cell.
    pub row: u16,
    /// The pane index targeted by this status pill cell.
    pub pane_index: usize,
    /// The selectable agent status field represented by this cell.
    pub field: PaneAgentStatusField,
}

/// A zero-based terminal cell occupied by an open pane agent selector item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MousePaneAgentSelectorCell {
    /// The zero-based rendered terminal column for the selector item cell.
    pub column: u16,
    /// The zero-based rendered terminal row for the selector item cell.
    pub row: u16,
    /// The pane index targeted by this selector cell.
    pub pane_index: usize,
    /// The selectable agent status field represented by this selector.
    pub field: PaneAgentStatusField,
    /// The zero-based selector item index represented by this cell.
    pub item_index: usize,
}

/// A rendered pane content rectangle with pane-local mouse ownership state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MousePaneRegion {
    /// Stable pane identity for targeted mouse forwarding.
    pub pane_id: String,
    /// The zero-based rendered terminal column where pane content starts.
    pub column: u16,
    /// The zero-based rendered terminal row where pane content starts.
    pub row: u16,
    /// The rendered pane content width in terminal cells.
    pub columns: u16,
    /// The rendered pane content height in terminal cells.
    pub rows: u16,
    /// Whether the pane application has enabled SGR mouse reporting.
    pub application_sgr_mouse_mode: bool,
    /// Whether the pane application has enabled any tracked mouse reporting.
    pub application_mouse_mode: bool,
    /// Whether this pane currently owns copy-mode mouse handling.
    pub copy_mode_active: bool,
    /// Whether this pane is currently focused.
    pub active: bool,
}

impl MousePaneRegion {
    /// Returns whether a rendered terminal cell lies inside this pane's content.
    pub fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.column
            && column < self.column.saturating_add(self.columns)
            && row >= self.row
            && row < self.row.saturating_add(self.rows)
    }
}

/// Carries Mouse Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    /// Represents the Ignore case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ignore,
    /// Represents the Forward To Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ForwardToPane,
    /// Represents the Focus Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusWindow {
        /// Stores the index value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        index: usize,
    },
    /// Represents the Focus Group case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusGroup {
        /// Stores the index value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        index: usize,
    },
    /// Marks a window status-bar action as pressed.
    PressWindowAction {
        /// The built-in action represented by the pressed pill.
        action: WindowFrameAction,
    },
    /// Runs a window status-bar action after release on the pressed pill.
    ReleaseWindowAction {
        /// The built-in action represented by the released pill.
        action: WindowFrameAction,
    },
    /// Clears a pressed window status-bar action without running it.
    CancelWindowAction,
    /// Opens a pane agent status selector for model or reasoning changes.
    OpenPaneAgentStatusSelector {
        /// Pane index targeted by the selector.
        pane_index: usize,
        /// Agent status field to select.
        field: PaneAgentStatusField,
    },
    /// Updates the highlighted pane agent selector item during mouse movement.
    HoverPaneAgentStatusSelector {
        /// Pane index targeted by the selector.
        pane_index: usize,
        /// Agent status field currently being selected.
        field: PaneAgentStatusField,
        /// Zero-based selector item index under the pointer.
        item_index: usize,
    },
    /// Applies a pane agent selector item selected by mouse release.
    SelectPaneAgentStatusSelector {
        /// Pane index targeted by the selector.
        pane_index: usize,
        /// Agent status field currently being selected.
        field: PaneAgentStatusField,
        /// Zero-based selector item index selected by the pointer.
        item_index: usize,
    },
    /// Scrolls an open pane agent selector without affecting pane scrollback.
    ScrollPaneAgentStatusSelector {
        /// Pane index targeted by the selector.
        pane_index: usize,
        /// Agent status field currently being selected.
        field: PaneAgentStatusField,
        /// Signed number of rows to move the selector viewport.
        lines: isize,
    },
    /// Closes an open pane agent selector without applying a value.
    ClosePaneAgentStatusSelector,
    /// Selects a line in the primary command-output overlay.
    SelectDisplayOverlay {
        /// Zero-based terminal position clicked by the user.
        position: CopyPosition,
    },
    /// Scrolls the primary command-output overlay with the mouse wheel.
    ScrollDisplayOverlay {
        /// Signed number of rows to move the overlay viewport.
        lines: isize,
    },
    /// Represents the Focus Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusPane(CopyPosition),
    /// Represents the Focus Pane Only case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusPaneOnly(CopyPosition),
    /// Represents the Paste Clipboard case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PasteClipboard(CopyPosition),
    /// Represents the Show Window Chooser case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShowWindowChooser {
        /// Stores the column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        column: u16,
        /// Stores the row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        row: u16,
    },
    /// Represents the Resize Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ResizePane {
        /// Stores the column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        column: u16,
        /// Stores the row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        row: u16,
    },
    /// Represents the Finish Resize Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FinishResizePane,
    /// Represents the Copy Selection Start case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CopySelectionStart(CopyPosition),
    /// Represents the Copy Selection Update case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CopySelectionUpdate(CopyPosition),
    /// Represents the Copy Selection Finish case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CopySelectionFinish(CopyPosition),
    /// Represents the Scroll History case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ScrollHistory {
        /// Stores the lines value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        lines: isize,
        /// Stores the position value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        position: CopyPosition,
    },
}

/// Carries Copy Mode Key Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeKeyAction {
    /// Represents the Move Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveUp,
    /// Moves the copy-mode cursor up by five lines.
    MoveUpFast,
    /// Represents the Move Down case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveDown,
    /// Moves the copy-mode cursor down by five lines.
    MoveDownFast,
    /// Represents the Move Left case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveLeft,
    /// Moves the copy-mode cursor left by one word-like segment.
    MoveWordLeft,
    /// Represents the Move Right case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveRight,
    /// Moves the copy-mode cursor right by one word-like segment.
    MoveWordRight,
    /// Represents the Page Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PageUp,
    /// Represents the Page Down case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PageDown,
    /// Represents the Top case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Top,
    /// Moves the copy-mode cursor to the beginning of the current line.
    LineStart,
    /// Represents the Bottom case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Bottom,
    /// Moves the copy-mode cursor to the end of the current line.
    LineEnd,
    /// Represents the Begin Selection case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BeginSelection,
    /// Represents an unbound key consumed while copy mode is active.
    ///
    /// Copy mode owns keyboard input until it exits, so unrecognized keys are
    /// intentionally ignored instead of being forwarded into the pane process.
    Ignore,
    /// Represents the Cancel case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Cancel,
}

/// Runs the parse sgr mouse operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_sgr_mouse(input: &[u8]) -> Result<Option<MouseEvent>> {
    let text = match std::str::from_utf8(input) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    let Some(rest) = text.strip_prefix("\u{1b}[<") else {
        return Ok(None);
    };
    let final_byte = match rest.chars().last() {
        Some(c @ ('M' | 'm')) => c,
        _ => return Ok(None),
    };
    let body = &rest[..rest.len().saturating_sub(final_byte.len_utf8())];
    let fields = body.split(';').collect::<Vec<_>>();
    if fields.len() < 3 {
        return Ok(None);
    }
    let Ok(code) = fields[0].parse::<u16>() else {
        return Ok(None);
    };
    let Ok(column) = fields[1].parse::<u16>() else {
        return Ok(None);
    };
    let Ok(row) = fields[2].parse::<u16>() else {
        return Ok(None);
    };
    if column == 0 || row == 0 {
        return Ok(None);
    }

    let modifiers = MouseModifiers {
        shift: code & 4 != 0,
        alt: code & 8 != 0,
        ctrl: code & 16 != 0,
    };
    let drag = code & 32 != 0;
    let wheel = code & 64 != 0;
    let base = code & 0b11;
    let button = if wheel {
        match base {
            0 => MouseButton::WheelUp,
            1 => MouseButton::WheelDown,
            other => MouseButton::Other(64 + other),
        }
    } else {
        match base {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            other => MouseButton::Other(other),
        }
    };
    let kind = if wheel {
        MouseEventKind::Scroll
    } else if final_byte == 'm' {
        MouseEventKind::Release
    } else if drag {
        MouseEventKind::Drag
    } else {
        MouseEventKind::Press
    };

    Ok(Some(MouseEvent {
        kind,
        button,
        column: column.saturating_sub(1),
        row: row.saturating_sub(1),
        modifiers,
    }))
}

/// Runs the classify mouse event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn classify_mouse_event(event: MouseEvent, policy: MousePolicy) -> MouseAction {
    if !policy.enabled {
        return MouseAction::Ignore;
    }
    if matches!(
        (event.kind, event.button),
        (MouseEventKind::Press, MouseButton::Left)
    ) && policy.over_window_frame
    {
        return MouseAction::ShowWindowChooser {
            column: event.column,
            row: event.row,
        };
    }
    if policy.pane_resize_active {
        return match (event.kind, event.button) {
            (MouseEventKind::Press | MouseEventKind::Drag, MouseButton::Left) => {
                MouseAction::ResizePane {
                    column: event.column,
                    row: event.row,
                }
            }
            (MouseEventKind::Release, MouseButton::Left) => MouseAction::FinishResizePane,
            _ => MouseAction::Ignore,
        };
    }
    match (event.kind, event.button) {
        (MouseEventKind::Press, MouseButton::Left) if policy.copy_mode_active => {
            MouseAction::CopySelectionStart(mouse_copy_position(event))
        }
        (MouseEventKind::Drag, MouseButton::Left) if policy.copy_mode_active => {
            MouseAction::CopySelectionUpdate(mouse_copy_position(event))
        }
        (MouseEventKind::Release, MouseButton::Left) if policy.copy_mode_active => {
            MouseAction::CopySelectionFinish(mouse_copy_position(event))
        }
        (MouseEventKind::Scroll, MouseButton::WheelUp) if policy.copy_mode_active => {
            MouseAction::ScrollHistory {
                lines: -3,
                position: mouse_copy_position(event),
            }
        }
        (MouseEventKind::Scroll, MouseButton::WheelDown) if policy.copy_mode_active => {
            MouseAction::ScrollHistory {
                lines: 3,
                position: mouse_copy_position(event),
            }
        }
        (MouseEventKind::Press | MouseEventKind::Drag, MouseButton::Left)
            if policy.over_pane_border =>
        {
            MouseAction::ResizePane {
                column: event.column,
                row: event.row,
            }
        }
        _ if policy.over_window_frame || policy.over_pane_border => MouseAction::Ignore,
        _ if policy.pane_application_mouse_mode => MouseAction::ForwardToPane,
        (MouseEventKind::Scroll, MouseButton::WheelUp) => MouseAction::ScrollHistory {
            lines: -3,
            position: mouse_copy_position(event),
        },
        (MouseEventKind::Scroll, MouseButton::WheelDown) => MouseAction::ScrollHistory {
            lines: 3,
            position: mouse_copy_position(event),
        },
        (MouseEventKind::Press, MouseButton::Right) => {
            MouseAction::PasteClipboard(mouse_copy_position(event))
        }
        (MouseEventKind::Release | MouseEventKind::Drag, MouseButton::Right) => MouseAction::Ignore,
        (MouseEventKind::Press, MouseButton::Left) => {
            MouseAction::FocusPane(mouse_copy_position(event))
        }
        (MouseEventKind::Drag, MouseButton::Left) => {
            MouseAction::CopySelectionUpdate(mouse_copy_position(event))
        }
        _ => MouseAction::Ignore,
    }
}

/// Runs the mouse copy position operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mouse_copy_position(event: MouseEvent) -> CopyPosition {
    CopyPosition {
        line: usize::from(event.row),
        column: usize::from(event.column),
    }
}
