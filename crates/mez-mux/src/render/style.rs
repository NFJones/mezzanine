//! Terminal render color and style-span helpers.
//!
//! This module owns pure theme-derived color math, active-status scan palette
//! generation, contrast helpers, and style-span coalescing used by mux frame,
//! footer, and prompt rendering. It does not choose product fields, actions,
//! animation timing, or host output policy.

use mez_terminal::{TerminalColor, TerminalStyleSpan};

use crate::theme::UiTheme;

/// Returns a restrained primary-tinted ramp for active status animation.
///
/// Running status pills use the same quiet container as other ordinary agent
/// states. Blending progressively small amounts of the running foreground into
/// that container adds motion and theme identity without restoring a saturated
/// full-pill background.
pub fn agent_status_running_gradient_palette(ui_theme: &UiTheme) -> [TerminalColor; 3] {
    let base = ui_theme.colors.agent_status_running.background;
    let accent = ui_theme.colors.agent_status_running.foreground;
    [
        blend_terminal_color(base, accent, 1, 8),
        blend_terminal_color(base, accent, 2, 8),
        blend_terminal_color(base, accent, 3, 8),
    ]
}

/// Chooses a scan highlight from the left, center, or right side of a ramp.
pub fn gradient_highlight_for_offset(palette: &[TerminalColor; 3], offset: isize) -> TerminalColor {
    if offset < -1 {
        palette[0]
    } else if offset > 1 {
        palette[2]
    } else {
        palette[1]
    }
}

/// Blends one scan-band cell between base and highlight colors.
pub fn animated_scan_background(
    base: TerminalColor,
    highlight: TerminalColor,
    intensity: usize,
    max_intensity: usize,
) -> TerminalColor {
    let numerator = intensity.min(max_intensity) as u16;
    let denominator = max_intensity.max(1) as u16;
    blend_terminal_color(base, highlight, numerator, denominator)
}

/// Returns RGB components for a true-color value.
pub fn terminal_color_rgb(color: TerminalColor) -> Option<(u8, u8, u8)> {
    match color {
        TerminalColor::Rgb(red, green, blue) => Some((red, green, blue)),
        TerminalColor::Indexed(_) => None,
    }
}

/// Returns a simple perceptual luminance approximation for true-color values.
pub fn terminal_color_luminance(color: TerminalColor) -> Option<u32> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some((u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000)
}

/// Returns the WCAG-style contrast ratio between two true-color values.
pub fn terminal_color_contrast_ratio(
    foreground: TerminalColor,
    background: TerminalColor,
) -> Option<f64> {
    let foreground_luminance = terminal_color_relative_luminance(foreground)?;
    let background_luminance = terminal_color_relative_luminance(background)?;
    let lighter = foreground_luminance.max(background_luminance);
    let darker = foreground_luminance.min(background_luminance);
    Some((lighter + 0.05) / (darker + 0.05))
}

/// Returns the relative luminance of a true-color value.
pub fn terminal_color_relative_luminance(color: TerminalColor) -> Option<f64> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some(
        0.2126 * srgb_channel_to_linear(red)
            + 0.7152 * srgb_channel_to_linear(green)
            + 0.0722 * srgb_channel_to_linear(blue),
    )
}

/// Converts one sRGB channel to linear-light space.
pub fn srgb_channel_to_linear(channel: u8) -> f64 {
    let normalized = f64::from(channel) / 255.0;
    if normalized <= 0.03928 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}

/// Chooses black or white text for one themed background.
pub fn contrasting_binary_foreground(background: TerminalColor) -> TerminalColor {
    match terminal_color_luminance(background) {
        Some(luminance) if luminance >= 140 => TerminalColor::Rgb(0x00, 0x00, 0x00),
        Some(_) | None => TerminalColor::Rgb(0xff, 0xff, 0xff),
    }
}

/// Blends two true-color values, falling back to the base for indexed colors.
pub fn blend_terminal_color(
    base: TerminalColor,
    highlight: TerminalColor,
    numerator: u16,
    denominator: u16,
) -> TerminalColor {
    let Some((base_r, base_g, base_b)) = terminal_color_rgb(base) else {
        return base;
    };
    let Some((highlight_r, highlight_g, highlight_b)) = terminal_color_rgb(highlight) else {
        return base;
    };
    let denominator = denominator.max(1);
    TerminalColor::Rgb(
        blend_channel(base_r, highlight_r, numerator, denominator),
        blend_channel(base_g, highlight_g, numerator, denominator),
        blend_channel(base_b, highlight_b, numerator, denominator),
    )
}

/// Builds a quiet neutral context-usage background from a frame surface.
pub fn neutral_surface_step(surface: TerminalColor) -> TerminalColor {
    let Some((red, green, blue)) = terminal_color_rgb(surface) else {
        return surface;
    };
    let luminance = terminal_color_luminance(surface).unwrap_or(0);
    let shift = if luminance >= 140 { -28 } else { 34 };
    TerminalColor::Rgb(
        shifted_channel(red, shift),
        shifted_channel(green, shift),
        shifted_channel(blue, shift),
    )
}

/// Shifts a color channel by a signed amount.
pub fn shifted_channel(value: u8, shift: i32) -> u8 {
    (i32::from(value) + shift).clamp(0, 255) as u8
}

/// Linearly blends one color channel with integer arithmetic.
fn blend_channel(base: u8, highlight: u8, numerator: u16, denominator: u16) -> u8 {
    let base = u16::from(base);
    let highlight = u16::from(highlight);
    let value = base
        .saturating_mul(denominator.saturating_sub(numerator))
        .saturating_add(highlight.saturating_mul(numerator))
        / denominator.max(1);
    value.min(u16::from(u8::MAX)) as u8
}

/// Appends a span, merging with the previous span when possible.
pub fn push_or_extend_style_span(spans: &mut Vec<TerminalStyleSpan>, span: TerminalStyleSpan) {
    if span.length == 0 {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.start.saturating_add(last.length) == span.start
        && last.rendition == span.rendition
    {
        last.length = last.length.saturating_add(span.length);
        return;
    }
    spans.push(span);
}

#[cfg(test)]
mod tests {
    use mez_terminal::GraphicRendition;

    use super::*;

    /// Verifies active-status animation remains a restrained wash of the
    /// semantic running accent over the quiet status container. The strongest
    /// stop must remain short of the raw accent so animation does not recreate
    /// a saturated persistent pill.
    #[test]
    fn running_gradient_blends_primary_foreground_into_quiet_container() {
        let theme = crate::theme::default_ui_theme();
        let base = theme.colors.agent_status_running.background;
        let accent = theme.colors.agent_status_running.foreground;

        assert_eq!(
            agent_status_running_gradient_palette(&theme),
            [
                blend_terminal_color(base, accent, 1, 8),
                blend_terminal_color(base, accent, 2, 8),
                blend_terminal_color(base, accent, 3, 8),
            ]
        );
        assert_ne!(agent_status_running_gradient_palette(&theme)[2], accent);
    }

    /// Verifies true-color blending and indexed-color fallback remain stable
    /// when product renderers consume the mux-owned style policy.
    #[test]
    fn terminal_color_blending_preserves_true_color_and_indexed_fallback() {
        assert_eq!(
            blend_terminal_color(
                TerminalColor::Rgb(0, 20, 40),
                TerminalColor::Rgb(100, 120, 140),
                1,
                2,
            ),
            TerminalColor::Rgb(50, 70, 90)
        );
        assert_eq!(
            blend_terminal_color(
                TerminalColor::Indexed(7),
                TerminalColor::Rgb(100, 120, 140),
                1,
                2,
            ),
            TerminalColor::Indexed(7)
        );
    }

    /// Verifies contiguous equal rendition spans coalesce while semantic style
    /// changes remain represented by separate terminal spans.
    #[test]
    fn style_span_coalescing_preserves_rendition_boundaries() {
        let inverse = GraphicRendition {
            inverse: true,
            ..GraphicRendition::default()
        };
        let mut spans = vec![TerminalStyleSpan {
            start: 0,
            length: 2,
            rendition: inverse,
        }];
        push_or_extend_style_span(
            &mut spans,
            TerminalStyleSpan {
                start: 2,
                length: 3,
                rendition: inverse,
            },
        );
        push_or_extend_style_span(
            &mut spans,
            TerminalStyleSpan {
                start: 5,
                length: 1,
                rendition: GraphicRendition::default(),
            },
        );

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].length, 5);
        assert_eq!(spans[1].start, 5);
    }
}
