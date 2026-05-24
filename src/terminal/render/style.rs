//! Terminal render color and style-span helpers.
//!
//! This module owns pure theme-derived color math, active-status scan palette
//! generation, contrast helpers, and style-span coalescing used by terminal
//! frame, footer, and prompt rendering.

use crate::terminal::{TerminalColor, TerminalStyleSpan, UiTheme};

/// Returns a theme-relative harmonious ramp for active agent status animation.
pub(super) fn agent_status_running_gradient_palette(ui_theme: &UiTheme) -> [TerminalColor; 3] {
    let base = ui_theme.colors.agent_status_running.background;
    let Some((red, green, blue)) = terminal_color_rgb(base) else {
        return [
            base,
            ui_theme.colors.agent_model.background,
            ui_theme.colors.agent_reasoning.background,
        ];
    };
    let hsl = rgb_to_hsl(red, green, blue);
    let saturation = (hsl.saturation * 1.12 + 0.04).clamp(0.0, 1.0);
    let lightness_offset = if hsl.lightness > 0.62 { -0.08 } else { 0.08 };
    let lightness = (hsl.lightness + lightness_offset).clamp(0.18, 0.82);
    [
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue - 30.0,
            saturation,
            lightness,
        }),
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue,
            saturation: (saturation * 0.92).clamp(0.0, 1.0),
            lightness: (lightness + lightness_offset / 2.0).clamp(0.18, 0.82),
        }),
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue + 30.0,
            saturation,
            lightness,
        }),
    ]
}

/// Chooses a scan highlight from the left, center, or right side of the ramp.
pub(super) fn gradient_highlight_for_offset(
    palette: &[TerminalColor; 3],
    offset: isize,
) -> TerminalColor {
    if offset < -1 {
        palette[0]
    } else if offset > 1 {
        palette[2]
    } else {
        palette[1]
    }
}

/// Blends one scan-band cell between the base and highlight colors.
pub(super) fn animated_scan_background(
    base: TerminalColor,
    highlight: TerminalColor,
    intensity: usize,
    max_intensity: usize,
) -> TerminalColor {
    let Some((base_r, base_g, base_b)) = terminal_color_rgb(base) else {
        return base;
    };
    let Some((highlight_r, highlight_g, highlight_b)) = terminal_color_rgb(highlight) else {
        return base;
    };
    let numerator = intensity.min(max_intensity) as u16;
    let denominator = max_intensity.max(1) as u16;
    TerminalColor::Rgb(
        blend_channel(base_r, highlight_r, numerator, denominator),
        blend_channel(base_g, highlight_g, numerator, denominator),
        blend_channel(base_b, highlight_b, numerator, denominator),
    )
}

/// HSL representation used for theme-derived color harmonies.
#[derive(Debug, Clone, Copy)]
struct HslColor {
    /// Hue in degrees.
    hue: f32,
    /// Saturation in the range 0.0..=1.0.
    saturation: f32,
    /// Lightness in the range 0.0..=1.0.
    lightness: f32,
}

/// Converts RGB components to HSL so neighboring hues can be derived.
fn rgb_to_hsl(red: u8, green: u8, blue: u8) -> HslColor {
    let red = f32::from(red) / 255.0;
    let green = f32::from(green) / 255.0;
    let blue = f32::from(blue) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let chroma = max - min;
    let lightness = (max + min) / 2.0;
    if chroma <= f32::EPSILON {
        return HslColor {
            hue: 0.0,
            saturation: 0.0,
            lightness,
        };
    }
    let saturation = chroma / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if red >= green && red >= blue {
        60.0 * ((green - blue) / chroma).rem_euclid(6.0)
    } else if green >= blue {
        60.0 * ((blue - red) / chroma + 2.0)
    } else {
        60.0 * ((red - green) / chroma + 4.0)
    };
    HslColor {
        hue,
        saturation,
        lightness,
    }
}

/// Converts HSL components into an RGB terminal color.
fn hsl_to_terminal_color(color: HslColor) -> TerminalColor {
    let hue = color.hue.rem_euclid(360.0);
    let saturation = color.saturation.clamp(0.0, 1.0);
    let lightness = color.lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_prime = hue / 60.0;
    let second = chroma * (1.0 - (hue_prime.rem_euclid(2.0) - 1.0).abs());
    let (red1, green1, blue1) = if hue_prime < 1.0 {
        (chroma, second, 0.0)
    } else if hue_prime < 2.0 {
        (second, chroma, 0.0)
    } else if hue_prime < 3.0 {
        (0.0, chroma, second)
    } else if hue_prime < 4.0 {
        (0.0, second, chroma)
    } else if hue_prime < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let match_lightness = lightness - chroma / 2.0;
    TerminalColor::Rgb(
        unit_float_to_u8(red1 + match_lightness),
        unit_float_to_u8(green1 + match_lightness),
        unit_float_to_u8(blue1 + match_lightness),
    )
}

/// Converts a normalized floating color channel to an integer byte.
fn unit_float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Returns RGB components for true-color values.
fn terminal_color_rgb(color: TerminalColor) -> Option<(u8, u8, u8)> {
    match color {
        TerminalColor::Rgb(red, green, blue) => Some((red, green, blue)),
        TerminalColor::Indexed(_) => None,
    }
}

/// Returns a simple perceptual luminance approximation for true-color values.
pub(super) fn terminal_color_luminance(color: TerminalColor) -> Option<u32> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some((u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000)
}

/// Returns WCAG-style contrast ratio for two true-color values.
pub(super) fn terminal_color_contrast_ratio(
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
pub(super) fn terminal_color_relative_luminance(color: TerminalColor) -> Option<f64> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some(
        0.2126 * srgb_channel_to_linear(red)
            + 0.7152 * srgb_channel_to_linear(green)
            + 0.0722 * srgb_channel_to_linear(blue),
    )
}

/// Converts one sRGB channel to linear-light space.
fn srgb_channel_to_linear(channel: u8) -> f64 {
    let normalized = f64::from(channel) / 255.0;
    if normalized <= 0.03928 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}

/// Chooses black or white text for one themed background.
pub(super) fn contrasting_binary_foreground(background: TerminalColor) -> TerminalColor {
    match terminal_color_luminance(background) {
        Some(luminance) if luminance >= 140 => TerminalColor::Rgb(0x00, 0x00, 0x00),
        Some(_) => TerminalColor::Rgb(0xff, 0xff, 0xff),
        None => TerminalColor::Rgb(0xff, 0xff, 0xff),
    }
}

/// Blends two true-color values, falling back to the base for indexed colors.
pub(super) fn blend_terminal_color(
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

/// Builds a quiet neutral context-usage background from the frame surface.
pub(super) fn neutral_surface_step(surface: TerminalColor) -> TerminalColor {
    let Some((red, green, blue)) = terminal_color_rgb(surface) else {
        return surface;
    };
    let luminance = terminal_color_luminance(surface).unwrap_or(0);
    let shift: i16 = if luminance >= 140 { -28 } else { 34 };
    TerminalColor::Rgb(
        shifted_channel(red, shift),
        shifted_channel(green, shift),
        shifted_channel(blue, shift),
    )
}

/// Shifts a color channel by a signed amount.
fn shifted_channel(value: u8, shift: i16) -> u8 {
    (i16::from(value) + shift).clamp(0, 255) as u8
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
pub(super) fn push_or_extend_style_span(
    spans: &mut Vec<TerminalStyleSpan>,
    span: TerminalStyleSpan,
) {
    if let Some(last) = spans.last_mut()
        && last.start.saturating_add(last.length) == span.start
        && last.rendition == span.rendition
    {
        last.length = last.length.saturating_add(span.length);
        return;
    }
    spans.push(span);
}
