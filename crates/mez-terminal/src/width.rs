//! Unicode segmentation and display-width measurement for terminal cells.
//!
//! Terminal screen storage and multiplexer presentation must agree on grapheme
//! boundaries and cell widths. This module owns that shared one-surface
//! compatibility contract while leaving process-wide configuration and
//! presentation-specific fitting in the product crate.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use std::sync::atomic::{AtomicU8, Ordering};

/// Selects how explicit emoji-presentation status symbols are measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalEmojiWidth {
    /// Use the Unicode terminal width used by emoji-capable terminals.
    #[default]
    Wide,
    /// Use one-cell text fallback widths for simple status symbols.
    Narrow,
}

static TERMINAL_EMOJI_WIDTH: AtomicU8 = AtomicU8::new(0);

/// Applies the process-wide terminal emoji-width compatibility policy.
pub fn set_terminal_emoji_width(width: TerminalEmojiWidth) {
    TERMINAL_EMOJI_WIDTH.store(
        u8::from(width == TerminalEmojiWidth::Narrow),
        Ordering::Relaxed,
    );
}

/// Returns the active process-wide terminal emoji-width compatibility policy.
pub fn terminal_emoji_width() -> TerminalEmojiWidth {
    match TERMINAL_EMOJI_WIDTH.load(Ordering::Relaxed) {
        1 => TerminalEmojiWidth::Narrow,
        _ => TerminalEmojiWidth::Wide,
    }
}

/// Returns the terminal display width of one Unicode scalar.
pub fn terminal_char_width(ch: char, emoji_width: TerminalEmojiWidth) -> usize {
    if emoji_width == TerminalEmojiWidth::Narrow && terminal_scalar_has_text_fallback_width(ch) {
        return 1;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Returns the display width of one extended grapheme cluster.
pub fn terminal_grapheme_width(grapheme: &str, emoji_width: TerminalEmojiWidth) -> usize {
    if emoji_width == TerminalEmojiWidth::Narrow
        && let Some(width) = narrow_text_fallback_grapheme_width(grapheme)
    {
        return width;
    }
    let mut chars = grapheme.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
    {
        return terminal_char_width(ch, emoji_width);
    }
    UnicodeWidthStr::width(grapheme).min(2)
}

/// Returns the display width of one complete terminal string.
pub fn terminal_text_width(value: &str, emoji_width: TerminalEmojiWidth) -> usize {
    terminal_graphemes(value)
        .map(|grapheme| terminal_grapheme_width(grapheme, emoji_width))
        .sum()
}

/// Returns an iterator over extended grapheme clusters in terminal text.
pub fn terminal_graphemes(value: &str) -> impl Iterator<Item = &str> {
    UnicodeSegmentation::graphemes(value, true)
}

fn terminal_scalar_has_text_fallback_width(ch: char) -> bool {
    if ch.is_ascii() || !terminal_scalar_is_text_fallback_status_symbol(ch) {
        return false;
    }
    let text_presentation = format!("{ch}\u{FE0E}");
    UnicodeWidthStr::width(text_presentation.as_str()) == 1
}

fn terminal_scalar_is_text_fallback_status_symbol(ch: char) -> bool {
    matches!(
        ch as u32,
        0x203C
            | 0x2049
            | 0x2139
            | 0x2194..=0x21AA
            | 0x231A..=0x23FF
            | 0x24C2
            | 0x24D8
            | 0x25AA..=0x25FE
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B00..=0x2BFF
            | 0x3030
            | 0x303D
            | 0x3297..=0x3299
    )
}

fn narrow_text_fallback_grapheme_width(grapheme: &str) -> Option<usize> {
    let mut chars = grapheme.chars();
    let base = chars.next()?;
    let variation = chars.next()?;
    if chars.next().is_some() || !matches!(variation, '\u{FE0E}' | '\u{FE0F}') {
        return None;
    }
    Some(terminal_char_width(base, TerminalEmojiWidth::Narrow))
}

#[cfg(test)]
mod tests {
    use super::{
        TerminalEmojiWidth, terminal_char_width, terminal_grapheme_width, terminal_graphemes,
        terminal_text_width,
    };

    /// Verifies segmentation preserves multi-scalar terminal glyphs as one
    /// grapheme so screen storage cannot split combining or emoji sequences.
    #[test]
    fn segments_terminal_text_into_extended_graphemes() {
        assert_eq!(
            terminal_graphemes("e\u{301}👍🏻").collect::<Vec<_>>(),
            ["e\u{301}", "👍🏻"]
        );
    }

    /// Verifies the wide policy normalizes complex emoji clusters to two cells
    /// while retaining combining text as a single display cell.
    #[test]
    fn measures_terminal_graphemes_with_wide_policy() {
        assert_eq!(terminal_grapheme_width("👍🏻", TerminalEmojiWidth::Wide), 2);
        assert_eq!(
            terminal_grapheme_width("e\u{301}", TerminalEmojiWidth::Wide),
            1
        );
        assert_eq!(terminal_text_width("ｓ 👍🏻", TerminalEmojiWidth::Wide), 5);
    }

    /// Verifies the narrow policy affects simple status-symbol fallbacks but
    /// does not collapse complex emoji clusters to one cell.
    #[test]
    fn measures_status_symbols_with_narrow_policy() {
        assert_eq!(terminal_char_width('✅', TerminalEmojiWidth::Narrow), 1);
        assert_eq!(terminal_grapheme_width("⚠️", TerminalEmojiWidth::Narrow), 1);
        assert_eq!(terminal_grapheme_width("👨‍💻", TerminalEmojiWidth::Narrow), 2);
    }
}
