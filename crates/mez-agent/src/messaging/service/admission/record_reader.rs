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

    /// Reads a string while enforcing its decoded UTF-8 byte bound before growth.
    pub(super) fn string(&mut self, limit: usize) -> Result<String> {
        self.literal(b"\"")?;
        let mut bytes = Vec::new();
        loop {
            match self.byte()? {
                b'"' => break,
                b'\\' => self.escape(&mut bytes, limit)?,
                0..=31 => {
                    return Err(MessageError::invalid_args(
                        "MMP acceptance string contains a raw control byte",
                    ));
                }
                value => append_bounded(&mut bytes, &[value], limit)?,
            }
        }
        String::from_utf8(bytes)
            .map_err(|_| MessageError::invalid_args("MMP acceptance string is not valid UTF-8"))
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
    fn escape(&mut self, output: &mut Vec<u8>, limit: usize) -> Result<()> {
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
        let mut encoded = [0u8; 4];
        append_bounded(output, value.encode_utf8(&mut encoded).as_bytes(), limit)
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
