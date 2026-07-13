//! Terminal Keys implementation.
//!
//! This module owns the terminal keys boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, MouseEvent, Result, parse_sgr_mouse};

pub use mez_mux::input::{
    KeyChord, KeyCode, KeyModifiers, key_chord_input_bytes, parse_key_chord_notation,
};

// Key chords, bindings, and input classification.

/// Defines the DEFAULT HISTORY LIMIT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_HISTORY_LIMIT: usize = 10_000;
/// Defines the DEFAULT HISTORY ROTATE LINES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_HISTORY_ROTATE_LINES: usize = 1_000;
/// Defines the DEFAULT PASTE BUFFER LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PASTE_BUFFER_LIMIT_BYTES: usize = 1_048_576;
/// Defines the DEFAULT PANE TERM const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PANE_TERM: &str = "screen-256color";
/// Defines the DEFAULT MEZZANINE TERMINFO const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MEZZANINE_TERMINFO: &str = "mez-256color";
/// Defines the MEZZANINE TERMINFO NAMES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MEZZANINE_TERMINFO_NAMES: &[&str] = &["mez-256color", "mezzanine-256color"];
/// Defines the TERMINFO FALLBACK ORDER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const TERMINFO_FALLBACK_ORDER: &[&str] = &["screen-256color", "screen", "vt100", "dumb"];
/// Carries Key Bindings state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    /// Stores the escape value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub escape: KeyChord,
    /// Stores the split vertical value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub split_vertical: Option<KeyChord>,
    /// Stores the split horizontal value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub split_horizontal: Option<KeyChord>,
    /// Stores the new window value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub new_window: Option<KeyChord>,
    /// Stores the new group value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub new_group: Option<KeyChord>,
    /// Stores the agent shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_shell: Option<KeyChord>,
    /// Stores the focus up value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_up: Option<KeyChord>,
    /// Stores the focus down value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_down: Option<KeyChord>,
    /// Stores the focus left value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_left: Option<KeyChord>,
    /// Stores the focus right value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_right: Option<KeyChord>,
    /// Stores the focus previous window value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_previous_window: Option<KeyChord>,
    /// Stores the focus next window value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_next_window: Option<KeyChord>,
    /// Stores the focus previous group value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_previous_group: Option<KeyChord>,
    /// Stores the focus next group value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_next_group: Option<KeyChord>,
}

impl Default for KeyBindings {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            escape: KeyChord::ctrl(KeyCode::Char('a')),
            split_vertical: None,
            split_horizontal: None,
            new_window: None,
            new_group: None,
            agent_shell: None,
            focus_up: None,
            focus_down: None,
            focus_left: None,
            focus_right: None,
            focus_previous_window: None,
            focus_next_window: None,
            focus_previous_group: None,
            focus_next_group: None,
        }
    }
}

/// Carries Pane Focus Direction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocusDirection {
    /// Represents the Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Up,
    /// Represents the Down case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Down,
    /// Represents the Left case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Left,
    /// Represents the Right case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Right,
}

/// Carries Window Focus Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFocusTarget {
    /// Represents the Next case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Next,
    /// Represents the Previous case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Previous,
    /// Represents the Last Active case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LastActive,
    /// Represents the Index case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Index(u8),
    /// Represents the Prompt For Index case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PromptForIndex,
    /// Represents the Prompt For New Index case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PromptForNewIndex,
    /// Represents the Choose Interactively case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ChooseInteractively,
}

/// Carries Group Focus Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupFocusTarget {
    /// Represents the Next case for this enumeration.
    Next,
    /// Represents the Previous case for this enumeration.
    Previous,
    /// Represents the Last Active case for this enumeration.
    LastActive,
    /// Represents the Choose Interactively case for this enumeration.
    ChooseInteractively,
}

/// Carries Paste Buffer Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteBufferTarget {
    /// Represents the Most Recent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MostRecent,
    /// Represents the Choose Interactively case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ChooseInteractively,
}

/// Carries Mux Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxAction {
    /// Represents the Send Prefix To Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SendPrefixToPane,
    /// Represents the Enter Command Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnterCommandPrompt,
    /// Represents the List Key Bindings case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ListKeyBindings,
    /// Represents the Detach Primary Client case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DetachPrimaryClient,
    /// Represents the Choose Client Or Observer To Detach case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ChooseClientOrObserverToDetach,
    /// Represents the New Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewWindow,
    /// Represents the New Group case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewGroup,
    /// Represents the Rename Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RenameWindow,
    /// Represents the Kill Window After Confirmation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    KillWindowAfterConfirmation,
    /// Represents the Focus Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusWindow(WindowFocusTarget),
    /// Represents the Focus Group case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusGroup(GroupFocusTarget),
    /// Represents the Split Pane Vertical case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SplitPaneVertical,
    /// Represents the Split Pane Horizontal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SplitPaneHorizontal,
    /// Represents the Focus Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusPane(PaneFocusDirection),
    /// Represents the Cycle Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CyclePane,
    /// Represents the Focus Last Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusLastPane,
    /// Represents the Show Pane Indexes case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShowPaneIndexes,
    /// Represents the Toggle Pane Zoom case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TogglePaneZoom,
    /// Represents the Cycle Layouts case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CycleLayouts,
    /// Represents the Kill Pane After Confirmation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    KillPaneAfterConfirmation,
    /// Represents the Break Pane To New Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BreakPaneToNewWindow,
    /// Represents the Swap Pane Previous case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SwapPanePrevious,
    /// Represents the Swap Pane Next case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SwapPaneNext,
    /// Represents the Enter Copy Mode case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnterCopyMode,
    /// Represents the Enter Copy Mode And Page Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnterCopyModeAndPageUp,
    /// Represents the Paste Buffer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PasteBuffer(PasteBufferTarget),
    /// Represents the List Paste Buffers case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ListPasteBuffers,
    /// Represents the Delete Most Recent Paste Buffer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeleteMostRecentPasteBuffer,
    /// Represents the Choose Pending Observers case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ChoosePendingObservers,
    /// Represents the Show Messages case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShowMessages,
    /// Represents the Toggle Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ToggleAgentShell,
}

/// Carries Terminal Input Classification state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInputClassification {
    /// Represents the Forward To Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ForwardToPane,
    /// Represents the Prefix Key Mode case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PrefixKeyMode,
    /// Represents the Unbound Prefix case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UnboundPrefix(KeyChord),
    /// Represents the Command Binding case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CommandBinding(String),
    /// Represents the Mouse case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Mouse(MouseEvent),
    /// Represents the Mux case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Mux(MuxAction),
}

/// Runs the classify terminal input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn classify_terminal_input(
    input: &[u8],
    bindings: &KeyBindings,
) -> Result<TerminalInputClassification> {
    classify_terminal_input_with_command_bindings(input, bindings, &BTreeMap::new())
}

/// Runs the classify terminal input with command bindings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn classify_terminal_input_with_command_bindings(
    input: &[u8],
    bindings: &KeyBindings,
    command_bindings: &BTreeMap<KeyChord, String>,
) -> Result<TerminalInputClassification> {
    if input.starts_with(b"\x1b[<")
        && let Ok(Some(event)) = parse_sgr_mouse(input)
    {
        return Ok(TerminalInputClassification::Mouse(event));
    }

    let Some((first, first_len)) = parse_key_chord_bytes(input) else {
        return Ok(TerminalInputClassification::ForwardToPane);
    };

    if first == bindings.escape {
        if first_len == input.len() {
            return Ok(TerminalInputClassification::PrefixKeyMode);
        }
        let remaining = &input[first_len..];
        let Some((second, second_len)) = parse_key_chord_bytes(remaining) else {
            return Ok(TerminalInputClassification::UnboundPrefix(first));
        };
        if second_len != remaining.len() {
            return Ok(TerminalInputClassification::UnboundPrefix(second));
        }
        if let Some(command) = command_bindings.get(&second) {
            return Ok(TerminalInputClassification::CommandBinding(
                command.to_string(),
            ));
        }
        return Ok(classify_prefix_binding(second, bindings)
            .map(TerminalInputClassification::Mux)
            .unwrap_or(TerminalInputClassification::UnboundPrefix(second)));
    }

    if first_len != input.len() {
        return Ok(TerminalInputClassification::ForwardToPane);
    }

    Ok(classify_direct_binding(first, bindings)
        .map(TerminalInputClassification::Mux)
        .unwrap_or(TerminalInputClassification::ForwardToPane))
}

/// Runs the parse key chord bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
    if input.is_empty() {
        return None;
    }
    let first = input[0];
    match first {
        0x01..=0x1a => Some((
            KeyChord::ctrl(KeyCode::Char(char::from(b'a' + first - 1))),
            1,
        )),
        b'\x1b' => parse_escape_key_chord_bytes(input),
        b' '..=b'~' => Some((KeyChord::new(KeyCode::Char(char::from(first))), 1)),
        _ => None,
    }
}

/// Runs the parse escape key chord bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_escape_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
    if let Some(parsed) = parse_csi_key_chord_bytes(input) {
        return Some(parsed);
    }
    if let Some(parsed) = parse_ss3_key_chord_bytes(input) {
        return Some(parsed);
    }

    let second = *input.get(1)?;
    if second.is_ascii_graphic() || second == b' ' {
        return Some((KeyChord::alt(KeyCode::Char(char::from(second))), 2));
    }
    None
}

/// Runs the parse csi key chord bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_csi_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
    if !input.starts_with(b"\x1b[") {
        return None;
    }
    let final_index = input
        .iter()
        .enumerate()
        .skip(2)
        .find_map(|(index, byte)| (b'@'..=b'~').contains(byte).then_some(index))?;
    let final_byte = input[final_index];
    let params = std::str::from_utf8(&input[2..final_index]).ok()?;

    let (code, modifiers) = match final_byte {
        b'A' | b'B' | b'C' | b'D' => {
            let modifiers = xterm_csi_modifier(params, 1)?;
            (arrow_key_code(final_byte)?, modifiers)
        }
        b'H' | b'F' => {
            let modifiers = xterm_csi_modifier(params, 1)?;
            (home_end_key_code(final_byte)?, modifiers)
        }
        b'~' => {
            let mut parts = params.split(';');
            let key_number = parts.next()?.parse::<u16>().ok()?;
            let code = match key_number {
                1 | 7 => KeyCode::Home,
                4 | 8 => KeyCode::End,
                5 => KeyCode::PageUp,
                6 => KeyCode::PageDown,
                _ => return None,
            };
            let modifiers = parts
                .next()
                .and_then(|part| part.parse::<u16>().ok())
                .map(xterm_modifier_value)
                .unwrap_or_default();
            (code, modifiers)
        }
        _ => return None,
    };

    Some((KeyChord { code, modifiers }, final_index + 1))
}

/// Runs the parse ss3 key chord bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_ss3_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
    if input.len() < 3 || !input.starts_with(b"\x1bO") {
        return None;
    }
    let code = arrow_key_code(input[2]).or_else(|| home_end_key_code(input[2]))?;
    Some((KeyChord::new(code), 3))
}

/// Runs the arrow key code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn arrow_key_code(final_byte: u8) -> Option<KeyCode> {
    match final_byte {
        b'A' => Some(KeyCode::Up),
        b'B' => Some(KeyCode::Down),
        b'C' => Some(KeyCode::Right),
        b'D' => Some(KeyCode::Left),
        _ => None,
    }
}

/// Runs the home end key code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn home_end_key_code(final_byte: u8) -> Option<KeyCode> {
    match final_byte {
        b'H' => Some(KeyCode::Home),
        b'F' => Some(KeyCode::End),
        _ => None,
    }
}

/// Runs the xterm csi modifier operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn xterm_csi_modifier(params: &str, default_key_number: u16) -> Option<KeyModifiers> {
    if params.is_empty() {
        return Some(KeyModifiers::default());
    }

    let mut parts = params.split(';');
    let key_number = parts
        .next()
        .filter(|part| !part.is_empty())
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(default_key_number);
    if key_number != default_key_number {
        return None;
    }
    let modifier = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .map(xterm_modifier_value)
        .unwrap_or_default();
    Some(modifier)
}

/// Runs the xterm modifier value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn xterm_modifier_value(value: u16) -> KeyModifiers {
    let flags = value.saturating_sub(1);
    KeyModifiers {
        shift: flags & 1 != 0,
        alt: flags & 2 != 0,
        ctrl: flags & 4 != 0,
    }
}

/// Runs the classify direct binding operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn classify_direct_binding(
    chord: KeyChord,
    bindings: &KeyBindings,
) -> Option<MuxAction> {
    if bindings.split_vertical == Some(chord) {
        Some(MuxAction::SplitPaneVertical)
    } else if bindings.split_horizontal == Some(chord) {
        Some(MuxAction::SplitPaneHorizontal)
    } else if bindings.new_window == Some(chord) {
        Some(MuxAction::NewWindow)
    } else if bindings.new_group == Some(chord) {
        Some(MuxAction::NewGroup)
    } else if bindings.agent_shell == Some(chord) {
        Some(MuxAction::ToggleAgentShell)
    } else if bindings.focus_up == Some(chord) {
        Some(MuxAction::FocusPane(PaneFocusDirection::Up))
    } else if bindings.focus_down == Some(chord) {
        Some(MuxAction::FocusPane(PaneFocusDirection::Down))
    } else if bindings.focus_left == Some(chord) {
        Some(MuxAction::FocusPane(PaneFocusDirection::Left))
    } else if bindings.focus_right == Some(chord) {
        Some(MuxAction::FocusPane(PaneFocusDirection::Right))
    } else if bindings.focus_previous_window == Some(chord) {
        Some(MuxAction::FocusWindow(WindowFocusTarget::Previous))
    } else if bindings.focus_next_window == Some(chord) {
        Some(MuxAction::FocusWindow(WindowFocusTarget::Next))
    } else if bindings.focus_previous_group == Some(chord) {
        Some(MuxAction::FocusGroup(GroupFocusTarget::Previous))
    } else if bindings.focus_next_group == Some(chord) {
        Some(MuxAction::FocusGroup(GroupFocusTarget::Next))
    } else {
        None
    }
}

/// Runs the classify prefix binding operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn classify_prefix_binding(
    chord: KeyChord,
    bindings: &KeyBindings,
) -> Option<MuxAction> {
    if chord == bindings.escape {
        return Some(MuxAction::SendPrefixToPane);
    }
    if chord.modifiers != KeyModifiers::default() {
        return None;
    }

    match chord.code {
        KeyCode::Char(':') => Some(MuxAction::EnterCommandPrompt),
        KeyCode::Char('?') => Some(MuxAction::ListKeyBindings),
        KeyCode::Char('d') => Some(MuxAction::DetachPrimaryClient),
        KeyCode::Char('D') => Some(MuxAction::ChooseClientOrObserverToDetach),
        KeyCode::Char('G') => Some(MuxAction::FocusGroup(GroupFocusTarget::ChooseInteractively)),
        KeyCode::Char('C') => Some(MuxAction::NewGroup),
        KeyCode::Char('(') => Some(MuxAction::FocusGroup(GroupFocusTarget::Previous)),
        KeyCode::Char(')') => Some(MuxAction::FocusGroup(GroupFocusTarget::Next)),
        KeyCode::Char('c') => Some(MuxAction::NewWindow),
        KeyCode::Char('a') => Some(MuxAction::ToggleAgentShell),
        KeyCode::Char(',') => Some(MuxAction::RenameWindow),
        KeyCode::Char('&') => Some(MuxAction::KillWindowAfterConfirmation),
        KeyCode::Char('w') => Some(MuxAction::FocusWindow(
            WindowFocusTarget::ChooseInteractively,
        )),
        KeyCode::Char('n') => Some(MuxAction::FocusWindow(WindowFocusTarget::Next)),
        KeyCode::Char('p') => Some(MuxAction::FocusWindow(WindowFocusTarget::Previous)),
        KeyCode::Char('l') => Some(MuxAction::FocusWindow(WindowFocusTarget::LastActive)),
        KeyCode::Char(index @ '0'..='9') => Some(MuxAction::FocusWindow(WindowFocusTarget::Index(
            index as u8 - b'0',
        ))),
        KeyCode::Char('\'') => Some(MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex)),
        KeyCode::Char('.') => Some(MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex)),
        KeyCode::Char('%') => Some(MuxAction::SplitPaneVertical),
        KeyCode::Char('"') => Some(MuxAction::SplitPaneHorizontal),
        KeyCode::Up => Some(MuxAction::FocusPane(PaneFocusDirection::Up)),
        KeyCode::Down => Some(MuxAction::FocusPane(PaneFocusDirection::Down)),
        KeyCode::Left => Some(MuxAction::FocusPane(PaneFocusDirection::Left)),
        KeyCode::Right => Some(MuxAction::FocusPane(PaneFocusDirection::Right)),
        KeyCode::Char('o') => Some(MuxAction::CyclePane),
        KeyCode::Char(';') => Some(MuxAction::FocusLastPane),
        KeyCode::Char('q') => Some(MuxAction::ShowPaneIndexes),
        KeyCode::Char('z') => Some(MuxAction::TogglePaneZoom),
        KeyCode::Char(' ') => Some(MuxAction::CycleLayouts),
        KeyCode::Char('x') => Some(MuxAction::KillPaneAfterConfirmation),
        KeyCode::Char('!') => Some(MuxAction::BreakPaneToNewWindow),
        KeyCode::Char('{') => Some(MuxAction::SwapPanePrevious),
        KeyCode::Char('}') => Some(MuxAction::SwapPaneNext),
        KeyCode::PageUp => Some(MuxAction::EnterCopyModeAndPageUp),
        KeyCode::Char('[') => Some(MuxAction::EnterCopyMode),
        KeyCode::Char(']') => Some(MuxAction::PasteBuffer(PasteBufferTarget::MostRecent)),
        KeyCode::Char('#') => Some(MuxAction::ListPasteBuffers),
        KeyCode::Char('=') => Some(MuxAction::PasteBuffer(
            PasteBufferTarget::ChooseInteractively,
        )),
        KeyCode::Char('-') => Some(MuxAction::DeleteMostRecentPasteBuffer),
        KeyCode::Char('O') => Some(MuxAction::ChoosePendingObservers),
        KeyCode::Char('~') => Some(MuxAction::ShowMessages),
        _ => None,
    }
}
