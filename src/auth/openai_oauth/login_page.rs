//! Themed browser callback page rendering and color translation.

use super::platform_browser::html_escape;
use super::{LoginPageKind, LoginPageRgb, LoginPageThemeTokens};
use mez_mux::theme::UiTheme;
use mez_terminal::TerminalColor;
use std::io::Write;

/// Writes a themed browser callback response to a blocking stream.
pub(super) fn write_http_response_with_tokens(
    stream: &mut impl Write,
    status: u16,
    body: &str,
    tokens: &LoginPageThemeTokens,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "OK",
    };
    let document = login_page_document(status, body, tokens);
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {document}",
        document.len()
    )
}

/// Builds the browser callback page using active Mezzanine theme tokens.
pub(super) fn login_page_document(
    status: u16,
    body: &str,
    tokens: &LoginPageThemeTokens,
) -> String {
    let kind = LoginPageKind::from_status(status);
    let escaped_message = html_escape(body);
    let color_scheme = if tokens.is_dark { "dark" } else { "light" };
    let technical_note = match kind {
        LoginPageKind::Success => {
            "Localhost callback complete. No external page assets were loaded."
        }
        LoginPageKind::Error => {
            "The localhost callback did not complete the requested credential exchange."
        }
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mezzanine sign-in</title>
<style>
:root {{
  color-scheme: {color_scheme};
  --bg: {bg};
  --surface: {surface};
  --surface-elevated: {surface_elevated};
  --border: {border};
  --text-primary: {text_primary};
  --text-secondary: {text_secondary};
  --accent-primary: {accent_primary};
  --accent-secondary: {accent_secondary};
  --success: {success};
  --glow-strength: {glow_strength};
}}
* {{
  box-sizing: border-box;
}}
html,
body {{
  min-height: 100%;
}}
body {{
  margin: 0;
  display: grid;
  place-items: center;
  min-height: 100vh;
  padding: clamp(1rem, 4vw, 2.5rem);
  color: var(--text-primary);
  background: var(--bg);
  font-family:
    ui-sans-serif,
    system-ui,
    -apple-system,
    BlinkMacSystemFont,
    "Segoe UI",
    sans-serif;
}}
.mez-shell {{
  width: min(94vw, 46rem);
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--surface);
  box-shadow:
    0 1.2rem 3.2rem rgba(0, 0, 0, calc(var(--glow-strength) + 0.18)),
    0 0 0 1px color-mix(in srgb, var(--text-primary) 5%, transparent);
}}
.mez-titlebar {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.72rem 0.88rem;
  border-bottom: 1px solid var(--border);
  background: var(--surface-elevated);
}}
.mez-title {{
  display: inline-flex;
  align-items: center;
  gap: 0.55rem;
  min-width: 0;
  color: var(--text-primary);
  font-size: 0.9rem;
  font-weight: 650;
}}
.mez-dot {{
  width: 0.65rem;
  height: 0.65rem;
  border-radius: 999px;
  background: var(--success);
  box-shadow: 0 0 0 0.18rem color-mix(in srgb, var(--success) 16%, transparent);
}}
h1 {{
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font: inherit;
}}
.status-pill {{
  flex: 0 0 auto;
  padding: 0.22rem 0.55rem;
  border: 1px solid color-mix(in srgb, var(--success) 52%, var(--border));
  border-radius: 999px;
  color: var(--success);
  background: color-mix(in srgb, var(--success) 10%, transparent);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}}
.mez-pane {{
  padding: clamp(1rem, 3vw, 1.35rem);
  background: color-mix(in srgb, var(--surface) 82%, var(--bg));
  font-family:
    ui-monospace,
    SFMono-Regular,
    Menlo,
    Monaco,
    Consolas,
    "Liberation Mono",
    monospace;
  font-size: clamp(0.9rem, 2.3vw, 1rem);
  line-height: 1.65;
}}
.transcript-line {{
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 0.75rem;
  align-items: baseline;
}}
.transcript-line + .transcript-line {{
  margin-top: 0.5rem;
}}
.speaker {{
  color: var(--accent-primary);
  font-weight: 700;
  white-space: nowrap;
}}
.line-text {{
  color: var(--text-primary);
}}
.line-text.secondary {{
  color: var(--text-secondary);
}}
.line-text.strong {{
  color: var(--text-primary);
  font-weight: 700;
}}
.mez-footer {{
  padding: 0.65rem 0.88rem;
  border-top: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
  background: var(--surface-elevated);
  color: var(--text-secondary);
  font-size: 0.78rem;
}}
</style>
</head>
<body>
<main class="mez-shell" aria-labelledby="login-title">
  <header class="mez-titlebar">
    <div class="mez-title">
      <span class="mez-dot" aria-hidden="true"></span>
      <h1 id="login-title">Mezzanine auth</h1>
    </div>
    <span class="status-pill">{badge}</span>
  </header>
  <section class="mez-pane" aria-label="Authentication result transcript">
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text strong">{headline}</span>
    </div>
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text">{escaped_message}</span>
    </div>
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text secondary">{hint}</span>
    </div>
  </section>
  <footer class="mez-footer">{technical_note}</footer>
</main>
</body>
</html>"#,
        color_scheme = color_scheme,
        bg = tokens.bg.as_str(),
        surface = tokens.surface.as_str(),
        surface_elevated = tokens.surface_elevated.as_str(),
        border = tokens.border.as_str(),
        text_primary = tokens.text_primary.as_str(),
        text_secondary = tokens.text_secondary.as_str(),
        accent_primary = tokens.accent_primary.as_str(),
        accent_secondary = tokens.accent_secondary.as_str(),
        success = tokens.success.as_str(),
        glow_strength = tokens.glow_strength,
        badge = kind.badge(),
        headline = kind.headline(),
        hint = kind.hint(),
        technical_note = technical_note,
        escaped_message = escaped_message,
    )
}

/// Builds web-safe callback-page tokens from the active Mezzanine UI theme.
pub(super) fn login_page_theme_tokens(ui_theme: &UiTheme) -> LoginPageThemeTokens {
    let fallback_bg = LoginPageRgb::new(11, 31, 23);
    let fallback_text = LoginPageRgb::new(228, 239, 232);
    let bg = login_page_rgb_from_terminal_color(ui_theme.colors.frame_fill.background, fallback_bg);
    let text_primary =
        login_page_rgb_from_terminal_color(ui_theme.colors.frame_fill.foreground, fallback_text);
    let text_secondary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_transcript_status.foreground,
        login_page_mix(text_primary, bg, 0.35),
    );
    let accent_primary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_status_running.background,
        LoginPageRgb::new(87, 199, 133),
    );
    let accent_secondary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_reasoning.background,
        LoginPageRgb::new(215, 196, 106),
    );
    let success = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_status_running.background,
        accent_primary,
    );
    let is_dark = login_page_is_dark(bg);
    let surface = if is_dark {
        login_page_mix(bg, text_primary, 0.08)
    } else {
        login_page_mix(bg, text_primary, 0.04)
    };
    let surface_elevated = if is_dark {
        login_page_mix(surface, text_primary, 0.06)
    } else {
        login_page_mix(surface, LoginPageRgb::new(255, 255, 255), 0.52)
    };
    let border = login_page_mix(surface, accent_primary, if is_dark { 0.48 } else { 0.34 });
    LoginPageThemeTokens {
        bg: login_page_rgb_hex(bg),
        surface: login_page_rgb_hex(surface),
        surface_elevated: login_page_rgb_hex(surface_elevated),
        border: login_page_rgb_hex(border),
        text_primary: login_page_rgb_hex(text_primary),
        text_secondary: login_page_rgb_hex(text_secondary),
        accent_primary: login_page_rgb_hex(accent_primary),
        accent_secondary: login_page_rgb_hex(accent_secondary),
        success: login_page_rgb_hex(success),
        glow_strength: if is_dark { "0.32" } else { "0.12" },
        is_dark,
    }
}

/// Converts a terminal color into RGB for browser CSS token generation.
pub(super) fn login_page_rgb_from_terminal_color(
    color: TerminalColor,
    fallback: LoginPageRgb,
) -> LoginPageRgb {
    match color {
        TerminalColor::Rgb(red, green, blue) => LoginPageRgb::new(red, green, blue),
        TerminalColor::Indexed(index) => login_page_rgb_from_index(index).unwrap_or(fallback),
    }
}

/// Converts an ANSI or xterm 256-color palette index into RGB.
pub(super) fn login_page_rgb_from_index(index: u8) -> Option<LoginPageRgb> {
    const ANSI: [LoginPageRgb; 16] = [
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 128,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 128,
            blue: 0,
        },
        LoginPageRgb {
            red: 128,
            green: 128,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 128,
        },
        LoginPageRgb {
            red: 128,
            green: 0,
            blue: 128,
        },
        LoginPageRgb {
            red: 0,
            green: 128,
            blue: 128,
        },
        LoginPageRgb {
            red: 192,
            green: 192,
            blue: 192,
        },
        LoginPageRgb {
            red: 128,
            green: 128,
            blue: 128,
        },
        LoginPageRgb {
            red: 255,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 255,
            blue: 0,
        },
        LoginPageRgb {
            red: 255,
            green: 255,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 255,
        },
        LoginPageRgb {
            red: 255,
            green: 0,
            blue: 255,
        },
        LoginPageRgb {
            red: 0,
            green: 255,
            blue: 255,
        },
        LoginPageRgb {
            red: 255,
            green: 255,
            blue: 255,
        },
    ];
    match index {
        0..=15 => Some(ANSI[usize::from(index)]),
        16..=231 => {
            let value = index - 16;
            let component = |slot: u8| -> u8 {
                if slot == 0 {
                    0
                } else {
                    55 + slot.saturating_mul(40)
                }
            };
            Some(LoginPageRgb::new(
                component(value / 36),
                component((value / 6) % 6),
                component(value % 6),
            ))
        }
        232..=255 => {
            let level = 8 + (index - 232).saturating_mul(10);
            Some(LoginPageRgb::new(level, level, level))
        }
    }
}

/// Formats an RGB color as a CSS hexadecimal color.
pub(super) fn login_page_rgb_hex(color: LoginPageRgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue)
}

/// Mixes two RGB colors by the supplied right-hand-side amount.
pub(super) fn login_page_mix(left: LoginPageRgb, right: LoginPageRgb, amount: f32) -> LoginPageRgb {
    let amount = amount.clamp(0.0, 1.0);
    let mix_channel = |left: u8, right: u8| -> u8 {
        (f32::from(left) + (f32::from(right) - f32::from(left)) * amount).round() as u8
    };
    LoginPageRgb::new(
        mix_channel(left.red, right.red),
        mix_channel(left.green, right.green),
        mix_channel(left.blue, right.blue),
    )
}

/// Returns whether a background color should be treated as dark.
pub(super) fn login_page_is_dark(color: LoginPageRgb) -> bool {
    login_page_luminance(color) < 140
}

/// Computes perceptual luma for a browser-page RGB token.
pub(super) fn login_page_luminance(color: LoginPageRgb) -> u16 {
    (u16::from(color.red) * 30 + u16::from(color.green) * 59 + u16::from(color.blue) * 11) / 100
}
