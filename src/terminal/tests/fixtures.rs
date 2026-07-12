//! Shared terminal-test assertions used by multiple behavior modules.

use mez_terminal::{TerminalColor, TerminalStyledLine};
use unicode_width::UnicodeWidthStr;

pub(super) fn display_column_for_fragment(line: &str, needle: &str) -> usize {
    let byte_index = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {line:?}"));
    UnicodeWidthStr::width(&line[..byte_index])
}

/// Returns the style active at one displayed terminal column.
///
/// # Parameters
/// - `line`: The styled terminal line to inspect.
/// - `column`: The zero-based display column within the line.
pub(super) fn styled_line_rendition_at(
    line: &TerminalStyledLine,
    column: usize,
) -> mez_terminal::GraphicRendition {
    line.style_spans
        .iter()
        .rev()
        .find(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .map(|span| span.rendition)
        .unwrap_or_default()
}

/// Returns RGB components for true-color test values.
pub(super) fn test_rgb_channels(color: TerminalColor) -> (u8, u8, u8) {
    match color {
        TerminalColor::Rgb(red, green, blue) => (red, green, blue),
        TerminalColor::Indexed(index) => panic!("expected true-color value: {index}"),
    }
}

/// Returns true when a test color is a neutral grey.
pub(super) fn test_color_is_grayscale(color: TerminalColor) -> bool {
    let (red, green, blue) = test_rgb_channels(color);
    red == green && green == blue
}

/// Returns WCAG-style contrast ratio for two true-color test values.
pub(super) fn test_contrast_ratio(foreground: TerminalColor, background: TerminalColor) -> f64 {
    let foreground_luminance = test_relative_luminance(foreground);
    let background_luminance = test_relative_luminance(background);
    let lighter = foreground_luminance.max(background_luminance);
    let darker = foreground_luminance.min(background_luminance);
    (lighter + 0.05) / (darker + 0.05)
}

/// Returns the relative luminance of a true-color test value.
pub(super) fn test_relative_luminance(color: TerminalColor) -> f64 {
    let (red, green, blue) = test_rgb_channels(color);
    0.2126 * test_srgb_channel_to_linear(red)
        + 0.7152 * test_srgb_channel_to_linear(green)
        + 0.0722 * test_srgb_channel_to_linear(blue)
}

/// Converts one sRGB channel to linear-light space for contrast assertions.
pub(super) fn test_srgb_channel_to_linear(channel: u8) -> f64 {
    let normalized = f64::from(channel) / 255.0;
    if normalized <= 0.03928 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}
