//! Serializable terminal mode and parser save-state contracts.
//!
//! These values describe one terminal surface without depending on screen
//! storage, multiplexer state, or product persistence. Callers may snapshot
//! and restore them around process and session lifecycle operations.

/// Terminal mode flags and title state that can be restored without a PTY replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalModeState {
    /// Current terminal title from OSC 0/2, when known.
    pub title: Option<String>,
    /// Whether the pane application has requested a visible terminal cursor.
    pub cursor_visible: bool,
    /// Whether bracketed paste mode is active.
    pub bracketed_paste_enabled: bool,
    /// Whether DECSET 1000 normal mouse tracking is active.
    pub normal_mouse_tracking_enabled: bool,
    /// Whether DECSET 1002 button-event mouse tracking is active.
    pub button_event_mouse_tracking_enabled: bool,
    /// Whether DECSET 1003 any-event mouse tracking is active.
    pub any_event_mouse_tracking_enabled: bool,
    /// Whether SGR mouse encoding is active.
    pub sgr_mouse_enabled: bool,
    /// Whether application cursor-key mode is active.
    pub application_cursor_enabled: bool,
    /// Whether DEC origin mode is active.
    pub origin_mode_enabled: bool,
    /// Whether DEC autowrap mode is active.
    pub autowrap_enabled: bool,
    /// Whether application keypad mode is active.
    pub application_keypad_enabled: bool,
    /// Whether focus event reporting is active.
    pub focus_events_enabled: bool,
}

impl Default for TerminalModeState {
    fn default() -> Self {
        Self {
            title: None,
            cursor_visible: true,
            bracketed_paste_enabled: false,
            normal_mouse_tracking_enabled: false,
            button_event_mouse_tracking_enabled: false,
            any_event_mouse_tracking_enabled: false,
            sgr_mouse_enabled: false,
            application_cursor_enabled: false,
            origin_mode_enabled: false,
            autowrap_enabled: true,
            application_keypad_enabled: false,
            focus_events_enabled: false,
        }
    }
}

/// Zero-based cursor state persisted for terminal save/restore behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCursorState {
    /// Zero-based row in terminal-cell coordinates.
    pub row: usize,
    /// Zero-based column in terminal-cell coordinates.
    pub column: usize,
}

/// Saved DEC private mode value persisted for later CSI `?mode r` handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSavedDecPrivateMode {
    /// DEC private mode number.
    pub mode: u16,
    /// Saved enabled state for the mode.
    pub enabled: bool,
}

/// Terminal parser save/restore state that can survive snapshot resume.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalSavedState {
    /// Saved cursor position from ESC 7 / CSI s, when one has been saved.
    pub saved_cursor: Option<TerminalCursorState>,
    /// Saved tracked DEC private modes from CSI `?mode s`.
    pub saved_dec_private_modes: Vec<TerminalSavedDecPrivateMode>,
    /// Whether G0 is designated as DEC Special Graphics.
    pub g0_dec_special_graphics: bool,
    /// Whether G1 is designated as DEC Special Graphics.
    pub g1_dec_special_graphics: bool,
    /// Whether SO currently invokes G1 into GL.
    pub shift_out: bool,
}

/// Returns whether one DEC private mode participates in save/restore state.
pub fn tracked_dec_private_mode(mode: u16) -> bool {
    matches!(
        mode,
        1 | 6 | 7 | 25 | 47 | 1047 | 1048 | 1049 | 1000 | 1002 | 1003 | 1004 | 1006 | 2004
    )
}
