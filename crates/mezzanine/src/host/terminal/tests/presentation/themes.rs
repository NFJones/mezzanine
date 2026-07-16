//! Product theme adapter regression tests.

use crate::host::terminal::render::agent_prompt_input_rendition;
use mez_mux::theme::{UiColorPair, UiTheme};
use mez_terminal::TerminalColor;

/// Verifies agent prompt input styling uses the configured prompt foreground
/// instead of recomputing a separate black-or-white contrast color.
#[test]
fn agent_prompt_input_rendition_uses_configured_prompt_foreground() {
    let mut theme = UiTheme::default();
    theme.colors.agent_prompt = UiColorPair {
        foreground: TerminalColor::Rgb(0xff, 0x00, 0x00),
        background: TerminalColor::Rgb(0x20, 0x20, 0x20),
    };

    let rendition = agent_prompt_input_rendition(&theme);

    assert_eq!(
        rendition.foreground,
        Some(theme.colors.agent_prompt.foreground)
    );
    assert_eq!(
        rendition.background,
        Some(theme.colors.agent_prompt.background)
    );
}
