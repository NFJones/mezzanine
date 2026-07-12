//! Terminal emulation and compatibility for one terminal surface.
//!
//! This crate will own pane-facing terminal parsing, screen state, history,
//! capability profiles, and mode-aware input encoding. Multiplexer layout,
//! frames, overlays, attached-client policy, and agent presentation remain
//! outside this boundary. The initial empty facade allows those responsibilities
//! to be separated in place before production modules move across packages.

/// Bounded pane-facing terminal scrollback history.
pub mod history;
/// Pane-facing terminal mouse protocol types and parser.
pub mod mouse;
/// Pane-facing compatibility profiles and terminfo selection policy.
pub mod profile;
/// Pane-facing terminal protocol event contracts.
pub mod protocol;
/// Restorable terminal mode and parser state contracts.
pub mod state;
/// Styled terminal-cell contracts produced by terminal emulation.
pub mod style;

pub use mouse::{MouseButton, MouseEvent, MouseEventKind, MouseModifiers, parse_sgr_mouse};

pub use protocol::TerminalOscEvent;

pub use history::{
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, HistoryBuffer, HistoryConfigError,
};

pub use style::{GraphicRendition, TerminalColor, TerminalStyleSpan, TerminalStyledLine};

pub use state::{
    TerminalCursorState, TerminalModeState, TerminalSavedDecPrivateMode, TerminalSavedState,
    tracked_dec_private_mode,
};

pub use profile::{
    CapabilitySupport, DEFAULT_TERMINAL_PROFILE_NAME, DecPrivateModeCapabilities,
    MEZZANINE_TERMINFO_PROFILES, SaveRestoreCapabilities, SgrCapabilities,
    TERMINFO_FALLBACK_PROFILES, TerminalCapabilities, TerminalCompatibilityProfile,
    TerminalDiagnostic, TerminalDiagnosticSeverity, TerminalProfile, TerminalProfileError,
    TerminfoCapabilityProfile, TerminfoSelection, TerminfoSource, select_installed_terminfo,
    select_terminfo, terminal_profile_named,
};

#[cfg(test)]
mod tests;
