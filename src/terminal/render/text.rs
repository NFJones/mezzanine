//! Terminal render text-cell and width helpers.
//!
//! This module owns low-level terminal text segmentation, display-width
//! measurement, style-span clipping, copy-selection coordinate helpers, and
//! the internal wide-glyph sentinel used by pane/window canvas rendering.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Default maximum display-cell width for Mezzanine-owned agent log rows.
pub(crate) const DEFAULT_AGENT_WRAP_COLUMN_CAP: usize = 120;

static AGENT_WRAP_COLUMN_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_AGENT_WRAP_COLUMN_CAP);

/// Selects how explicit emoji-presentation status symbols are measured in
/// terminal display cells.
use mez_terminal::TerminalEmojiWidth;

/// Applies the process-wide terminal emoji width policy.
///
/// Mezzanine uses one attached-terminal compatibility policy for all terminal
/// renderers in the process. Keeping this at the shared width helper boundary
/// ensures screen storage, pane composition, row diffing, prompt fitting, and
/// copy-mode coordinates stay aligned.
///
/// # Parameters
/// - `width`: The emoji status glyph width policy to use.
pub(crate) fn set_terminal_emoji_width(width: TerminalEmojiWidth) {
    mez_terminal::set_terminal_emoji_width(width);
}

/// Returns the active process-wide terminal emoji width policy.
pub(crate) fn terminal_emoji_width() -> TerminalEmojiWidth {
    mez_terminal::terminal_emoji_width()
}

/// Applies the process-wide maximum display width for Mezzanine-owned agent rows.
///
/// # Parameters
/// - `columns`: The positive display-cell cap to use for agent transcript rows.
pub(crate) fn set_agent_wrap_column_cap(columns: usize) {
    AGENT_WRAP_COLUMN_CAP.store(columns.max(1), Ordering::Relaxed);
}

/// Returns the process-wide maximum display width for Mezzanine-owned agent rows.
pub(crate) fn agent_wrap_column_cap() -> usize {
    AGENT_WRAP_COLUMN_CAP.load(Ordering::Relaxed).max(1)
}

/// Returns the bounded display width used for Mezzanine-owned agent log rows.
pub(crate) fn agent_log_wrap_width(terminal_width: u16) -> usize {
    usize::from(terminal_width).clamp(1, agent_wrap_column_cap())
}

/// Word-wraps one Mezzanine-owned agent log text block for terminal display.
///
/// Explicit newlines are preserved as row breaks. Individual logical rows wrap
/// at the nearest whitespace boundary before the display-cell limit, falling
/// back to hard grapheme boundaries when an unbroken token exceeds the limit.
pub(crate) fn wrap_agent_log_text(value: &str, terminal_width: u16) -> Vec<String> {
    mez_mux::render::wrap_text(value, agent_log_wrap_width(terminal_width))
}

/// Word-wraps Mezzanine-owned agent log rows for terminal display.
pub(crate) fn wrap_agent_log_lines(lines: &[String], terminal_width: u16) -> Vec<String> {
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_agent_log_text(line, terminal_width));
    }
    wrapped
}

/// Returns the display width of one Unicode grapheme cluster.
///
/// Terminal renderers display each grapheme cluster in a single cell span of
/// zero, one, or two columns even when a multi-scalar cluster contains emoji
/// or combining scalars whose Unicode widths would sum to a larger number.
///
/// # Parameters
/// - `grapheme`: The extended grapheme cluster to measure.
pub(crate) fn terminal_grapheme_width(grapheme: &str) -> usize {
    terminal_grapheme_width_for_emoji_width(grapheme, terminal_emoji_width())
}

/// Returns the display width of one Unicode grapheme cluster under an explicit
/// emoji status glyph width policy.
///
/// # Parameters
/// - `grapheme`: The extended grapheme cluster to measure.
/// - `emoji_width`: The compatibility policy selected for status glyphs.
fn terminal_grapheme_width_for_emoji_width(
    grapheme: &str,
    emoji_width: TerminalEmojiWidth,
) -> usize {
    mez_terminal::terminal_grapheme_width(grapheme, emoji_width)
}

/// Returns the display width of one complete terminal string.
///
/// # Parameters
/// - `value`: The terminal text to measure.
pub(crate) fn terminal_text_width(value: &str) -> usize {
    mez_terminal::terminal_text_width(value, terminal_emoji_width())
}

/// Returns an iterator over Unicode grapheme clusters in terminal text.
///
/// # Parameters
/// - `value`: The terminal text to segment.
pub(crate) fn terminal_graphemes(value: &str) -> impl Iterator<Item = &str> {
    mez_terminal::terminal_graphemes(value)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_AGENT_WRAP_COLUMN_CAP, TerminalEmojiWidth, agent_log_wrap_width,
        set_agent_wrap_column_cap, terminal_grapheme_width_for_emoji_width, terminal_text_width,
        wrap_agent_log_text,
    };

    /// Verifies agent log wrapping uses the pane width until the default cap
    /// applies, so very wide terminals do not create unbounded transcript rows.
    #[test]
    fn agent_log_wrap_width_caps_terminal_width_at_default_columns() {
        set_agent_wrap_column_cap(DEFAULT_AGENT_WRAP_COLUMN_CAP);

        assert_eq!(agent_log_wrap_width(0), 1);
        assert_eq!(agent_log_wrap_width(80), 80);
        assert_eq!(agent_log_wrap_width(200), DEFAULT_AGENT_WRAP_COLUMN_CAP);
    }

    /// Verifies the process-wide agent row cap controls the maximum wrap width.
    ///
    /// Runtime config applies this shared cap before transcript rows are rendered
    /// or persisted, so the low-level wrapper must stop using a fixed constant.
    #[test]
    fn agent_log_wrap_width_uses_configured_column_cap() {
        set_agent_wrap_column_cap(96);

        assert_eq!(agent_log_wrap_width(200), 96);

        set_agent_wrap_column_cap(DEFAULT_AGENT_WRAP_COLUMN_CAP);
    }

    /// Verifies multi-scalar terminal emoji grapheme clusters keep their
    /// rendered two-cell width so pane row accounting does not overcount
    /// modifier and regional-indicator sequences.
    #[test]
    fn terminal_text_width_keeps_terminal_emoji_clusters_at_two_cells() {
        for grapheme in ["👍🏻", "👍🏼", "👍🏽", "👍🏾", "🇪🇺", "🇯🇵", "🇧🇷", "🇨🇦"]
        {
            assert_eq!(super::terminal_grapheme_width(grapheme), 2, "{grapheme}");
            assert_eq!(terminal_text_width(grapheme), 2, "{grapheme}");
        }
    }

    /// Verifies mixed fullwidth text and multi-scalar emoji clusters still sum
    /// to the correct terminal row width after cluster widths are normalized.
    #[test]
    fn terminal_text_width_counts_mixed_fullwidth_text_and_emoji_clusters() {
        assert_eq!(terminal_text_width("ｓ 👍🏻 🇪🇺"), 8);
    }

    /// Verifies the wide terminal emoji-width compatibility policy does not
    /// widen bare emoji-capable text symbols unless the rendered cluster asks
    /// for emoji presentation. This protects subsequent table separators and
    /// pane dividers from one-cell cursor drift on text-fallback terminals.
    #[test]
    fn terminal_text_width_wide_policy_keeps_bare_status_symbols_narrow() {
        for grapheme in ["↗", "✔", "⚠"] {
            assert_eq!(
                terminal_grapheme_width_for_emoji_width(grapheme, TerminalEmojiWidth::Wide),
                grapheme.chars().count(),
                "{grapheme}"
            );
        }

        assert_eq!(terminal_text_width("↗ Positive  │"), 13);
    }

    /// Verifies the wide terminal emoji-width compatibility policy still
    /// measures explicit emoji-presentation status glyphs with the Unicode
    /// two-cell width used by emoji-capable terminal renderers.
    #[test]
    fn terminal_text_width_wide_policy_counts_explicit_status_emoji_as_two_cells() {
        for grapheme in ["↗️", "✔️", "⚠️"] {
            assert_eq!(
                terminal_grapheme_width_for_emoji_width(grapheme, TerminalEmojiWidth::Wide),
                2,
                "{grapheme}"
            );
        }
    }

    /// Verifies the narrow terminal emoji-width compatibility policy measures
    /// simple emoji/text status glyphs as one cell when a host terminal renders
    /// them through text fallback fonts. This directly covers the status marks
    /// that otherwise leave pane dividers and following text shifted by one
    /// display cell on one-cell fallback terminals.
    #[test]
    fn terminal_text_width_narrow_policy_counts_status_glyph_fallbacks_as_one_cell() {
        for grapheme in ["✅", "✅︎", "⚠", "⚠️", "⚠︎", "✔", "✔️", "✔︎"] {
            assert_eq!(
                terminal_grapheme_width_for_emoji_width(grapheme, TerminalEmojiWidth::Narrow),
                1,
                "{grapheme}"
            );
        }
    }

    /// Verifies the narrow status-glyph compatibility policy does not collapse
    /// non-status emoji or complex emoji clusters such as skin-tone modifiers,
    /// regional-indicator flags, and ZWJ emoji. Those clusters still occupy one
    /// two-cell terminal span in terminals that render them successfully.
    #[test]
    fn terminal_text_width_narrow_policy_keeps_complex_emoji_clusters_wide() {
        for grapheme in ["👍", "👍🏻", "🇪🇺", "👨‍💻", "1️⃣"] {
            assert_eq!(
                terminal_grapheme_width_for_emoji_width(grapheme, TerminalEmojiWidth::Narrow),
                2,
                "{grapheme}"
            );
        }
    }

    /// Verifies the 120-column cap is applied even when the active pane is
    /// wider, protecting persisted replay rows from host-width drift.
    #[test]
    fn wrap_agent_log_text_applies_global_column_cap() {
        let wrapped = wrap_agent_log_text(&"x".repeat(130), 200);

        assert_eq!(terminal_text_width(&wrapped[0]), 120);
        assert_eq!(terminal_text_width(&wrapped[1]), 10);
    }
}
