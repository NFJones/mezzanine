//! Finite, settings-stable ASCII clock-format capture without clock machinery.
//!
//! This neutral contract compiles printable literals and the numeric %H/%M/%S
//! directives once, before settings gain authority. Every token has a proven
//! fixed cell width; neither chrono, locale, time sampling nor I/O belongs here.
//! Screens and workers share a small immutable owner, not an inline AST or a
//! second registry. Terminal geometry reads only width; the product supplies
//! captured UTC numeric components when explicitly projecting visual rows.

use std::sync::Arc;

/// One validated fixed-width output component, exposed read-only to projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTimestampToken {
    /// One printable ASCII literal cell (including an escaped percent).
    Literal(u8),
    /// Two zero-padded hour digits in 24-hour UTC time.
    Hour,
    /// Two zero-padded minute digits.
    Minute,
    /// Two zero-padded second digits; no fractional resolution is implied.
    Second,
}

/// Private proof that tokens and decoded cell width were validated together.
#[derive(Debug, PartialEq, Eq)]
struct CompiledTimestampFormat {
    /// At most 32 tokens; numeric tokens consume two of the permitted cells.
    tokens: Box<[LogTimestampToken]>,
    /// Nonzero decoded width, bounded to 32 printable ASCII display cells.
    width: usize,
}

/// Immutable compiled format whose equality compares content, not Arc identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTimestampFormat(Arc<CompiledTimestampFormat>);

impl Default for LogTimestampFormat {
    /// Captures the original eight-cell HH:MM:SS policy without fallible parsing.
    fn default() -> Self {
        use LogTimestampToken::*;
        Self(Arc::new(CompiledTimestampFormat {
            tokens: vec![Hour, Literal(b':'), Minute, Literal(b':'), Second].into_boxed_slice(),
            width: 8,
        }))
    }
}

impl LogTimestampFormat {
    /// Compiles 1..32 decoded cells from at most 64 source bytes. Only printable
    /// ASCII literals and %H/%M/%S/%% are accepted; flags, widths, dates, fractions,
    /// control bytes and locale directives fail with a content-free diagnostic.
    pub fn compile(source: &str) -> Result<Self, &'static str> {
        if source.is_empty() || source.len() > 64 {
            return Err("format must contain 1..32 cells and at most 64 ASCII source bytes");
        }
        let mut bytes = source.bytes();
        let mut tokens = Vec::new();
        let mut width = 0;
        while let Some(byte) = bytes.next() {
            let (token, cells) = match byte {
                b'%' => match bytes.next() {
                    Some(b'H') => (LogTimestampToken::Hour, 2),
                    Some(b'M') => (LogTimestampToken::Minute, 2),
                    Some(b'S') => (LogTimestampToken::Second, 2),
                    Some(b'%') => (LogTimestampToken::Literal(b'%'), 1),
                    _ => return Err("format permits only %H, %M, %S and %% directives"),
                },
                0x20..=0x7e => (LogTimestampToken::Literal(byte), 1),
                _ => return Err("format literals must be printable ASCII without controls"),
            };
            width += cells;
            if width > 32 {
                return Err("format must render at most 32 ASCII cells");
            }
            tokens.push(token);
        }
        Ok(Self(Arc::new(CompiledTimestampFormat {
            tokens: tokens.into_boxed_slice(),
            width,
        })))
    }

    /// Returns the validated invariant display width; no per-entry measurement.
    pub fn width(&self) -> usize {
        self.0.width
    }

    /// Borrows validated tokens without recompiling or allocating during paint.
    pub fn tokens(&self) -> &[LogTimestampToken] {
        &self.0.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoded width, not source length, determines geometry. Maximum escaped
    /// percent input is valid; equal separately compiled captures compare by
    /// content, while same-width reversed numeric directives remain distinct.
    #[test]
    fn log_timestamp_format_compilation_proves_width_and_content_equality() {
        for (source, width) in [("%H:%M:%S", 8), ("%H:%M", 5), ("%%", 1), ("literal", 7)] {
            let format = LogTimestampFormat::compile(source).unwrap();
            assert_eq!(format.width(), width);
            assert_eq!(format, LogTimestampFormat::compile(source).unwrap());
        }
        assert_eq!(
            LogTimestampFormat::default(),
            LogTimestampFormat::compile("%H:%M:%S").unwrap()
        );
        assert_ne!(
            LogTimestampFormat::compile("%H:%M").unwrap(),
            LogTimestampFormat::compile("%M:%H").unwrap()
        );
        for width in [1, 31, 32] {
            assert_eq!(
                LogTimestampFormat::compile(&"a".repeat(width))
                    .unwrap()
                    .width(),
                width
            );
        }
        assert_eq!(
            LogTimestampFormat::compile(&"%%".repeat(32))
                .unwrap()
                .width(),
            32
        );
    }

    /// Unsupported grammar never reaches chrono or terminal control parsing.
    /// Byte and decoded-width limits both apply, including long escaped input;
    /// Rust strings cannot carry invalid UTF-8, and all non-ASCII scalars reject.
    #[test]
    fn log_timestamp_format_rejects_unbounded_or_unsafe_grammar() {
        for source in [
            "", "%", "%Y", "%f", "%T", "%c", "%_H", "%-H", "%3H", "%:z", "\n", "\t", "\r", "\x1b",
            "\x00", "\x7f", "é", "🙂",
        ] {
            assert!(LogTimestampFormat::compile(source).is_err());
        }
        for source in [
            "a".repeat(33),
            "%H".repeat(17),
            "%".repeat(65),
            "%%".repeat(33),
        ] {
            assert!(LogTimestampFormat::compile(&source).is_err());
        }
    }
}
