//! Terminal byte decoding for readline prompts.
//!
//! The decoder preserves incomplete escape or UTF-8 sequences between reads and
//! normalizes complete terminal sequences into buffer edits and outcomes.

use crate::error::{MezError, Result};

use super::types::{
    ReadlineBuffer, ReadlineEdit, ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt,
};

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

impl ReadlineInputDecoder {
    /// Create a decoder with no buffered partial sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bytes retained because they might complete on a later read.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Converts a pending standalone Escape key into prompt cancellation.
    ///
    /// Escape-prefixed terminal sequences are buffered so split arrow-key and
    /// function-key reads can still complete. Interactive prompt loops call this
    /// after a readiness poll with no new input so a literal Escape key can still
    /// leave the prompt instead of waiting forever for a sequence suffix.
    pub fn flush_pending_escape_as_cancel(&mut self) -> Option<ReadlineOutcome> {
        if self.bracketed_paste_active {
            return None;
        }
        if self.pending.as_slice() != b"\x1b" {
            return None;
        }
        self.pending.clear();
        Some(ReadlineOutcome::Cancelled)
    }

    /// Apply a terminal byte batch to a prompt, preserving incomplete input.
    pub fn apply_to_prompt(
        &mut self,
        prompt: &mut ReadlinePrompt,
        input: &[u8],
    ) -> Result<Vec<ReadlineOutcome>> {
        if input.is_empty() && self.pending.is_empty() {
            return Ok(Vec::new());
        }

        let mut bytes = Vec::with_capacity(self.pending.len().saturating_add(input.len()));
        bytes.extend_from_slice(&self.pending);
        bytes.extend_from_slice(input);
        self.pending.clear();

        let mut outcomes = Vec::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if self.bracketed_paste_active {
                if let Some(end_offset) = find_bytes(&bytes[cursor..], BRACKETED_PASTE_END) {
                    self.bracketed_paste
                        .extend_from_slice(&bytes[cursor..cursor + end_offset]);
                    let payload = std::mem::take(&mut self.bracketed_paste);
                    self.bracketed_paste_active = false;
                    if let Some(outcome) = apply_bracketed_paste(prompt, &payload)? {
                        outcomes.push(outcome);
                    }
                    cursor = cursor
                        .saturating_add(end_offset)
                        .saturating_add(BRACKETED_PASTE_END.len());
                    continue;
                }

                let tail = longest_suffix_that_prefixes(&bytes[cursor..], BRACKETED_PASTE_END);
                let payload_end = bytes.len().saturating_sub(tail);
                self.bracketed_paste
                    .extend_from_slice(&bytes[cursor..payload_end]);
                if tail > 0 {
                    self.pending.extend_from_slice(&bytes[payload_end..]);
                }
                break;
            }

            if bytes[cursor..].starts_with(BRACKETED_PASTE_START) {
                self.bracketed_paste_active = true;
                self.bracketed_paste.clear();
                cursor = cursor.saturating_add(BRACKETED_PASTE_START.len());
                continue;
            }

            if BRACKETED_PASTE_START.starts_with(&bytes[cursor..])
                && bytes[cursor] == BRACKETED_PASTE_START[0]
            {
                self.pending.extend_from_slice(&bytes[cursor..]);
                break;
            }

            match next_readline_sequence(&bytes[cursor..])? {
                DecodedReadlineSequence::Complete {
                    len,
                    bytes: sequence,
                } => {
                    if !sequence.is_empty() {
                        outcomes.push(prompt.apply_terminal_input(sequence)?);
                    }
                    cursor = cursor.saturating_add(len);
                }
                DecodedReadlineSequence::Incomplete => {
                    self.pending.extend_from_slice(&bytes[cursor..]);
                    break;
                }
            }
        }

        Ok(outcomes)
    }
}

/// Applies a complete bracketed paste payload without treating embedded
/// newlines as prompt submissions.
fn apply_bracketed_paste(
    prompt: &mut ReadlinePrompt,
    payload: &[u8],
) -> Result<Option<ReadlineOutcome>> {
    if payload.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(payload)
        .map_err(|_| MezError::invalid_args("readline paste is not valid UTF-8 text"))?;
    if text.is_empty() {
        return Ok(None);
    }
    prompt.selector = None;
    let outcome = prompt
        .buffer
        .apply(ReadlineEdit::InsertText(text.to_string()));
    Ok(Some(outcome))
}

/// Finds `needle` in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Returns the longest haystack suffix that may be completed into `needle`.
fn longest_suffix_that_prefixes(haystack: &[u8], needle: &[u8]) -> usize {
    let max = haystack.len().min(needle.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|len| needle.starts_with(&haystack[haystack.len() - len..]))
        .unwrap_or(0)
}

/// Apply one terminal key sequence to a readline buffer.
///
/// This function intentionally covers the common editing sequences used by
/// Unix terminals and shells without depending on a terminal UI library.
pub fn apply_readline_terminal_input(
    buffer: &mut ReadlineBuffer,
    input: &[u8],
) -> Result<ReadlineOutcome> {
    if readline_input_is_ctrl_r(input) {
        return Ok(buffer.apply(ReadlineEdit::HistorySearchBackward));
    }
    match input {
        b"\r" | b"\n" | b"\r\n" => return Ok(buffer.apply(ReadlineEdit::Submit)),
        b"\x03" => return Ok(ReadlineOutcome::Cancelled),
        b"\x04" if buffer.line().is_empty() => return Ok(ReadlineOutcome::Eof),
        b"\x04" => return Ok(buffer.apply(ReadlineEdit::DeleteForward)),
        b"\x01" | b"\x1b[H" | b"\x1b[1~" | b"\x1bOH" => {
            return Ok(buffer.apply(ReadlineEdit::MoveHome));
        }
        b"\x05" | b"\x1b[F" | b"\x1b[4~" | b"\x1bOF" => {
            return Ok(buffer.apply(ReadlineEdit::MoveEnd));
        }
        b"\x1b[1;5H" => return Ok(buffer.apply(ReadlineEdit::MoveBufferStart)),
        b"\x1b[1;5F" => return Ok(buffer.apply(ReadlineEdit::MoveBufferEnd)),
        b"\x02" | b"\x1b[D" | b"\x1bOD" => return Ok(buffer.apply(ReadlineEdit::MoveLeft)),
        b"\x06" | b"\x1b[C" | b"\x1bOC" => return Ok(buffer.apply(ReadlineEdit::MoveRight)),
        b"\x1bb" | b"\x1b[1;3D" | b"\x1b[1;5D" | b"\x1b[3D" | b"\x1b[5D" => {
            return Ok(buffer.apply(ReadlineEdit::MoveWordLeft));
        }
        b"\x1bf" | b"\x1b[1;3C" | b"\x1b[1;5C" | b"\x1b[3C" | b"\x1b[5C" => {
            return Ok(buffer.apply(ReadlineEdit::MoveWordRight));
        }
        b"\x7f" | b"\x08" => return Ok(buffer.apply(ReadlineEdit::Backspace)),
        b"\x1b[3~" => return Ok(buffer.apply(ReadlineEdit::DeleteForward)),
        b"\x17" | b"\x1b\x7f" | b"\x1b\x08" => {
            return Ok(buffer.apply(ReadlineEdit::KillWordLeft));
        }
        b"\x1bd" => return Ok(buffer.apply(ReadlineEdit::KillWordRight)),
        b"\x15" => return Ok(buffer.apply(ReadlineEdit::KillToStart)),
        b"\x0b" => return Ok(buffer.apply(ReadlineEdit::KillToEnd)),
        b"\x10" => return Ok(buffer.apply(ReadlineEdit::HistoryPrevious)),
        b"\x0e" => return Ok(buffer.apply(ReadlineEdit::HistoryNext)),
        b"\x1b[A" | b"\x1bOA" => {
            return Ok(buffer.apply(ReadlineEdit::MoveRowUpOrHistoryPrevious));
        }
        b"\x1b[B" | b"\x1bOB" => {
            return Ok(buffer.apply(ReadlineEdit::MoveRowDownOrHistoryNext));
        }
        b"\x12" => return Ok(buffer.apply(ReadlineEdit::HistorySearchBackward)),
        _ => {}
    }

    if input.iter().any(|byte| byte.is_ascii_control()) {
        return Ok(ReadlineOutcome::Noop);
    }
    let text = std::str::from_utf8(input)
        .map_err(|_| MezError::invalid_args("readline input is not valid UTF-8 text"))?;
    if text.is_empty() {
        return Ok(ReadlineOutcome::Noop);
    }
    Ok(buffer.apply(ReadlineEdit::InsertText(text.to_string())))
}

/// Returns whether an encoded terminal modified-key sequence represents
/// Ctrl+target. This covers CSI-u and xterm modifyOtherKeys encodings used by
/// modern terminals when they do not emit the legacy ASCII control byte.
pub(super) fn readline_input_is_ctrl_r(input: &[u8]) -> bool {
    if input == b"\x12" {
        return true;
    }
    readline_modified_key_is_ctrl(input, 'r')
}

/// Returns whether an encoded terminal modified-key sequence represents
/// Ctrl+Shift+R for forward incremental history search.
pub(super) fn readline_input_is_ctrl_shift_r(input: &[u8]) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl && !modifiers.alt && modifiers.shift && ch == 'R'
}

/// Returns whether an encoded terminal modified-key sequence represents
/// Ctrl+target.
fn readline_modified_key_is_ctrl(input: &[u8], target: char) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl && !modifiers.alt && ch.eq_ignore_ascii_case(&target)
}

/// Decodes common terminal modified-character key encodings.
fn readline_modified_key(input: &[u8]) -> Option<(char, ReadlineKeyModifiers)> {
    if !input.starts_with(b"\x1b[") {
        return None;
    }
    let final_byte = *input.last()?;
    let params = std::str::from_utf8(&input[2..input.len().saturating_sub(1)]).ok()?;
    match final_byte {
        b'u' => {
            let mut parts = params.split(';');
            let codepoint = parts.next()?.parse::<u32>().ok()?;
            let modifiers = parts
                .next()
                .and_then(|part| part.parse::<u16>().ok())
                .map(readline_xterm_modifier_value)
                .unwrap_or_default();
            char::from_u32(codepoint).map(|ch| (ch, modifiers))
        }
        b'~' => {
            let mut parts = params.split(';');
            if parts.next()? != "27" {
                return None;
            }
            let modifiers = parts
                .next()
                .and_then(|part| part.parse::<u16>().ok())
                .map(readline_xterm_modifier_value)?;
            let codepoint = parts.next()?.parse::<u32>().ok()?;
            char::from_u32(codepoint).map(|ch| (ch, modifiers))
        }
        _ => None,
    }
}

/// Modifier bits decoded from xterm-compatible modified key parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ReadlineKeyModifiers {
    /// Whether Shift was held.
    shift: bool,
    /// Whether Alt was held.
    alt: bool,
    /// Whether Control was held.
    ctrl: bool,
}

/// Converts xterm modifier values where 1 is the unmodified baseline.
fn readline_xterm_modifier_value(value: u16) -> ReadlineKeyModifiers {
    let flags = value.saturating_sub(1);
    ReadlineKeyModifiers {
        shift: flags & 1 != 0,
        alt: flags & 2 != 0,
        ctrl: flags & 4 != 0,
    }
}

/// Carries Decoded Readline Sequence state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
enum DecodedReadlineSequence<'a> {
    /// Represents the Complete case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Complete {
        /// Stores the len value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        len: usize,
        /// Stores the bytes value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        bytes: &'a [u8],
    },
    /// Represents the Incomplete case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Incomplete,
}

/// Runs the next readline sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn next_readline_sequence(input: &[u8]) -> Result<DecodedReadlineSequence<'_>> {
    if input.is_empty() {
        return Ok(DecodedReadlineSequence::Incomplete);
    }

    if input.starts_with(b"\r\n") {
        return Ok(DecodedReadlineSequence::Complete {
            len: 2,
            bytes: b"\r\n",
        });
    }

    if input[0] == b'\x1b' {
        return decode_escape_sequence(input);
    }

    if input[0].is_ascii_control() {
        return Ok(DecodedReadlineSequence::Complete {
            len: 1,
            bytes: &input[..1],
        });
    }

    decode_text_sequence(input)
}

/// Runs the decode escape sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn decode_escape_sequence(input: &[u8]) -> Result<DecodedReadlineSequence<'_>> {
    if input.len() == 1 {
        return Ok(DecodedReadlineSequence::Incomplete);
    }
    if input[1] != b'[' {
        if input[1] == b'O' {
            if input.len() < 3 {
                return Ok(DecodedReadlineSequence::Incomplete);
            }
            return Ok(DecodedReadlineSequence::Complete {
                len: 3,
                bytes: &input[..3],
            });
        }
        return Ok(DecodedReadlineSequence::Complete {
            len: 2,
            bytes: &input[..2],
        });
    }

    for (index, byte) in input.iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(byte) {
            let len = index.saturating_add(1);
            return Ok(DecodedReadlineSequence::Complete {
                len,
                bytes: &input[..len],
            });
        }
    }

    Ok(DecodedReadlineSequence::Incomplete)
}

/// Runs the decode text sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn decode_text_sequence(input: &[u8]) -> Result<DecodedReadlineSequence<'_>> {
    let len = input
        .iter()
        .position(|byte| *byte == b'\x1b' || byte.is_ascii_control())
        .unwrap_or(input.len());
    let candidate = &input[..len];
    match std::str::from_utf8(candidate) {
        Ok(_) => Ok(DecodedReadlineSequence::Complete {
            len,
            bytes: candidate,
        }),
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            let len = error.valid_up_to();
            Ok(DecodedReadlineSequence::Complete {
                len,
                bytes: &input[..len],
            })
        }
        Err(error) if error.error_len().is_none() => Ok(DecodedReadlineSequence::Incomplete),
        Err(error) if error.valid_up_to() > 0 => {
            let len = error.valid_up_to();
            Ok(DecodedReadlineSequence::Complete {
                len,
                bytes: &input[..len],
            })
        }
        Err(_) => Err(MezError::invalid_args(
            "readline input is not valid UTF-8 text",
        )),
    }
}
