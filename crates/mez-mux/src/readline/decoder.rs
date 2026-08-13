//! Terminal byte decoding for multiplexer-owned readline surfaces.
//!
//! This module owns terminal sequence framing, partial-input buffering,
//! bracketed-paste assembly, and baseline readline key bindings. Product prompt
//! policy applies the decoded input to selector-aware prompt state.

use std::time::{Duration, Instant};

use super::{ReadlineBuffer, ReadlineEdit, ReadlineOutcome};
use crate::{MuxError, Result};

/// Maximum retained bytes for one readline bracketed-paste payload.
pub const READLINE_BRACKETED_PASTE_MAX_BYTES: usize = 1024 * 1024;

/// Maximum age of an incomplete readline bracketed-paste payload.
pub const READLINE_BRACKETED_PASTE_STALE_AFTER: Duration = Duration::from_millis(500);

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// Reason an incomplete or oversized bracketed paste was discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadlineBracketedPasteRejection {
    /// The payload exceeded the configured retained-byte budget.
    TooLarge {
        /// Maximum payload bytes accepted by the decoder.
        max_bytes: usize,
    },
    /// The payload remained incomplete beyond the configured age limit.
    Expired {
        /// Maximum age allowed for an incomplete payload.
        stale_after: Duration,
    },
}

/// One complete terminal input item decoded from a byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineDecodedInput {
    /// A complete control, escape, or UTF-8 text sequence.
    Sequence(Vec<u8>),
    /// One complete bracketed-paste payload.
    BracketedPaste(String),
    /// An incomplete or oversized bracketed paste was discarded.
    BracketedPasteRejected(ReadlineBracketedPasteRejection),
}

/// Stateful terminal-input decoder for readline prompt surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineTerminalInputDecoder {
    pending: Vec<u8>,
    bracketed_paste_active: bool,
    bracketed_paste: Vec<u8>,
    bracketed_paste_started_at: Option<Instant>,
    bracketed_paste_max_bytes: usize,
    bracketed_paste_stale_after: Duration,
}

impl Default for ReadlineTerminalInputDecoder {
    fn default() -> Self {
        Self::with_bracketed_paste_limits(
            READLINE_BRACKETED_PASTE_MAX_BYTES,
            READLINE_BRACKETED_PASTE_STALE_AFTER,
        )
    }
}

impl ReadlineTerminalInputDecoder {
    /// Creates a decoder with no buffered partial sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a decoder with explicit retained-byte and incomplete-age limits.
    pub fn with_bracketed_paste_limits(max_bytes: usize, stale_after: Duration) -> Self {
        Self {
            pending: Vec::new(),
            bracketed_paste_active: false,
            bracketed_paste: Vec::new(),
            bracketed_paste_started_at: None,
            bracketed_paste_max_bytes: max_bytes,
            bracketed_paste_stale_after: stale_after,
        }
    }

    /// Returns bytes retained because they may complete on a later read.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns all bytes currently retained for partial input or paste data.
    pub fn buffered_len(&self) -> usize {
        self.pending
            .len()
            .saturating_add(self.bracketed_paste.len())
    }

    /// Decodes complete input items while preserving incomplete suffixes.
    pub fn decode(&mut self, input: &[u8]) -> Result<Vec<ReadlineDecodedInput>> {
        self.decode_at(input, Instant::now())
    }

    /// Decodes input using `now` for deterministic incomplete-paste expiry.
    pub fn decode_at(&mut self, input: &[u8], now: Instant) -> Result<Vec<ReadlineDecodedInput>> {
        let mut decoded = Vec::new();
        if self.bracketed_paste_active
            && self.bracketed_paste_started_at.is_some_and(|started| {
                now.saturating_duration_since(started) >= self.bracketed_paste_stale_after
            })
        {
            self.reset_bracketed_paste();
            self.pending.clear();
            decoded.push(ReadlineDecodedInput::BracketedPasteRejected(
                ReadlineBracketedPasteRejection::Expired {
                    stale_after: self.bracketed_paste_stale_after,
                },
            ));
        }
        if input.is_empty() && self.pending.is_empty() {
            return Ok(decoded);
        }
        let mut bytes = Vec::with_capacity(self.pending.len().saturating_add(input.len()));
        bytes.extend_from_slice(&self.pending);
        bytes.extend_from_slice(input);
        self.pending.clear();

        let mut cursor = 0;
        while cursor < bytes.len() {
            if self.bracketed_paste_active {
                if let Some(end_offset) = find_bytes(&bytes[cursor..], BRACKETED_PASTE_END) {
                    if !self.bracketed_paste_fits(end_offset) {
                        self.reset_bracketed_paste();
                        decoded.push(ReadlineDecodedInput::BracketedPasteRejected(
                            ReadlineBracketedPasteRejection::TooLarge {
                                max_bytes: self.bracketed_paste_max_bytes,
                            },
                        ));
                        cursor = cursor
                            .saturating_add(end_offset)
                            .saturating_add(BRACKETED_PASTE_END.len());
                        continue;
                    }
                    self.bracketed_paste
                        .extend_from_slice(&bytes[cursor..cursor + end_offset]);
                    let payload = std::mem::take(&mut self.bracketed_paste);
                    self.bracketed_paste_active = false;
                    self.bracketed_paste_started_at = None;
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
                let payload_len = payload_end.saturating_sub(cursor);
                if !self.bracketed_paste_fits(payload_len) {
                    self.reset_bracketed_paste();
                    decoded.push(ReadlineDecodedInput::BracketedPasteRejected(
                        ReadlineBracketedPasteRejection::TooLarge {
                            max_bytes: self.bracketed_paste_max_bytes,
                        },
                    ));
                    break;
                }
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
                self.bracketed_paste_started_at = Some(now);
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

    /// Reports whether `additional_bytes` fit without overflowing the budget.
    fn bracketed_paste_fits(&self, additional_bytes: usize) -> bool {
        self.bracketed_paste
            .len()
            .checked_add(additional_bytes)
            .is_some_and(|total| total <= self.bracketed_paste_max_bytes)
    }

    /// Clears all state owned by an active bracketed paste.
    fn reset_bracketed_paste(&mut self) {
        self.bracketed_paste_active = false;
        self.bracketed_paste.clear();
        self.bracketed_paste_started_at = None;
    }
}

/// Applies one complete terminal key sequence to a readline buffer.
pub fn apply_readline_terminal_input(
    buffer: &mut ReadlineBuffer,
    input: &[u8],
) -> Result<ReadlineOutcome> {
    if let Some(key) = enhanced_key(input) {
        return Ok(apply_enhanced_key(buffer, key));
    }
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

/// Applies one parsed enhanced-key event without weakening unknown modifiers.
fn apply_enhanced_key(buffer: &mut ReadlineBuffer, key: EnhancedKey) -> ReadlineOutcome {
    if !matches!(key.event, KeyEvent::Press | KeyEvent::Repeat) || !key.modifiers.is_supported() {
        return ReadlineOutcome::Noop;
    }

    let codepoint = key.codepoint;
    let modifiers = key.modifiers;
    if matches!(codepoint, 8 | 127) {
        return match modifiers.flags {
            0 => buffer.apply(ReadlineEdit::Backspace),
            KeyModifiers::ALT | KeyModifiers::CTRL => buffer.apply(ReadlineEdit::KillWordLeft),
            _ => ReadlineOutcome::Noop,
        };
    }
    if modifiers.flags == 0 && matches!(codepoint, 10 | 13) {
        return buffer.apply(ReadlineEdit::Submit);
    }
    if modifiers.flags == KeyModifiers::ALT {
        return match char::from_u32(codepoint) {
            Some('b' | 'B') => buffer.apply(ReadlineEdit::MoveWordLeft),
            Some('f' | 'F') => buffer.apply(ReadlineEdit::MoveWordRight),
            Some('d' | 'D') => buffer.apply(ReadlineEdit::KillWordRight),
            _ => ReadlineOutcome::Noop,
        };
    }
    if modifiers.flags != KeyModifiers::CTRL
        && modifiers.flags != KeyModifiers::CTRL | KeyModifiers::SHIFT
    {
        return ReadlineOutcome::Noop;
    }

    match char::from_u32(codepoint).map(|ch| ch.to_ascii_lowercase()) {
        Some('a') => buffer.apply(ReadlineEdit::MoveHome),
        Some('b') => buffer.apply(ReadlineEdit::MoveLeft),
        Some('c') => ReadlineOutcome::Cancelled,
        Some('d') if buffer.line().is_empty() => ReadlineOutcome::Eof,
        Some('d') => buffer.apply(ReadlineEdit::DeleteForward),
        Some('e') => buffer.apply(ReadlineEdit::MoveEnd),
        Some('f') => buffer.apply(ReadlineEdit::MoveRight),
        Some('k') => buffer.apply(ReadlineEdit::KillToEnd),
        Some('n') => buffer.apply(ReadlineEdit::HistoryNext),
        Some('p') => buffer.apply(ReadlineEdit::HistoryPrevious),
        Some('r') => buffer.apply(ReadlineEdit::HistorySearchBackward),
        Some('u') => buffer.apply(ReadlineEdit::KillToStart),
        Some('v') => ReadlineOutcome::Noop,
        Some('w') => buffer.apply(ReadlineEdit::KillWordLeft),
        _ => ReadlineOutcome::Noop,
    }
}

/// Reports whether input encodes Ctrl+R.
pub fn readline_input_is_ctrl_r(input: &[u8]) -> bool {
    input == b"\x12" || readline_modified_key_is_ctrl(input, 'r')
}

/// Reports whether input encodes Ctrl+V.
pub fn readline_input_is_ctrl_v(input: &[u8]) -> bool {
    input == b"\x16" || readline_modified_key_is_ctrl(input, 'v')
}

/// Reports whether input encodes Ctrl+Shift+R.
pub fn readline_input_is_ctrl_shift_r(input: &[u8]) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl() && !modifiers.alt() && modifiers.shift() && ch == 'R'
}

fn readline_modified_key_is_ctrl(input: &[u8], target: char) -> bool {
    let Some((ch, modifiers)) = readline_modified_key(input) else {
        return false;
    };
    modifiers.ctrl() && !modifiers.alt() && ch.eq_ignore_ascii_case(&target)
}

fn readline_modified_key(input: &[u8]) -> Option<(char, KeyModifiers)> {
    let key = enhanced_key(input)?;
    if !matches!(key.event, KeyEvent::Press | KeyEvent::Repeat) || !key.modifiers.is_supported() {
        return None;
    }
    char::from_u32(key.codepoint).map(|ch| (ch, key.modifiers))
}

/// One parsed Kitty CSI-u or xterm modifyOtherKeys event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnhancedKey {
    codepoint: u32,
    alternate_codepoint: Option<u32>,
    base_layout_codepoint: Option<u32>,
    modifiers: KeyModifiers,
    event: KeyEvent,
}

/// Event phase reported by Kitty's keyboard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyEvent {
    Press,
    Repeat,
    Release,
    Unknown,
}

/// Modifier flags retained from the terminal protocol without truncation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct KeyModifiers {
    flags: u16,
}

impl KeyModifiers {
    const SHIFT: u16 = 1;
    const ALT: u16 = 2;
    const CTRL: u16 = 4;
    const SUPPORTED: u16 = Self::SHIFT | Self::ALT | Self::CTRL;

    fn is_supported(self) -> bool {
        self.flags & !Self::SUPPORTED == 0
    }

    fn shift(self) -> bool {
        self.flags & Self::SHIFT != 0
    }

    fn alt(self) -> bool {
        self.flags & Self::ALT != 0
    }

    fn ctrl(self) -> bool {
        self.flags & Self::CTRL != 0
    }
}

/// Parses the enhanced-key encodings accepted by Mez-owned readline prompts.
fn enhanced_key(input: &[u8]) -> Option<EnhancedKey> {
    if !input.starts_with(b"\x1b[") {
        return None;
    }
    let final_byte = *input.last()?;
    let params = std::str::from_utf8(&input[2..input.len().checked_sub(1)?]).ok()?;
    match final_byte {
        b'u' => parse_kitty_key(params),
        b'~' => parse_modify_other_keys(params),
        _ => None,
    }
}

/// Parses Kitty's `codepoint[:alternate...];modifiers[:event]u` parameters.
fn parse_kitty_key(params: &str) -> Option<EnhancedKey> {
    let mut fields = params.split(';');
    let mut codepoints = fields.next()?.split(':');
    let codepoint = codepoints.next()?.parse().ok()?;
    let alternate_codepoint = codepoints.next().map(str::parse).transpose().ok()?;
    let base_layout_codepoint = codepoints.next().map(str::parse).transpose().ok()?;
    if codepoints.next().is_some() {
        return None;
    }
    let modifier_field = fields.next().unwrap_or("1");
    let mut modifier_parts = modifier_field.split(':');
    let modifiers = xterm_modifier(modifier_parts.next()?.parse().ok()?)?;
    let event = match modifier_parts.next().unwrap_or("1").parse::<u8>().ok()? {
        1 => KeyEvent::Press,
        2 => KeyEvent::Repeat,
        3 => KeyEvent::Release,
        _ => KeyEvent::Unknown,
    };
    if modifier_parts.next().is_some() {
        return None;
    }
    if fields.next().is_some() {
        return None;
    }
    Some(EnhancedKey {
        codepoint,
        alternate_codepoint,
        base_layout_codepoint,
        modifiers,
        event,
    })
}

/// Parses xterm's `CSI 27;modifiers;codepoint~` form.
fn parse_modify_other_keys(params: &str) -> Option<EnhancedKey> {
    let mut fields = params.split(';');
    if fields.next()? != "27" {
        return None;
    }
    let modifiers = xterm_modifier(fields.next()?.parse().ok()?)?;
    let codepoint = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(EnhancedKey {
        codepoint,
        alternate_codepoint: None,
        base_layout_codepoint: None,
        modifiers,
        event: KeyEvent::Press,
    })
}

fn xterm_modifier(value: u16) -> Option<KeyModifiers> {
    value.checked_sub(1).map(|flags| KeyModifiers { flags })
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
    use super::{
        ReadlineBracketedPasteRejection, ReadlineDecodedInput, ReadlineTerminalInputDecoder,
        apply_readline_terminal_input,
    };
    use crate::readline::{ReadlineBuffer, ReadlineOutcome};
    use std::time::{Duration, Instant};

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

    /// Verifies the configured paste limit includes the exact boundary rather
    /// than rejecting a payload that fits within the retained-byte budget.
    #[test]
    fn readline_decoder_accepts_bracketed_paste_at_exact_limit() {
        let now = Instant::now();
        let mut decoder =
            ReadlineTerminalInputDecoder::with_bracketed_paste_limits(3, Duration::from_secs(1));

        assert_eq!(
            decoder.decode_at(b"\x1b[200~abc\x1b[201~", now).unwrap(),
            vec![ReadlineDecodedInput::BracketedPaste("abc".into())]
        );
    }

    /// Verifies fragmented paste overflow is reported explicitly, releases all
    /// retained bytes, and leaves the next ordinary key decodable.
    #[test]
    fn readline_decoder_rejects_oversized_paste_and_recovers() {
        let now = Instant::now();
        let mut decoder =
            ReadlineTerminalInputDecoder::with_bracketed_paste_limits(3, Duration::from_secs(1));

        assert!(decoder.decode_at(b"\x1b[200~abc", now).unwrap().is_empty());
        assert_eq!(
            decoder.decode_at(b"d", now).unwrap(),
            vec![ReadlineDecodedInput::BracketedPasteRejected(
                ReadlineBracketedPasteRejection::TooLarge { max_bytes: 3 }
            )]
        );
        assert_eq!(decoder.buffered_len(), 0);
        assert_eq!(
            decoder.decode_at(b"z", now).unwrap(),
            vec![ReadlineDecodedInput::Sequence(b"z".to_vec())]
        );
    }

    /// Verifies a complete oversized frame is rejected without swallowing an
    /// unambiguous ordinary suffix from the same terminal read.
    #[test]
    fn readline_decoder_preserves_suffix_after_complete_oversized_paste() {
        let now = Instant::now();
        let mut decoder =
            ReadlineTerminalInputDecoder::with_bracketed_paste_limits(3, Duration::from_secs(1));

        assert_eq!(
            decoder.decode_at(b"\x1b[200~abcd\x1b[201~z", now).unwrap(),
            vec![
                ReadlineDecodedInput::BracketedPasteRejected(
                    ReadlineBracketedPasteRejection::TooLarge { max_bytes: 3 }
                ),
                ReadlineDecodedInput::Sequence(b"z".to_vec()),
            ]
        );
    }

    /// Verifies stale unterminated paste state is reported and reset before
    /// current input is decoded, so malformed input cannot capture later keys.
    #[test]
    fn readline_decoder_expires_unterminated_paste_and_recovers() {
        let now = Instant::now();
        let stale_after = Duration::from_millis(500);
        let mut decoder =
            ReadlineTerminalInputDecoder::with_bracketed_paste_limits(32, stale_after);

        assert!(
            decoder
                .decode_at(b"\x1b[200~unterminated", now)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            decoder.decode_at(b"z", now + stale_after).unwrap(),
            vec![
                ReadlineDecodedInput::BracketedPasteRejected(
                    ReadlineBracketedPasteRejection::Expired { stale_after }
                ),
                ReadlineDecodedInput::Sequence(b"z".to_vec()),
            ]
        );
        assert_eq!(decoder.buffered_len(), 0);
    }

    /// Verifies enhanced Backspace preserves Alt and Control modifiers across
    /// Kitty CSI-u and xterm modifyOtherKeys encodings.
    #[test]
    fn enhanced_backspace_kills_the_previous_word() {
        for input in [
            b"\x1b[127;3u".as_slice(),
            b"\x1b[8;5u".as_slice(),
            b"\x1b[27;3;127~".as_slice(),
            b"\x1b[27;5;8~".as_slice(),
        ] {
            let mut buffer = ReadlineBuffer::new();
            buffer.insert_text("alpha/beta");

            assert_eq!(
                apply_readline_terminal_input(&mut buffer, input).unwrap(),
                ReadlineOutcome::Edited
            );
            assert_eq!(buffer.line(), "alpha/");
        }
    }

    /// Verifies Kitty colon subparameters accept press and repeat events while
    /// release events and unsupported modifier combinations do not edit text.
    #[test]
    fn kitty_event_types_and_modifier_combinations_are_preserved() {
        let mut buffer = ReadlineBuffer::new();
        buffer.insert_text("alpha beta");

        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[127;3:1u").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(buffer.line(), "alpha ");
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[127;3:2u").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(buffer.line(), "");

        buffer.insert_text("draft");
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[127;3:3u").unwrap(),
            ReadlineOutcome::Noop
        );
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[127;11u").unwrap(),
            ReadlineOutcome::Noop
        );
        assert_eq!(buffer.line(), "draft");
    }

    /// Verifies the stream decoder retains a fragmented Kitty CSI-u record
    /// until its final byte arrives, including alternate-codepoint fields.
    #[test]
    fn readline_decoder_buffers_fragmented_kitty_input() {
        let mut decoder = ReadlineTerminalInputDecoder::new();

        assert!(decoder.decode(b"\x1b[127:8:8;5").unwrap().is_empty());
        assert_eq!(decoder.pending_len(), 11);
        assert_eq!(
            decoder.decode(b":2u").unwrap(),
            vec![ReadlineDecodedInput::Sequence(
                b"\x1b[127:8:8;5:2u".to_vec()
            )]
        );
    }

    /// Verifies enhanced forms of readline editing controls retain the same
    /// behavior as their legacy Control and Meta byte encodings.
    #[test]
    fn enhanced_editing_controls_match_legacy_readline_bindings() {
        let mut buffer = ReadlineBuffer::new();
        buffer.insert_text("alpha beta");

        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[98;3u").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(buffer.cursor(), "alpha ".len());
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[102;3u").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(buffer.cursor(), "alpha beta".len());
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[119;5u").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(buffer.line(), "alpha ");
        assert_eq!(
            apply_readline_terminal_input(&mut buffer, b"\x1b[13u").unwrap(),
            ReadlineOutcome::Submitted(String::from("alpha "))
        );
    }

    /// Verifies malformed enhanced-key records are consumed as controls and
    /// never inserted into an editable prompt as literal text.
    #[test]
    fn malformed_enhanced_keys_are_noops() {
        let mut buffer = ReadlineBuffer::new();
        buffer.insert_text("draft");

        for input in [
            b"\x1b[127;bogus u".as_slice(),
            b"\x1b[127;3:4u".as_slice(),
            b"\x1b[127;0u".as_slice(),
            b"\x1b[127:bogus;3u".as_slice(),
            b"\x1b[127;3;99u".as_slice(),
            b"\x1b[27;3;bogus~".as_slice(),
        ] {
            assert_eq!(
                apply_readline_terminal_input(&mut buffer, input).unwrap(),
                ReadlineOutcome::Noop
            );
            assert_eq!(buffer.line(), "draft");
        }
    }
}
