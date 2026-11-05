//! Bounded primitives for decoding the private canonical acceptance format.
//!
//! The admission encoder writes compact JSON in a fixed field order. These
//! primitives read that exact grammar incrementally, bound decoded strings
//! before growing their buffers, and reject malformed UTF-8 and escapes. They
//! provide no receipt, registration, delivery, or restore authority: a caller
//! must validate the entire versioned record and the enclosing committed source
//! before using any decoded data. Storage I/O belongs on the owned worker.

use std::io::{BufRead, BufReader, Read};

use super::{MessageError, Result};
use sha2::{Digest, Sha256};

/// Validated decoded payload length and SHA256, without a retained body.
/// These descriptive bytes never authorize delivery or reconstruct a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePayloadFingerprint {
    bytes: usize,
    sha256: [u8; 32],
}
impl MessagePayloadFingerprint {
    /// Returns the exact decoded UTF-8 length, excluding JSON escape syntax.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Returns SHA256 of the exact validated decoded UTF-8 payload bytes.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Bounded buffered cursor over one private encoded acceptance source.
pub(super) struct RecordReader<R> {
    input: BufReader<R>,
}

impl<R: Read> RecordReader<R> {
    /// Creates a cursor without loading the complete encoded record.
    pub(super) fn new(input: R) -> Self {
        Self {
            input: BufReader::with_capacity(8 * 1024, input),
        }
    }

    /// Peeks at one byte, preserving it for the next parsing operation.
    pub(super) fn peek(&mut self) -> Result<Option<u8>> {
        self.input
            .fill_buf()
            .map(|bytes| bytes.first().copied())
            .map_err(|error| {
                MessageError::invalid_state(format!("MMP acceptance read failed: {error}"))
            })
    }

    /// Reads one required byte or reports a truncated source.
    fn byte(&mut self) -> Result<u8> {
        let value = self
            .peek()?
            .ok_or_else(|| MessageError::invalid_args("MMP acceptance record is truncated"))?;
        self.input.consume(1);
        Ok(value)
    }

    /// Requires exact canonical punctuation or field spelling without allocation.
    pub(super) fn literal(&mut self, expected: &[u8]) -> Result<()> {
        for expected_byte in expected {
            if self.byte()? != *expected_byte {
                return Err(MessageError::invalid_args(
                    "MMP acceptance record framing is invalid",
                ));
            }
        }
        Ok(())
    }

    /// Reads one canonical nonnegative integer, rejecting overflow and leading zeros.
    pub(super) fn unsigned(&mut self) -> Result<u64> {
        let first = self.byte()?;
        if !first.is_ascii_digit() {
            return Err(MessageError::invalid_args(
                "MMP acceptance integer is invalid",
            ));
        }
        let mut value = u64::from(first - b'0');
        while let Some(next @ b'0'..=b'9') = self.peek()? {
            if first == b'0' {
                return Err(MessageError::invalid_args(
                    "MMP acceptance integer is not canonical",
                ));
            }
            self.input.consume(1);
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(next - b'0')))
                .ok_or_else(|| MessageError::invalid_args("MMP acceptance integer overflow"))?;
        }
        Ok(value)
    }

    /// Reads an optional integer without treating a malformed value as absent.
    pub(super) fn optional_unsigned(&mut self) -> Result<Option<u64>> {
        if self.peek()? == Some(b'n') {
            self.literal(b"null")?;
            Ok(None)
        } else {
            self.unsigned().map(Some)
        }
    }

    /// Retains the same bounded decoded string used by the metadata-only reader.
    pub(super) fn string(&mut self, limit: usize) -> Result<String> {
        let mut output = Vec::new();
        self.string_chunks(limit, |bytes| append_bounded(&mut output, bytes, limit))?;
        // The shared parser checked every UTF-8 fragment before emitting it.
        String::from_utf8(output).map_err(|_| invalid_utf8())
    }

    /// Validates and hashes a payload using one fixed 8-KiB buffer. There is no
    /// body-sized allocation, empty-envelope substitute or unchecked skip path.
    pub(super) fn payload_fingerprint(
        &mut self,
        limit: usize,
    ) -> Result<MessagePayloadFingerprint> {
        let mut hash = Sha256::new();
        let bytes = self.string_chunks(limit, |bytes| {
            hash.update(bytes);
            Ok(())
        })?;
        Ok(MessagePayloadFingerprint {
            bytes,
            sha256: hash.finalize().into(),
        })
    }

    /// Shares exact JSON escapes, decoded-byte limits and UTF-8 validation with
    /// both consumers. A fragment boundary may retain at most three UTF-8 bytes;
    /// malformed bytes, incomplete scalars and read errors withhold the result.
    fn string_chunks(
        &mut self,
        limit: usize,
        mut emit: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<usize> {
        self.literal(b"\"")?;
        let mut buffer = [0u8; 8192];
        let mut used = 0usize;
        let mut total = 0usize;
        loop {
            let mut scalar = [0u8; 4];
            let bytes: &[u8] = match self.byte()? {
                b'"' => break,
                b'\\' => self.escape()?.encode_utf8(&mut scalar).as_bytes(),
                0..=31 => {
                    return Err(MessageError::invalid_args(
                        "MMP acceptance string contains a raw control byte",
                    ));
                }
                value => {
                    scalar[0] = value;
                    &scalar[..1]
                }
            };
            total = total
                .checked_add(bytes.len())
                .filter(|length| *length <= limit)
                .ok_or_else(|| {
                    MessageError::invalid_args("MMP acceptance decoded string exceeds its budget")
                })?;
            for byte in bytes {
                buffer[used] = *byte;
                used += 1;
                if used == buffer.len() {
                    used = emit_utf8(&mut buffer, used, false, &mut emit)?;
                }
            }
        }
        emit_utf8(&mut buffer, used, true, &mut emit)?;
        Ok(total)
    }

    /// Reads either a bounded string or an explicit JSON null.
    pub(super) fn optional_string(&mut self, limit: usize) -> Result<Option<String>> {
        if self.peek()? == Some(b'n') {
            self.literal(b"null")?;
            Ok(None)
        } else {
            self.string(limit).map(Some)
        }
    }

    /// Decodes one JSON escape, including a complete UTF-16 surrogate pair.
    fn escape(&mut self) -> Result<char> {
        let value = match self.byte()? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = self.hex_quad()?;
                let scalar = match first {
                    0xd800..=0xdbff => {
                        self.literal(b"\\u")?;
                        let second = self.hex_quad()?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err(MessageError::invalid_args(
                                "MMP acceptance string has an invalid surrogate pair",
                            ));
                        }
                        0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                    }
                    0xdc00..=0xdfff => {
                        return Err(MessageError::invalid_args(
                            "MMP acceptance string has an unpaired surrogate",
                        ));
                    }
                    value => u32::from(value),
                };
                char::from_u32(scalar).ok_or_else(|| {
                    MessageError::invalid_args(
                        "MMP acceptance string has an invalid Unicode scalar",
                    )
                })?
            }
            _ => {
                return Err(MessageError::invalid_args(
                    "MMP acceptance string escape is invalid",
                ));
            }
        };
        Ok(value)
    }

    /// Reads four ASCII hexadecimal digits as one UTF-16 code unit.
    fn hex_quad(&mut self) -> Result<u16> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = match self.byte()? {
                value @ b'0'..=b'9' => value - b'0',
                value @ b'a'..=b'f' => value - b'a' + 10,
                value @ b'A'..=b'F' => value - b'A' + 10,
                _ => {
                    return Err(MessageError::invalid_args(
                        "MMP acceptance Unicode escape is invalid",
                    ));
                }
            };
            value = (value << 4) | u16::from(digit);
        }
        Ok(value)
    }

    /// Rejects any trailing bytes, including bytes already read into the buffer.
    pub(super) fn finish(&mut self) -> Result<()> {
        if self.peek()?.is_some() {
            return Err(MessageError::invalid_args(
                "MMP acceptance record has trailing input",
            ));
        }
        Ok(())
    }
}

/// Appends only after checking the decoded budget and reserving bounded space.
fn append_bounded(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<()> {
    let needed = output
        .len()
        .checked_add(bytes.len())
        .filter(|needed| *needed <= limit)
        .ok_or_else(|| {
            MessageError::invalid_args("MMP acceptance string exceeds its decoded byte budget")
        })?;
    if needed > output.capacity() {
        let target = output
            .capacity()
            .saturating_mul(2)
            .max(128)
            .max(needed)
            .min(limit);
        output
            .try_reserve_exact(target - output.len())
            .map_err(|_| MessageError::invalid_state("MMP acceptance string allocation failed"))?;
    }
    output.extend_from_slice(bytes);
    Ok(())
}

/// Emits complete valid scalars and carries an incomplete suffix across chunks.
fn emit_utf8(
    buffer: &mut [u8; 8192],
    used: usize,
    final_chunk: bool,
    emit: &mut impl FnMut(&[u8]) -> Result<()>,
) -> Result<usize> {
    let valid = match std::str::from_utf8(&buffer[..used]) {
        Ok(_) => used,
        Err(error) if !final_chunk && error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return Err(invalid_utf8()),
    };
    emit(&buffer[..valid])?;
    buffer.copy_within(valid..used, 0);
    Ok(used - valid)
}
fn invalid_utf8() -> MessageError {
    MessageError::invalid_args("MMP acceptance string is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use super::RecordReader;

    /// The decoder consumes the encoder's escaped string representation without
    /// changing controls, Unicode, or exact decoded bytes. Its decoded-byte
    /// budget is inclusive and does not count JSON escape syntax as content.
    #[test]
    fn bounded_string_preserves_encoded_content_and_exact_limit() {
        let original = "plain \"quoted\" \\ slash\n\t\0 é 😀";
        let encoded = serde_json::to_vec(original).unwrap();
        let mut reader = RecordReader::new(encoded.as_slice());
        assert_eq!(reader.string(original.len()).unwrap(), original);
        reader.finish().unwrap();

        let mut too_small = RecordReader::new(encoded.as_slice());
        assert!(too_small.string(original.len() - 1).is_err());
    }

    /// Escaped UTF-16 pairs produce the same four UTF-8 bytes as a literal
    /// scalar, while lone or mismatched surrogates cannot enter decoded state.
    #[test]
    fn bounded_string_validates_unicode_surrogates_and_utf8() {
        let mut reader = RecordReader::new(br#""\uD83D\uDE00""#.as_slice());
        assert_eq!(reader.string(4).unwrap(), "😀");
        reader.finish().unwrap();
        for invalid in [
            br#""\uD83D""#.as_slice(),
            br#""\uDE00""#.as_slice(),
            br#""\uD83D\u0041""#.as_slice(),
            br#""\uZZZZ""#.as_slice(),
            b"\"\xff\"".as_slice(),
            b"\"line\nfeed\"".as_slice(),
            br#""\x00""#.as_slice(),
        ] {
            assert!(
                RecordReader::new(invalid).string(32).is_err(),
                "{invalid:?}"
            );
        }
    }

    /// Malformed numbers, truncated strings and extra bytes fail closed. The
    /// end check must inspect read-ahead bytes, not merely the source cursor.
    #[test]
    fn canonical_numbers_and_full_consumption_are_checked() {
        let mut reader = RecordReader::new(b"0,18446744073709551615,null".as_slice());
        assert_eq!(reader.unsigned().unwrap(), 0);
        reader.literal(b",").unwrap();
        assert_eq!(reader.unsigned().unwrap(), u64::MAX);
        reader.literal(b",").unwrap();
        assert_eq!(reader.optional_unsigned().unwrap(), None);
        reader.finish().unwrap();
        for invalid in ["01", "18446744073709551616", "-1", "", "nullx"] {
            let mut reader = RecordReader::new(invalid.as_bytes());
            assert!(
                reader
                    .optional_unsigned()
                    .and_then(|_| reader.finish())
                    .is_err(),
                "{invalid}"
            );
        }
        assert!(
            RecordReader::new(b"\"incomplete".as_slice())
                .string(100)
                .is_err()
        );
        let mut trailing = RecordReader::new(b"\"ok\" ".as_slice());
        assert_eq!(trailing.string(2).unwrap(), "ok");
        assert!(trailing.finish().is_err());
    }

    /// UTF-8 scalars may straddle the fixed output fragment. Literal and escaped
    /// scalars hash identically after decoding; invalid cross-fragment UTF-8,
    /// incomplete sequences and invalid surrogates never yield a fingerprint.
    #[test]
    fn payload_fingerprint_checks_utf8_at_fragment_edges_and_escape_aliases() {
        use sha2::{Digest, Sha256};
        for offset in [8189, 8190, 8191, 8192] {
            let text = format!("{}😀é\0\n\"", "a".repeat(offset));
            let encoded = serde_json::to_vec(&text).unwrap();
            let value = RecordReader::new(encoded.as_slice())
                .payload_fingerprint(text.len())
                .unwrap();
            assert_eq!(value.bytes(), text.len());
            assert_eq!(
                value.sha256(),
                &<[u8; 32]>::from(Sha256::digest(text.as_bytes()))
            );
            let mut invalid = Vec::new();
            invalid.push(b'"');
            invalid.extend(std::iter::repeat_n(b'a', offset));
            invalid.extend_from_slice(&[0xf0, 0x9f, b'"']);
            assert!(
                RecordReader::new(invalid.as_slice())
                    .payload_fingerprint(16384)
                    .is_err()
            );
        }
        let literal = RecordReader::new("\"😀\"".as_bytes())
            .payload_fingerprint(4)
            .unwrap();
        let escaped = RecordReader::new(br#""\uD83D\uDE00""#.as_slice())
            .payload_fingerprint(4)
            .unwrap();
        assert_eq!(literal, escaped);
        for bad in [
            br#""\uD83D""#.as_slice(),
            br#""\uDE00""#.as_slice(),
            b"\"\xff\"",
            b"\"x\n\"",
        ] {
            assert!(RecordReader::new(bad).payload_fingerprint(100).is_err());
        }
    }

    /// Short underlying reads and a physical read error are observed rather
    /// than converted into an apparently complete accepted source or value.
    #[test]
    fn reader_handles_short_reads_and_propagates_source_failure() {
        struct ShortRead<'a> {
            bytes: &'a [u8],
            fail_at_end: bool,
        }
        impl Read for ShortRead<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.bytes.is_empty() {
                    return if self.fail_at_end {
                        Err(io::Error::other("fixture read error"))
                    } else {
                        Ok(0)
                    };
                }
                if output.is_empty() {
                    return Ok(0);
                }
                output[0] = self.bytes[0];
                self.bytes = &self.bytes[1..];
                Ok(1)
            }
        }
        let mut reader = RecordReader::new(ShortRead {
            bytes: br#""\uD83D\uDE00""#,
            fail_at_end: false,
        });
        assert_eq!(reader.string(4).unwrap(), "😀");
        reader.finish().unwrap();
        let mut failed = RecordReader::new(ShortRead {
            bytes: b"\"prefix",
            fail_at_end: true,
        });
        assert!(
            failed
                .string(32)
                .unwrap_err()
                .to_string()
                .contains("fixture read error")
        );
    }
}
