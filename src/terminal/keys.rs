//! Terminal Keys implementation.
//!
//! This module owns the terminal keys boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, MouseEvent, Result, parse_sgr_mouse};

pub use mez_mux::input::{
    GroupFocusTarget, KeyBindings, KeyChord, KeyCode, KeyModifiers, MuxAction, PaneFocusDirection,
    PasteBufferTarget, WindowFocusTarget, classify_direct_binding, classify_prefix_binding,
    key_chord_input_bytes, parse_key_chord_bytes, parse_key_chord_notation,
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
