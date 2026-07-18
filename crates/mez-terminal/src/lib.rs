//! Terminal emulation and compatibility for one terminal surface.
//!
//! This crate owns pane-facing control parsing, screen and buffer state,
//! history, profiles, styles, width behavior, mouse packets, and terminal-mode
//! input encoding. Multiplexer layout, frames, overlays, attached-client policy,
//! and agent presentation remain outside this boundary.

/// Positive dimensions for one terminal surface.
pub mod geometry;
/// Bounded pane-facing terminal scrollback history.
pub mod history;
/// Pane-facing terminal mouse protocol types and parser.
pub mod mouse;
/// Pane-facing compatibility profiles and terminfo selection policy.
pub mod profile;
/// Pane-facing terminal protocol event contracts.
pub mod protocol;
/// Pane-facing terminal parser and emulated screen state.
pub mod screen;
/// Configuration errors for one emulated terminal screen.
pub mod screen_error;
/// Restorable terminal mode and parser state contracts.
pub mod state;
/// Styled terminal-cell contracts produced by terminal emulation.
pub mod style;
/// Unicode segmentation and display-width contracts for terminal cells.
pub mod width;

pub use mouse::{MouseButton, MouseEvent, MouseEventKind, MouseModifiers, parse_sgr_mouse};

pub use geometry::{TerminalSize, TerminalSizeError};

pub use protocol::{
    MAX_OSC_STRING_BYTES, TerminalClipboardContent, TerminalClipboardRequest,
    TerminalClipboardSelection, TerminalOscEvent,
};

pub use screen::{AlternateScreenState, TerminalScreen};

pub use screen_error::TerminalScreenConfigError;

pub use history::{
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, HistoryBuffer, HistoryConfigError,
};

pub use style::{GraphicRendition, TerminalColor, TerminalStyleSpan, TerminalStyledLine};

pub use width::{
    TerminalEmojiWidth, active_terminal_grapheme_width, active_terminal_text_width,
    set_terminal_emoji_width, terminal_char_width, terminal_emoji_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width,
};

pub use state::{
    TerminalCursorState, TerminalModeState, TerminalSavedDecPrivateMode, TerminalSavedState,
    tracked_dec_private_mode,
};

pub use profile::{
    CapabilitySupport, DEFAULT_MEZZANINE_TERMINFO, DEFAULT_PANE_TERM,
    DEFAULT_TERMINAL_PROFILE_NAME, DecPrivateModeCapabilities, MEZZANINE_TERMINFO_NAMES,
    MEZZANINE_TERMINFO_PROFILES, SaveRestoreCapabilities, SgrCapabilities, TERMINFO_FALLBACK_ORDER,
    TERMINFO_FALLBACK_PROFILES, TerminalCapabilities, TerminalCompatibilityProfile,
    TerminalDiagnostic, TerminalDiagnosticSeverity, TerminalProfile, TerminalProfileError,
    TerminfoCapabilityProfile, TerminfoSelection, TerminfoSource, select_installed_terminfo,
    select_terminfo, terminal_profile_named,
};

#[cfg(test)]
mod tests;
