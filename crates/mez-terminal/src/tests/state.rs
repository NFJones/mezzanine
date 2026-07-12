use crate::{TerminalModeState, TerminalSavedState, tracked_dec_private_mode};

/// Default terminal state reflects ordinary visible-cursor, autowrap behavior.
#[test]
fn terminal_mode_state_defaults_match_terminal_baseline() {
    let state = TerminalModeState::default();

    assert!(state.cursor_visible);
    assert!(state.autowrap_enabled);
    assert!(!state.bracketed_paste_enabled);
    assert!(!state.sgr_mouse_enabled);
}

/// Saved parser state starts empty so restoring a fresh snapshot is inert.
#[test]
fn terminal_saved_state_defaults_are_empty() {
    let state = TerminalSavedState::default();

    assert_eq!(state.saved_cursor, None);
    assert!(state.saved_dec_private_modes.is_empty());
    assert!(!state.g0_dec_special_graphics);
    assert!(!state.g1_dec_special_graphics);
    assert!(!state.shift_out);
}

/// DEC save/restore accepts supported modes and rejects unrelated mode numbers.
#[test]
fn tracked_dec_private_modes_are_explicitly_bounded() {
    assert!(tracked_dec_private_mode(1049));
    assert!(tracked_dec_private_mode(2004));
    assert!(!tracked_dec_private_mode(9999));
}
