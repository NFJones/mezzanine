//! Dependency-neutral terminal multiplexer input contracts.
//!
//! This module owns key-chord values plus their configuration notation and
//! pane-facing byte encoding. Product key-binding policy and routing remain in
//! Mezzanine until those responsibilities can move without importing runtime,
//! mouse-presentation, or agent behavior.

use std::collections::BTreeMap;

use mez_terminal::{MouseEvent, parse_sgr_mouse};

use crate::{MuxError, Result};

/// One logical key accepted by multiplexer input bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyCode {
    /// One Unicode character.
    Char(char),
    /// The up-arrow key.
    Up,
    /// The down-arrow key.
    Down,
    /// The left-arrow key.
    Left,
    /// The right-arrow key.
    Right,
    /// The page-up key.
    PageUp,
    /// The page-down key.
    PageDown,
    /// The home key.
    Home,
    /// The end key.
    End,
}

/// Modifier state associated with a [`KeyCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct KeyModifiers {
    /// Whether Control is held.
    pub ctrl: bool,
    /// Whether Alt is held.
    pub alt: bool,
    /// Whether Shift is held.
    pub shift: bool,
}

/// One key and its modifiers as used by multiplexer bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyChord {
    /// Logical key for this chord.
    pub code: KeyCode,
    /// Modifiers applied to the key.
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// Constructs an unmodified key chord.
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::default(),
        }
    }

    /// Constructs a Control-modified key chord.
    pub fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        }
    }

    /// Constructs an Alt-modified key chord.
    pub fn alt(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        }
    }

    /// Constructs a Control-and-Alt-modified key chord.
    pub fn ctrl_alt(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers {
                ctrl: true,
                alt: true,
                shift: false,
            },
        }
    }

    /// Parses one configuration key-chord notation.
    ///
    /// Returns an invalid-argument error when the notation is empty, repeats a
    /// modifier, omits the key, or names an unsupported multi-character key.
    pub fn parse(notation: &str) -> Result<Self> {
        parse_key_chord_notation(notation)
    }
}

/// Encodes a key chord as bytes suitable for a pane terminal.
///
/// Returns `None` when the chord cannot be represented by the supported
/// terminal input sequences.
pub fn key_chord_input_bytes(chord: KeyChord) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    match chord.code {
        KeyCode::Char(ch) if chord.modifiers.ctrl && !chord.modifiers.shift => {
            if chord.modifiers.alt {
                bytes.push(b'\x1b');
            }
            let ch = ch.to_ascii_lowercase();
            if !ch.is_ascii_lowercase() {
                return None;
            }
            bytes.push(ch as u8 - b'a' + 1);
        }
        KeyCode::Char(ch) if !chord.modifiers.ctrl && !chord.modifiers.shift && ch.is_ascii() => {
            if chord.modifiers.alt {
                bytes.push(b'\x1b');
            }
            bytes.push(ch as u8);
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End => {
            if chord.modifiers == KeyModifiers::default() {
                bytes.extend_from_slice(unmodified_special_key_bytes(chord.code)?);
            } else {
                bytes.extend_from_slice(&modified_special_key_bytes(chord.code, chord.modifiers)?);
            }
        }
        _ => return None,
    }
    Some(bytes)
}

/// Parses one configuration key-chord notation.
///
/// This free function supports callers that do not construct values through
/// [`KeyChord::parse`].
pub fn parse_key_chord_notation(notation: &str) -> Result<KeyChord> {
    let mut rest = notation.trim();
    if rest.is_empty() {
        return Err(MuxError::invalid_args("key binding must not be empty"));
    }

    let mut modifiers = KeyModifiers::default();
    while let Some(remaining) = strip_modifier_prefix(rest, &mut modifiers)? {
        rest = remaining;
    }
    if rest.is_empty() {
        return Err(MuxError::invalid_args("key binding is missing a key"));
    }

    let mut code = parse_key_code_notation(rest, modifiers.ctrl)?;
    if modifiers.shift && !modifiers.ctrl && matches!(code, KeyCode::Char('=')) {
        code = KeyCode::Char('+');
        modifiers.shift = false;
    }
    Ok(KeyChord { code, modifiers })
}

/// Decodes the first supported terminal key sequence from `input`.
///
/// Returns the decoded chord and number of consumed bytes, or `None` when the
/// input is empty, incomplete, or not one of the supported terminal sequences.
pub fn parse_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
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

fn parse_escape_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
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

fn parse_csi_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
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

fn parse_ss3_key_chord_bytes(input: &[u8]) -> Option<(KeyChord, usize)> {
    if input.len() < 3 || !input.starts_with(b"\x1bO") {
        return None;
    }
    let code = arrow_key_code(input[2]).or_else(|| home_end_key_code(input[2]))?;
    Some((KeyChord::new(code), 3))
}

fn arrow_key_code(final_byte: u8) -> Option<KeyCode> {
    match final_byte {
        b'A' => Some(KeyCode::Up),
        b'B' => Some(KeyCode::Down),
        b'C' => Some(KeyCode::Right),
        b'D' => Some(KeyCode::Left),
        _ => None,
    }
}

fn home_end_key_code(final_byte: u8) -> Option<KeyCode> {
    match final_byte {
        b'H' => Some(KeyCode::Home),
        b'F' => Some(KeyCode::End),
        _ => None,
    }
}

fn xterm_csi_modifier(params: &str, default_key_number: u16) -> Option<KeyModifiers> {
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

fn xterm_modifier_value(value: u16) -> KeyModifiers {
    let flags = value.saturating_sub(1);
    KeyModifiers {
        shift: flags & 1 != 0,
        alt: flags & 2 != 0,
        ctrl: flags & 4 != 0,
    }
}

fn unmodified_special_key_bytes(code: KeyCode) -> Option<&'static [u8]> {
    match code {
        KeyCode::Up => Some(b"\x1bOA"),
        KeyCode::Down => Some(b"\x1bOB"),
        KeyCode::Right => Some(b"\x1bOC"),
        KeyCode::Left => Some(b"\x1bOD"),
        KeyCode::PageUp => Some(b"\x1b[5~"),
        KeyCode::PageDown => Some(b"\x1b[6~"),
        KeyCode::Home => Some(b"\x1b[H"),
        KeyCode::End => Some(b"\x1b[F"),
        KeyCode::Char(_) => None,
    }
}

fn modified_special_key_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let modifier = 1
        + u8::from(modifiers.shift)
        + (u8::from(modifiers.alt) * 2)
        + (u8::from(modifiers.ctrl) * 4);
    let sequence = match code {
        KeyCode::Up => format!("\x1b[1;{modifier}A"),
        KeyCode::Down => format!("\x1b[1;{modifier}B"),
        KeyCode::Right => format!("\x1b[1;{modifier}C"),
        KeyCode::Left => format!("\x1b[1;{modifier}D"),
        KeyCode::PageUp => format!("\x1b[5;{modifier}~"),
        KeyCode::PageDown => format!("\x1b[6;{modifier}~"),
        KeyCode::Home => format!("\x1b[1;{modifier}H"),
        KeyCode::End => format!("\x1b[1;{modifier}F"),
        KeyCode::Char(_) => return None,
    };
    Some(sequence.into_bytes())
}

fn strip_modifier_prefix<'a>(
    rest: &'a str,
    modifiers: &mut KeyModifiers,
) -> Result<Option<&'a str>> {
    for (prefix, target) in [
        ("Ctrl+", ModifierTarget::Ctrl),
        ("Control+", ModifierTarget::Ctrl),
        ("C-", ModifierTarget::Ctrl),
        ("Alt+", ModifierTarget::Alt),
        ("A-", ModifierTarget::Alt),
        ("Shift+", ModifierTarget::Shift),
        ("S-", ModifierTarget::Shift),
    ] {
        let Some(remaining) = rest.strip_prefix(prefix) else {
            continue;
        };
        let duplicate = match target {
            ModifierTarget::Ctrl => replace_true(&mut modifiers.ctrl),
            ModifierTarget::Alt => replace_true(&mut modifiers.alt),
            ModifierTarget::Shift => replace_true(&mut modifiers.shift),
        };
        if duplicate {
            return Err(MuxError::invalid_args("key binding repeats a modifier"));
        }
        return Ok(Some(remaining));
    }
    Ok(None)
}

fn replace_true(value: &mut bool) -> bool {
    let was_set = *value;
    *value = true;
    was_set
}

#[derive(Debug, Clone, Copy)]
enum ModifierTarget {
    Ctrl,
    Alt,
    Shift,
}

fn parse_key_code_notation(rest: &str, ctrl: bool) -> Result<KeyCode> {
    match rest {
        "Up" => Ok(KeyCode::Up),
        "Down" => Ok(KeyCode::Down),
        "Left" => Ok(KeyCode::Left),
        "Right" => Ok(KeyCode::Right),
        "PageUp" | "PgUp" => Ok(KeyCode::PageUp),
        "PageDown" | "PgDown" => Ok(KeyCode::PageDown),
        "Home" => Ok(KeyCode::Home),
        "End" => Ok(KeyCode::End),
        "Space" => Ok(KeyCode::Char(' ')),
        _ => {
            let mut chars = rest.chars();
            let Some(mut ch) = chars.next() else {
                return Err(MuxError::invalid_args("key binding is missing a key"));
            };
            if chars.next().is_some() {
                return Err(MuxError::invalid_args("key binding key name is unknown"));
            }
            if ctrl && ch.is_ascii_uppercase() {
                ch.make_ascii_lowercase();
            }
            Ok(KeyCode::Char(ch))
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

/// Host and pane state used to classify one mouse event at the mux boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MousePolicy {
    /// Whether mouse handling is enabled for the attached client.
    pub enabled: bool,
    /// Whether the focused pane has enabled application mouse reporting.
    pub pane_application_mouse_mode: bool,
    /// Whether the focused pane has enabled SGR mouse reporting.
    pub pane_sgr_mouse_mode: bool,
    /// Whether the focused pane has enabled application cursor keys.
    pub pane_application_cursor_mode: bool,
    /// Whether the focused pane has enabled application keypad input.
    pub pane_application_keypad_mode: bool,
    /// Whether a pane-border resize interaction is active.
    pub pane_resize_active: bool,
    /// Whether the pointer is over a mux-managed pane border.
    pub over_pane_border: bool,
    /// Whether the pointer is over a mux-managed window frame.
    pub over_window_frame: bool,
    /// Whether copy mode currently owns mouse input.
    pub copy_mode_active: bool,
}

/// One routing decision produced from attached-client terminal input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInputClassification {
    /// Forward the original bytes to the focused pane.
    ForwardToPane,
    /// Enter multiplexer prefix-key mode.
    PrefixKeyMode,
    /// Consume an unbound chord following the multiplexer prefix.
    UnboundPrefix(KeyChord),
    /// Run a product-configured command binding.
    CommandBinding(String),
    /// Route a decoded terminal mouse event through mux mouse policy.
    Mouse(MouseEvent),
    /// Run a built-in multiplexer action.
    Mux(MuxAction),
}

/// Classifies one attached-client terminal input sequence using mux bindings.
pub fn classify_terminal_input(
    input: &[u8],
    bindings: &KeyBindings,
) -> Result<TerminalInputClassification> {
    classify_terminal_input_with_command_bindings(input, bindings, &BTreeMap::new())
}

/// Classifies terminal input with additional product-configured commands.
pub fn classify_terminal_input_with_command_bindings(
    input: &[u8],
    bindings: &KeyBindings,
    command_bindings: &BTreeMap<KeyChord, String>,
) -> Result<TerminalInputClassification> {
    if input.starts_with(b"\x1b[<")
        && let Some(event) = parse_sgr_mouse(input)
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

/// Configurable key chords that bypass or enter multiplexer prefix routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    /// Prefix chord that enters multiplexer key mode.
    pub escape: KeyChord,
    /// Optional direct vertical-split binding.
    pub split_vertical: Option<KeyChord>,
    /// Optional direct horizontal-split binding.
    pub split_horizontal: Option<KeyChord>,
    /// Optional direct new-window binding.
    pub new_window: Option<KeyChord>,
    /// Optional direct new-group binding.
    pub new_group: Option<KeyChord>,
    /// Optional direct agent-shell adapter binding.
    pub agent_shell: Option<KeyChord>,
    /// Optional direct upward pane-focus binding.
    pub focus_up: Option<KeyChord>,
    /// Optional direct downward pane-focus binding.
    pub focus_down: Option<KeyChord>,
    /// Optional direct leftward pane-focus binding.
    pub focus_left: Option<KeyChord>,
    /// Optional direct rightward pane-focus binding.
    pub focus_right: Option<KeyChord>,
    /// Optional direct previous-window binding.
    pub focus_previous_window: Option<KeyChord>,
    /// Optional direct next-window binding.
    pub focus_next_window: Option<KeyChord>,
    /// Optional direct previous-group binding.
    pub focus_previous_group: Option<KeyChord>,
    /// Optional direct next-group binding.
    pub focus_next_group: Option<KeyChord>,
}

impl Default for KeyBindings {
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

/// Runs the classify direct binding operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn classify_direct_binding(chord: KeyChord, bindings: &KeyBindings) -> Option<MuxAction> {
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
pub fn classify_prefix_binding(chord: KeyChord, bindings: &KeyBindings) -> Option<MuxAction> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MuxErrorKind;

    /// Verifies mux-owned binding defaults preserve the established prefix and opt-in direct keys.
    #[test]
    fn key_binding_defaults_preserve_prefix_policy() {
        let bindings = KeyBindings::default();

        assert_eq!(bindings.escape, KeyChord::ctrl(KeyCode::Char('a')));
        assert_eq!(bindings.split_vertical, None);
        assert_eq!(bindings.new_window, None);
        assert_eq!(bindings.agent_shell, None);
        assert_eq!(bindings.focus_next_group, None);
    }

    /// Verifies configuration notation preserves modifier aliases and pane-byte encoding.
    #[test]
    fn parses_and_encodes_key_chord_notation() {
        assert_eq!(
            KeyChord::parse("C-A-PageDown").unwrap(),
            KeyChord::ctrl_alt(KeyCode::PageDown)
        );
        assert_eq!(
            key_chord_input_bytes(KeyChord::parse("C-A-PageDown").unwrap()).unwrap(),
            b"\x1b[6;7~"
        );
        assert_eq!(
            key_chord_input_bytes(KeyChord::parse("A-S-=").unwrap()).unwrap(),
            b"\x1b+"
        );
    }

    /// Verifies malformed notation remains a typed invalid-argument error.
    #[test]
    fn rejects_invalid_key_chord_notation() {
        assert_eq!(
            KeyChord::parse("C-C-a").unwrap_err().kind(),
            MuxErrorKind::InvalidArgs
        );
        assert_eq!(
            KeyChord::parse("DefinitelyNotAKey").unwrap_err().kind(),
            MuxErrorKind::InvalidArgs
        );
    }

    /// Verifies terminal byte decoding recognizes control, CSI, SS3, and Alt sequences.
    #[test]
    fn decodes_supported_terminal_key_sequences() {
        assert_eq!(
            parse_key_chord_bytes(b"\x01"),
            Some((KeyChord::ctrl(KeyCode::Char('a')), 1))
        );
        assert_eq!(
            parse_key_chord_bytes(b"\x1b[1;7A"),
            Some((
                KeyChord {
                    code: KeyCode::Up,
                    modifiers: KeyModifiers {
                        ctrl: true,
                        alt: true,
                        shift: false,
                    },
                },
                6,
            ))
        );
        assert_eq!(
            parse_key_chord_bytes(b"\x1bOF"),
            Some((KeyChord::new(KeyCode::End), 3))
        );
        assert_eq!(
            parse_key_chord_bytes(b"\x1b-"),
            Some((KeyChord::alt(KeyCode::Char('-')), 2))
        );
    }

    /// Verifies incomplete escape input preserves the established Alt-key fallback.
    #[test]
    fn preserves_incomplete_terminal_key_sequence_fallbacks() {
        assert_eq!(parse_key_chord_bytes(b""), None);
        assert_eq!(parse_key_chord_bytes(b"\x1b"), None);
        assert_eq!(
            parse_key_chord_bytes(b"\x1b[1;"),
            Some((KeyChord::alt(KeyCode::Char('[')), 2))
        );
        assert_eq!(parse_key_chord_bytes(b"\xff"), None);
    }

    /// Verifies classification preserves mouse decoding and malformed mouse fallback behavior.
    #[test]
    fn classifies_terminal_mouse_input() {
        let bindings = KeyBindings::default();

        assert!(matches!(
            classify_terminal_input(b"\x1b[<0;3;4M", &bindings).unwrap(),
            TerminalInputClassification::Mouse(event) if event.column == 2 && event.row == 3
        ));
        assert_eq!(
            classify_terminal_input(b"\x1b[<malformed", &bindings).unwrap(),
            TerminalInputClassification::ForwardToPane
        );
    }

    /// Verifies prefix and configured command routing remain owned by the mux classifier.
    #[test]
    fn classifies_prefix_and_command_bindings() {
        let bindings = KeyBindings::default();
        let command_chord = KeyChord::new(KeyCode::Char('g'));
        let command_bindings = BTreeMap::from([(command_chord, "display-message".to_owned())]);

        assert_eq!(
            classify_terminal_input(b"\x01", &bindings).unwrap(),
            TerminalInputClassification::PrefixKeyMode
        );
        assert_eq!(
            classify_terminal_input_with_command_bindings(b"\x01g", &bindings, &command_bindings,)
                .unwrap(),
            TerminalInputClassification::CommandBinding("display-message".to_owned())
        );
    }
}
