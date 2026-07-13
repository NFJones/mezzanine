//! Terminal byte decoding for multiplexer-owned readline surfaces.
//!
//! This module owns terminal sequence framing, partial-input buffering,
//! bracketed-paste assembly, and baseline readline key bindings. Product prompt
//! policy applies the decoded input to selector-aware prompt state.

use super::{ReadlineBuffer, ReadlineEdit, ReadlineOutcome};
use crate::{MuxError, Result};

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// One complete terminal input item decoded from a byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineDecodedInput {
    /// A complete control, escape, or UTF-8 text sequence.
    Sequence(Vec<u8>),
    /// One complete bracketed-paste payload.
    BracketedPaste(String),
}

/// Stateful terminal-input decoder for readline prompt surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadlineTerminalInputDecoder {
    pending: Vec<u8>,
    bracketed_paste_active: bool,
    bracketed_paste: Vec<u8>,
}

impl ReadlineTerminalInputDecoder {
    /// Creates a decoder with no buffered partial sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns bytes retained because they may complete on a later read.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Consumes a pending standalone Escape key.
    pub fn flush_pending_escape(&mut self) -> bool {
        if self.bracketed_paste_active || self.pending.as_slice() != b"\x1b" {
            return false;
        }
        self.pending.clear();
        true
    }

    /// Decodes complete input items while preserving incomplete suffixes.
    pub fn decode(&mut self, input: &[u8]) -> Result<Vec<ReadlineDecodedInput>> {
        if input.is_empty() && self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let mut bytes = Vec::with_capacity(self.pending.len().saturating_add(input.len()));
        bytes.extend_from_slice(&self.pending);
        bytes.extend_from_slice(input);
        self.pending.clear();

        let mut decoded = Vec::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if self.bracketed_paste_active {
                if let Some(end_offset) = find_bytes(&bytes[cursor..], BRACKETED_PASTE_END) {
                    self.bracketed_paste
                        .extend_from_slice(&bytes[cursor..cursor + end_offset]);
                    let payload = std::mem::take(&mut self.bracketed_paste);
                    self.bracketed_paste_active = false;
                    let text = String::from_utf8(payload).map_err(|_| {
                        MuxError::invalid_args("readline paste is not valid UTF-8 text")
                    })?;
                    if !text.is_empty() {
                        decoded.push(ReadlineDecodedInput::BracketedPaste(text));
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
                DecodedSequence::Complete { len, bytes: item } => {
                    if !item.is_empty() {
                        decoded.push(ReadlineDecodedInput::Sequence(item.to_vec()));
                    }
                    cursor = cursor.saturating_add(len);
                }
                DecodedSequence::Incomplete => {
                    self.pending.extend_from_slice(&bytes[cursor..]);
                    break;
                }
            }
        }
        Ok(decoded)
    }
}

/// Applies one complete terminal key sequence to a readline buffer.
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
        b"\x17" | b"\x1b\x7f" | b"\x1b\x08" => return Ok(buffer.apply(ReadlineEdit::KillWordLeft)),
        b"\x1bd" => return Ok(buffer.apply(ReadlineEdit::KillWordRight)),
        b"\x15" => return Ok(buffer.apply(ReadlineEdit::KillToStart)),
        b"\x0b" => return Ok(buffer.apply(ReadlineEdit::KillToEnd)),
        b"\x10" => return Ok(buffer.apply(ReadlineEdit::HistoryPrevious)),
        b"\x0e" => return Ok(buffer.apply(ReadlineEdit::HistoryNext)),
        b"\x1b[A" | b"\x1bOA" => return Ok(buffer.apply(ReadlineEdit::MoveRowUpOrHistoryPrevious)),
        b"\x1b[B" | b"\x1bOB" => return Ok(buffer.apply(ReadlineEdit::MoveRowDownOrHistoryNext)),
        b"\x12" => return Ok(buffer.apply(ReadlineEdit::HistorySearchBackward)),
        _ => {}
    }
    if input.iter().any(|byte| byte.is_ascii_control()) {
        return Ok(ReadlineOutcome::Noop);
    }
    let text = std::str::from_utf8(input)
        .map_err(|_| MuxError::invalid_args("readline input is not valid UTF-8 text"))?;
    if text.is_empty() {
        return Ok(ReadlineOutcome::Noop);
    }
    Ok(buffer.apply(ReadlineEdit::InsertText(text.to_string())))
}

/// Reports whether input encodes Ctrl+R.
pub fn readline_input_is_ctrl_r(input: &[u8]) -> bool {
    input == b"\x12" || readline_modified_key_is_ctrl(input, 'r')
}

/// Reports whether input encodes Ctrl+Shift+R.
pub fn readline_input_is_ctrl_shift_r(input: &[u8]) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl && !modifiers.alt && modifiers.shift && ch == 'R'
}

fn readline_modified_key_is_ctrl(input: &[u8], target: char) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl && !modifiers.alt && ch.eq_ignore_ascii_case(&target)
}

fn readline_modified_key(input: &[u8]) -> Option<(char, KeyModifiers)> {
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
                .and_then(|part| part.parse().ok())
                .map(xterm_modifier)
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
                .and_then(|part| part.parse().ok())
                .map(xterm_modifier)?;
            let codepoint = parts.next()?.parse::<u32>().ok()?;
            char::from_u32(codepoint).map(|ch| (ch, modifiers))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct KeyModifiers {
    shift: bool,
    alt: bool,
    ctrl: bool,
}

fn xterm_modifier(value: u16) -> KeyModifiers {
    let flags = value.saturating_sub(1);
    KeyModifiers {
        shift: flags & 1 != 0,
        alt: flags & 2 != 0,
        ctrl: flags & 4 != 0,
    }
}

enum DecodedSequence<'a> {
    Complete { len: usize, bytes: &'a [u8] },
    Incomplete,
}

fn next_readline_sequence(input: &[u8]) -> Result<DecodedSequence<'_>> {
    if input.is_empty() {
        return Ok(DecodedSequence::Incomplete);
    }
    if input.starts_with(b"\r\n") {
        return Ok(DecodedSequence::Complete {
            len: 2,
            bytes: b"\r\n",
        });
    }
    if input[0] == b'\x1b' {
        return decode_escape_sequence(input);
    }
    if input[0].is_ascii_control() {
        return Ok(DecodedSequence::Complete {
            len: 1,
            bytes: &input[..1],
        });
    }
    decode_text_sequence(input)
}

fn decode_escape_sequence(input: &[u8]) -> Result<DecodedSequence<'_>> {
    if input.len() == 1 {
        return Ok(DecodedSequence::Incomplete);
    }
    if input[1] != b'[' {
        if input[1] == b'O' {
            if input.len() < 3 {
                return Ok(DecodedSequence::Incomplete);
            }
            return Ok(DecodedSequence::Complete {
                len: 3,
                bytes: &input[..3],
            });
        }
        return Ok(DecodedSequence::Complete {
            len: 2,
            bytes: &input[..2],
        });
    }
    for (index, byte) in input.iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(byte) {
            let len = index.saturating_add(1);
            return Ok(DecodedSequence::Complete {
                len,
                bytes: &input[..len],
            });
        }
    }
    Ok(DecodedSequence::Incomplete)
}

fn decode_text_sequence(input: &[u8]) -> Result<DecodedSequence<'_>> {
    let len = input
        .iter()
        .position(|byte| *byte == b'\x1b' || byte.is_ascii_control())
        .unwrap_or(input.len());
    let candidate = &input[..len];
    match std::str::from_utf8(candidate) {
        Ok(_) => Ok(DecodedSequence::Complete {
            len,
            bytes: candidate,
        }),
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            let len = error.valid_up_to();
            Ok(DecodedSequence::Complete {
                len,
                bytes: &input[..len],
            })
        }
        Err(error) if error.error_len().is_none() => Ok(DecodedSequence::Incomplete),
        Err(error) if error.valid_up_to() > 0 => {
            let len = error.valid_up_to();
            Ok(DecodedSequence::Complete {
                len,
                bytes: &input[..len],
            })
        }
        Err(_) => Err(MuxError::invalid_args(
            "readline input is not valid UTF-8 text",
        )),
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn longest_suffix_that_prefixes(haystack: &[u8], needle: &[u8]) -> usize {
    let max = haystack.len().min(needle.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|len| needle.starts_with(&haystack[haystack.len() - len..]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{ReadlineDecodedInput, ReadlineTerminalInputDecoder};

    /// Verifies decoder framing preserves partial UTF-8 and escape sequences
    /// without depending on product prompt policy.
    #[test]
    fn readline_decoder_buffers_partial_sequences() {
        let mut decoder = ReadlineTerminalInputDecoder::new();
        assert_eq!(
            decoder.decode(b"a\xc3").unwrap(),
            vec![ReadlineDecodedInput::Sequence(b"a".to_vec())]
        );
        assert_eq!(decoder.pending_len(), 1);
        assert_eq!(
            decoder.decode(b"\xa9\x1b[").unwrap(),
            vec![ReadlineDecodedInput::Sequence("é".as_bytes().to_vec())]
        );
        assert_eq!(decoder.pending_len(), 2);
        assert_eq!(
            decoder.decode(b"D").unwrap(),
            vec![ReadlineDecodedInput::Sequence(b"\x1b[D".to_vec())]
        );
    }

    /// Verifies split bracketed paste payloads become one literal text item.
    #[test]
    fn readline_decoder_assembles_bracketed_paste() {
        let mut decoder = ReadlineTerminalInputDecoder::new();
        assert!(decoder.decode(b"\x1b[200~one\n").unwrap().is_empty());
        assert_eq!(
            decoder.decode(b"two\x1b[201~").unwrap(),
            vec![ReadlineDecodedInput::BracketedPaste("one\ntwo".into())]
        );
    }
}
