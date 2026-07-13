//! Dependency-neutral terminal multiplexer input contracts.
//!
//! This module owns key-chord values plus their configuration notation and
//! pane-facing byte encoding. Product key-binding policy and routing remain in
//! Mezzanine until those responsibilities can move without importing runtime,
//! mouse-presentation, or agent behavior.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MuxErrorKind;

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
}
