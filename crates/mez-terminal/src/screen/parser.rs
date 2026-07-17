//! Incremental terminal control-sequence parser and dispatch.
//!
//! This module advances parser state for printable text, C0 controls, ESC, CSI,
//! OSC, and charset sequences. It delegates resulting grid and lifecycle changes
//! to their owning `TerminalScreen` implementations.

use super::*;

impl TerminalScreen {
    /// Runs the feed char operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_char(&mut self, ch: char) {
        match self.parser_state {
            ParserState::Ground => self.feed_ground(ch),
            ParserState::Escape => self.feed_escape(ch),
            ParserState::EscapeCharsetG0 => self.feed_escape_charset_g0(ch),
            ParserState::EscapeCharsetG1 => self.feed_escape_charset_g1(ch),
            ParserState::Csi => self.feed_csi(ch),
            ParserState::Osc => self.feed_osc(ch),
            ParserState::OscEscape => self.feed_osc_escape(ch),
            ParserState::Dcs => self.feed_dcs(ch),
            ParserState::DcsEscape => self.feed_dcs_escape(ch),
        }
    }

    /// Runs the feed ground operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_ground(&mut self, ch: char) {
        match ch {
            '\u{1b}' => {
                self.wrap_pending = false;
                self.parser_state = ParserState::Escape;
            }
            '\u{0007}' => self.bell_events = self.bell_events.saturating_add(1),
            '\n' => {
                if self.line_feed_newline_enabled {
                    self.newline();
                } else {
                    self.index();
                }
            }
            '\r' => {
                self.wrap_pending = false;
                self.cursor.column = 0;
            }
            '\u{0008}' => {
                self.wrap_pending = false;
                self.cursor.column = self.cursor.column.saturating_sub(1);
            }
            '\t' => {
                self.wrap_pending = false;
                let next_tab = (self.cursor.column / 8 + 1) * 8;
                self.cursor.column = next_tab.min(self.max_column());
            }
            '\u{000e}' => self.shift_out = true,
            '\u{000f}' => self.shift_out = false,
            ch if !ch.is_control() => self.print(ch),
            _ => {}
        }
    }

    /// Runs the feed escape operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_escape(&mut self, ch: char) {
        if ch == '[' {
            self.csi_buffer.clear();
            self.csi_buffer_truncated = false;
            self.parser_state = ParserState::Csi;
        } else if ch == ']' {
            self.osc_buffer.clear();
            self.osc_buffer_truncated = false;
            self.parser_state = ParserState::Osc;
        } else if ch == '(' {
            self.parser_state = ParserState::EscapeCharsetG0;
        } else if ch == ')' {
            self.parser_state = ParserState::EscapeCharsetG1;
        } else if matches!(ch, 'P' | 'X' | '^' | '_') {
            self.parser_state = ParserState::Dcs;
        } else if ch == '7' {
            self.save_cursor();
            self.parser_state = ParserState::Ground;
        } else if ch == '8' {
            self.restore_cursor();
            self.parser_state = ParserState::Ground;
        } else if ch == 'D' {
            self.index();
            self.parser_state = ParserState::Ground;
        } else if ch == 'E' {
            self.next_line();
            self.parser_state = ParserState::Ground;
        } else if ch == 'M' {
            self.reverse_index();
            self.parser_state = ParserState::Ground;
        } else if ch == '=' {
            self.application_keypad_enabled = true;
            self.parser_state = ParserState::Ground;
        } else if ch == '>' {
            self.application_keypad_enabled = false;
            self.parser_state = ParserState::Ground;
        } else {
            self.parser_state = ParserState::Ground;
        }
    }

    /// Runs the feed escape charset G0 operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_escape_charset_g0(&mut self, ch: char) {
        self.designate_charset(false, ch);
    }

    /// Runs the feed escape charset G1 operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_escape_charset_g1(&mut self, ch: char) {
        self.designate_charset(true, ch);
    }

    /// Runs the designate charset operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn designate_charset(&mut self, g1: bool, ch: char) {
        let charset = match ch {
            '0' => TerminalCharset::DecSpecialGraphics,
            'B' => TerminalCharset::Ascii,
            _ => {
                self.parser_state = ParserState::Ground;
                return;
            }
        };

        if g1 {
            self.g1_charset = charset;
        } else {
            self.g0_charset = charset;
        }
        self.parser_state = ParserState::Ground;
    }

    /// Returns the currently invoked GL charset.
    pub(super) fn active_charset(&self) -> TerminalCharset {
        if self.shift_out {
            self.g1_charset
        } else {
            self.g0_charset
        }
    }

    /// Runs the feed osc operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_osc(&mut self, ch: char) {
        match ch {
            '\u{0007}' => self.finish_osc(),
            '\u{001b}' => self.parser_state = ParserState::OscEscape,
            _ => self.push_osc_char(ch),
        }
    }

    /// Runs the feed osc escape operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_osc_escape(&mut self, ch: char) {
        if ch == '\\' {
            self.finish_osc();
        } else {
            self.push_osc_char('\u{001b}');
            self.parser_state = ParserState::Osc;
            self.feed_osc(ch);
        }
    }

    /// Runs the feed dcs operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_dcs(&mut self, ch: char) {
        if ch == '\u{001b}' {
            self.parser_state = ParserState::DcsEscape;
        }
    }

    /// Runs the feed dcs escape operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_dcs_escape(&mut self, ch: char) {
        if ch == '\\' {
            self.parser_state = ParserState::Ground;
        } else if ch != '\u{001b}' {
            self.parser_state = ParserState::Dcs;
        }
    }

    /// Runs the finish osc operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn finish_osc(&mut self) {
        let payload = std::mem::take(&mut self.osc_buffer);
        let truncated = std::mem::take(&mut self.osc_buffer_truncated);
        if !truncated {
            self.dispatch_osc(&payload);
        }
        self.parser_state = ParserState::Ground;
    }

    /// Runs the push osc char operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn push_osc_char(&mut self, ch: char) {
        if self.osc_buffer.len().saturating_add(ch.len_utf8()) <= MAX_OSC_STRING_BYTES {
            self.osc_buffer.push(ch);
        } else {
            self.osc_buffer_truncated = true;
        }
    }

    /// Runs the dispatch osc operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_osc(&mut self, payload: &str) {
        if let Some(payload) = payload.strip_prefix("133;") {
            self.osc_events.push(TerminalOscEvent::ShellIntegration {
                payload: payload.to_string(),
            });
            return;
        }
        let Some((command, text)) = payload.split_once(';') else {
            return;
        };
        if matches!(command, "0" | "2") {
            self.title = Some(text.to_string());
            self.osc_events.push(TerminalOscEvent::TitleChanged {
                title: text.to_string(),
            });
        } else if command == "52"
            && let Some((selection, encoded)) = text.split_once(';')
            && let Some(content) = decode_standard_base64_utf8(encoded)
        {
            self.osc_events.push(TerminalOscEvent::ClipboardSet {
                selection: selection.to_string(),
                content,
            });
        }
    }

    /// Runs the feed csi operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn feed_csi(&mut self, ch: char) {
        if ('@'..='~').contains(&ch) {
            let params = self.csi_buffer.clone();
            if !self.csi_buffer_truncated {
                self.dispatch_csi(&params, ch);
            }
            self.csi_buffer.clear();
            self.csi_buffer_truncated = false;
            self.parser_state = ParserState::Ground;
        } else if self.csi_buffer.len().saturating_add(ch.len_utf8()) <= MAX_CSI_STRING_BYTES {
            self.csi_buffer.push(ch);
        } else {
            self.csi_buffer_truncated = true;
        }
    }

    /// Runs the dispatch csi operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_csi(&mut self, params: &str, final_byte: char) {
        if matches!(final_byte, 'h' | 'l')
            && let Some(modes) = parse_dec_private_mode_params(params)
        {
            self.apply_dec_private_modes(&modes, final_byte == 'h');
            return;
        }
        if matches!(final_byte, 'h' | 'l') && params == "20" {
            self.line_feed_newline_enabled = final_byte == 'h';
            return;
        }
        if final_byte == 's'
            && let Some(modes) = parse_dec_private_mode_params(params)
        {
            self.save_dec_private_modes(&modes);
            return;
        }
        if final_byte == 'r'
            && let Some(modes) = parse_dec_private_mode_params(params)
        {
            self.restore_dec_private_modes(&modes);
            return;
        }

        match final_byte {
            'A' => self.move_cursor_relative(params, -1, 0),
            'B' => self.move_cursor_relative(params, 1, 0),
            'C' => self.move_cursor_relative(params, 0, 1),
            'D' => self.move_cursor_relative(params, 0, -1),
            'E' => self.move_cursor_next_line(params),
            'F' => self.move_cursor_previous_line(params),
            'G' | '`' => self.move_cursor_column(params),
            'H' | 'f' => self.move_cursor(params),
            'X' => self.erase_chars(csi_count(params)),
            'a' => self.move_cursor_relative(params, 0, 1),
            'd' => self.move_cursor_row(params),
            'e' => self.move_cursor_relative(params, 1, 0),
            'J' => self.erase_display(params),
            'K' => self.erase_line(params),
            'n' => self.report_device_status(params),
            '@' => self.insert_blank_chars(csi_count(params)),
            'P' => self.delete_chars(csi_count(params)),
            'L' => self.insert_lines(csi_count(params)),
            'M' => self.delete_lines(csi_count(params)),
            'S' => self.scroll_region_up(csi_count(params)),
            'T' => self.scroll_region_down(csi_count(params)),
            'm' => self.apply_sgr(params),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    /// Queues terminal-generated device status replies for the pane process.
    pub(super) fn report_device_status(&mut self, params: &str) {
        match first_csi_param(params) {
            5 => self.terminal_response_bytes.extend_from_slice(b"\x1b[0n"),
            6 => {
                let row = self.cursor.row.saturating_add(1);
                let column = self.cursor.column.saturating_add(1);
                self.terminal_response_bytes
                    .extend_from_slice(format!("\x1b[{row};{column}R").as_bytes());
            }
            _ => {}
        }
    }

    /// Captures normal-screen state before alternate mode clears the viewport.
    pub(super) fn saved_normal_screen_state(&self) -> SavedNormalScreenState {
        SavedNormalScreenState {
            cells: self.cells.clone(),
            renditions: self.renditions.clone(),
            line_wraps: self.line_wraps.clone(),
            line_copy_texts: self.line_copy_texts.clone(),
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            wrap_pending: self.wrap_pending,
            saved_cursor: self.saved_cursor,
            graphic_rendition: self.graphic_rendition,
            normal_viewport_detached_from_history: self.normal_viewport_detached_from_history,
            size: self.size,
            autowrap_enabled: self.autowrap_enabled,
            origin_mode_enabled: self.origin_mode_enabled,
            scroll_region: self.scroll_region,
        }
    }

    /// Restores saved normal-screen state after alternate mode exits.
    pub(super) fn restore_saved_normal_screen_state(&mut self, state: SavedNormalScreenState) {
        let target_size = self.size;
        self.cells = state.cells;
        self.renditions = state.renditions;
        self.line_wraps = state.line_wraps;
        self.line_copy_texts = state.line_copy_texts;
        self.cursor = state.cursor;
        self.cursor_visible = state.cursor_visible;
        self.wrap_pending = state.wrap_pending;
        self.saved_cursor = state.saved_cursor;
        self.graphic_rendition = state.graphic_rendition;
        self.normal_viewport_detached_from_history = state.normal_viewport_detached_from_history;
        self.size = state.size;
        self.autowrap_enabled = state.autowrap_enabled;
        self.origin_mode_enabled = state.origin_mode_enabled;
        self.scroll_region = state.scroll_region;

        if self.size != target_size {
            self.resize(target_size);
            return;
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self.cursor.row.min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Runs the apply dec private modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_dec_private_modes(&mut self, modes: &[u16], enabled: bool) {
        for mode in modes {
            self.apply_dec_private_mode(*mode, enabled);
        }
    }

    /// Runs the save dec private modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn save_dec_private_modes(&mut self, modes: &[u16]) {
        for mode in modes {
            if let Some(enabled) = self.dec_private_mode_enabled(*mode) {
                self.saved_dec_private_modes.insert(*mode, enabled);
            }
        }
    }

    /// Runs the restore dec private modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn restore_dec_private_modes(&mut self, modes: &[u16]) {
        for mode in modes {
            if let Some(enabled) = self.saved_dec_private_modes.get(mode).copied() {
                self.apply_dec_private_mode(*mode, enabled);
            }
        }
    }

    /// Runs the apply dec private mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_dec_private_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            47 | 1047 | 1049 => {
                if enabled {
                    if self.alternate.active() {
                        return;
                    }
                    if mode == 1049 {
                        self.save_cursor();
                    }
                    let state = self.saved_normal_screen_state();
                    self.alternate.enter_with_saved_normal_screen(state);
                    self.alternate_screen_generation =
                        self.alternate_screen_generation.wrapping_add(1);
                    self.clear_screen();
                } else if let Some(state) = self.alternate.leave() {
                    self.alternate_screen_generation =
                        self.alternate_screen_generation.wrapping_add(1);
                    self.restore_saved_normal_screen_state(state);
                    if mode == 1049 {
                        self.restore_cursor();
                    }
                }
            }
            1048 => {
                if enabled {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            25 => self.cursor_visible = enabled,
            1 => self.application_cursor_enabled = enabled,
            7 => {
                self.autowrap_enabled = enabled;
                if !enabled {
                    self.wrap_pending = false;
                }
            }
            6 => {
                self.origin_mode_enabled = enabled;
                self.cursor.row = if enabled {
                    self.active_scroll_region().0
                } else {
                    0
                };
                self.cursor.column = 0;
                self.wrap_pending = false;
            }
            1000 => self.normal_mouse_tracking_enabled = enabled,
            1002 => self.button_event_mouse_tracking_enabled = enabled,
            1003 => self.any_event_mouse_tracking_enabled = enabled,
            1004 => self.focus_events_enabled = enabled,
            1006 => self.sgr_mouse_enabled = enabled,
            2004 => self.bracketed_paste_enabled = enabled,
            _ => {}
        }
    }

    /// Runs the dec private mode enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dec_private_mode_enabled(&self, mode: u16) -> Option<bool> {
        if !tracked_dec_private_mode(mode) {
            return None;
        }
        match mode {
            47 | 1047 | 1049 => Some(self.alternate.active()),
            1048 => Some(self.saved_cursor.is_some()),
            25 => Some(self.cursor_visible),
            1 => Some(self.application_cursor_enabled),
            7 => Some(self.autowrap_enabled),
            6 => Some(self.origin_mode_enabled),
            1000 => Some(self.normal_mouse_tracking_enabled),
            1002 => Some(self.button_event_mouse_tracking_enabled),
            1003 => Some(self.any_event_mouse_tracking_enabled),
            1004 => Some(self.focus_events_enabled),
            1006 => Some(self.sgr_mouse_enabled),
            2004 => Some(self.bracketed_paste_enabled),
            _ => unreachable!("tracked DEC private mode must be handled"),
        }
    }
}
