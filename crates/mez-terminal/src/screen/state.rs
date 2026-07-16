//! Public terminal mode, cursor, title, and OSC-event state.
//!
//! This module projects and drains non-content state observed by clients. It
//! does not parse escape sequences or mutate the terminal grid.

use super::*;

impl TerminalScreen {
    /// Returns the current terminal mode flags and title state.
    pub fn mode_state(&self) -> TerminalModeState {
        TerminalModeState {
            title: self.title.clone(),
            cursor_visible: self.cursor_visible,
            bracketed_paste_enabled: self.bracketed_paste_enabled,
            normal_mouse_tracking_enabled: self.normal_mouse_tracking_enabled,
            button_event_mouse_tracking_enabled: self.button_event_mouse_tracking_enabled,
            any_event_mouse_tracking_enabled: self.any_event_mouse_tracking_enabled,
            sgr_mouse_enabled: self.sgr_mouse_enabled,
            application_cursor_enabled: self.application_cursor_enabled,
            origin_mode_enabled: self.origin_mode_enabled,
            autowrap_enabled: self.autowrap_enabled,
            application_keypad_enabled: self.application_keypad_enabled,
            focus_events_enabled: self.focus_events_enabled,
        }
    }

    /// Restores terminal mode flags and title state from a snapshot.
    pub fn restore_mode_state(&mut self, state: &TerminalModeState) {
        self.title = state.title.clone();
        self.cursor_visible = state.cursor_visible;
        self.bracketed_paste_enabled = state.bracketed_paste_enabled;
        self.normal_mouse_tracking_enabled = state.normal_mouse_tracking_enabled;
        self.button_event_mouse_tracking_enabled = state.button_event_mouse_tracking_enabled;
        self.any_event_mouse_tracking_enabled = state.any_event_mouse_tracking_enabled;
        self.sgr_mouse_enabled = state.sgr_mouse_enabled;
        self.application_cursor_enabled = state.application_cursor_enabled;
        self.origin_mode_enabled = state.origin_mode_enabled;
        self.autowrap_enabled = state.autowrap_enabled;
        self.application_keypad_enabled = state.application_keypad_enabled;
        self.focus_events_enabled = state.focus_events_enabled;
    }

    /// Returns saved terminal parser state used by future restore sequences.
    pub fn saved_state(&self) -> TerminalSavedState {
        TerminalSavedState {
            saved_cursor: self.saved_cursor.map(|cursor| TerminalCursorState {
                row: cursor.row,
                column: cursor.column,
            }),
            saved_dec_private_modes: self
                .saved_dec_private_modes
                .iter()
                .map(|(mode, enabled)| TerminalSavedDecPrivateMode {
                    mode: *mode,
                    enabled: *enabled,
                })
                .collect(),
            g0_dec_special_graphics: self.g0_charset == TerminalCharset::DecSpecialGraphics,
            g1_dec_special_graphics: self.g1_charset == TerminalCharset::DecSpecialGraphics,
            shift_out: self.shift_out,
        }
    }

    /// Restores saved terminal parser state from a snapshot.
    pub fn restore_saved_state(&mut self, state: &TerminalSavedState) {
        self.saved_cursor = state.saved_cursor.map(|cursor| Cursor {
            row: cursor.row.min(self.max_row()),
            column: cursor.column.min(self.max_column()),
        });
        self.saved_dec_private_modes.clear();
        for saved_mode in &state.saved_dec_private_modes {
            if tracked_dec_private_mode(saved_mode.mode) {
                self.saved_dec_private_modes
                    .insert(saved_mode.mode, saved_mode.enabled);
            }
        }
        self.g0_charset = if state.g0_dec_special_graphics {
            TerminalCharset::DecSpecialGraphics
        } else {
            TerminalCharset::Ascii
        };
        self.g1_charset = if state.g1_dec_special_graphics {
            TerminalCharset::DecSpecialGraphics
        } else {
            TerminalCharset::Ascii
        };
        self.shift_out = state.shift_out;
    }

    /// Runs the alternate screen active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn alternate_screen_active(&self) -> bool {
        self.alternate.active()
    }

    /// Returns whether the pane application requested a visible cursor.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Runs the bracketed paste enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste_enabled
    }

    /// Runs the application sgr mouse enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn application_sgr_mouse_enabled(&self) -> bool {
        self.application_mouse_enabled() && self.sgr_mouse_enabled
    }

    /// Runs the application mouse enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn application_mouse_enabled(&self) -> bool {
        self.normal_mouse_tracking_enabled
            || self.button_event_mouse_tracking_enabled
            || self.any_event_mouse_tracking_enabled
    }

    /// Runs the application cursor enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn application_cursor_enabled(&self) -> bool {
        self.application_cursor_enabled
    }

    /// Returns the current zero-based cursor position tracked for pane output.
    pub fn cursor_state(&self) -> TerminalCursorState {
        TerminalCursorState {
            row: self.cursor.row,
            column: self.cursor.column,
        }
    }

    /// Runs the application keypad enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn application_keypad_enabled(&self) -> bool {
        self.application_keypad_enabled
    }

    /// Runs the focus events enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn focus_events_enabled(&self) -> bool {
        self.focus_events_enabled
    }

    /// Runs the title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Runs the drain osc events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn drain_osc_events(&mut self) -> Vec<TerminalOscEvent> {
        std::mem::take(&mut self.osc_events)
    }

    /// Drains terminal-generated reply bytes for the pane process.
    pub fn drain_terminal_response_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.terminal_response_bytes)
    }

    /// Runs the activity events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn activity_events(&self) -> u64 {
        self.activity_events
    }

    /// Runs the bell events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn bell_events(&self) -> u64 {
        self.bell_events
    }
}
