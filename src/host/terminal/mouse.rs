//! Terminal Mouse implementation.
//!
//! This module owns the terminal mouse boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use mez_mux::copy::CopyPosition;

use mez_mux::attached_client::AttachedMouseAction;
use mez_terminal::MouseEvent;

// Mouse event parsing and policy classification.

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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
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
    /// Starts a text selection inside the primary command-output overlay.
    BeginDisplayOverlaySelection {
        /// Zero-based terminal position where the selection started.
        position: CopyPosition,
    },
    /// Extends a text selection inside the primary command-output overlay.
    UpdateDisplayOverlaySelection {
        /// Zero-based terminal position currently under the pointer.
        position: CopyPosition,
    },
    /// Finishes a text selection inside the primary command-output overlay.
    FinishDisplayOverlaySelection {
        /// Zero-based terminal position where the selection ended.
        position: CopyPosition,
    },
    /// Selects a line in the primary command-output overlay.
    #[allow(
        dead_code,
        reason = "product mouse action is handled by runtime input projection"
    )]
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
    /// Selects and copies the readline-style word segment under the pointer.
    #[allow(
        dead_code,
        reason = "product mouse action is handled by runtime input projection"
    )]
    CopyWord(CopyPosition),
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

impl From<AttachedMouseAction> for MouseAction {
    /// Projects neutral mux mouse policy into product runtime actions.
    fn from(action: AttachedMouseAction) -> Self {
        match action {
            AttachedMouseAction::Ignore => Self::Ignore,
            AttachedMouseAction::ForwardToPane => Self::ForwardToPane,
            AttachedMouseAction::ShowWindowChooser { column, row } => {
                Self::ShowWindowChooser { column, row }
            }
            AttachedMouseAction::ResizePane { column, row } => Self::ResizePane { column, row },
            AttachedMouseAction::FinishResizePane => Self::FinishResizePane,
            AttachedMouseAction::CopySelectionStart(position) => Self::CopySelectionStart(position),
            AttachedMouseAction::CopySelectionUpdate(position) => {
                Self::CopySelectionUpdate(position)
            }
            AttachedMouseAction::CopySelectionFinish(position) => {
                Self::CopySelectionFinish(position)
            }
            AttachedMouseAction::ScrollHistory { lines, position } => {
                Self::ScrollHistory { lines, position }
            }
            AttachedMouseAction::PasteClipboard(position) => Self::PasteClipboard(position),
            AttachedMouseAction::FocusPane(position) => Self::FocusPane(position),
        }
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
