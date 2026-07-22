//! Stateful terminal-screen engine and public screen contract.
//!
//! This module owns the terminal grid, parser state, cursor and saved-screen
//! records shared by the focused screen implementations. Child modules divide
//! lifecycle, parsing, editing, content projection, mode state, cell handling,
//! and continuation wrapping without changing the public `TerminalScreen`
//! contract. The engine remains independent of product runtime and host I/O.

use std::collections::BTreeMap;

use crate::{
    DEFAULT_HISTORY_ROTATE_LINES, HistoryBuffer, TerminalSize as Size,
    terminal_char_width as terminal_char_width_for_policy, terminal_emoji_width,
    terminal_grapheme_width as terminal_grapheme_width_for_policy, terminal_graphemes,
    terminal_text_width as terminal_text_width_for_policy,
};

pub use crate::{
    GraphicRendition, MAX_OSC_STRING_BYTES, TerminalColor, TerminalCursorState, TerminalModeState,
    TerminalOscEvent, TerminalSavedDecPrivateMode, TerminalSavedState, TerminalScreenConfigError,
    TerminalStyleSpan, TerminalStyledLine, tracked_dec_private_mode,
};

/// Returns the display width of one scalar under the active compatibility policy.
fn terminal_char_width(ch: char) -> usize {
    terminal_char_width_for_policy(ch, terminal_emoji_width())
}

/// Returns the display width of one grapheme under the active compatibility policy.
fn terminal_grapheme_width(grapheme: &str) -> usize {
    terminal_grapheme_width_for_policy(grapheme, terminal_emoji_width())
}

/// Returns the display width of text under the active compatibility policy.
fn terminal_text_width(value: &str) -> usize {
    terminal_text_width_for_policy(value, terminal_emoji_width())
}

// Terminal screen parser, OSC events, and alternate-screen state.

/// Maximum bytes retained for one CSI parameter/intermediate sequence.
const MAX_CSI_STRING_BYTES: usize = 1024;

/// Runs the parse dec private mode params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_dec_private_mode_params(params: &str) -> Option<Vec<u16>> {
    let params = params.strip_prefix('?')?;
    let mut modes = Vec::new();
    for part in params.split(';') {
        let mode = part.parse::<u16>().ok()?;
        modes.push(mode);
    }
    (!modes.is_empty()).then_some(modes)
}

/// Identifies one designated VT charset for printable GL bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum TerminalCharset {
    /// ASCII maps printable bytes directly to Unicode scalars.
    #[default]
    Ascii,
    /// DEC Special Graphics remaps ASCII bytes to box-drawing glyphs.
    DecSpecialGraphics,
}

/// Returns the Unicode glyph emitted for one DEC Special Graphics character.
fn dec_special_graphics_char(ch: char) -> Option<char> {
    Some(match ch {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => return None,
    })
}

/// One styled terminal cell from a display-only prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StyledPrefixCell {
    /// Character stored in the terminal cell.
    ch: char,
    /// Terminal display width occupied by the character.
    width: usize,
    /// Graphic rendition applied to the cell.
    rendition: GraphicRendition,
}

/// One display cell in the live terminal screen buffer.
///
/// Leading cells store the complete grapheme cluster that should be emitted
/// for that display position. Extra columns occupied by a wide grapheme are
/// explicit continuation sentinels, so multi-scalar clusters survive the live
/// screen path without being reduced to their first Unicode scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalScreenCell {
    /// Complete grapheme cluster stored at a leading display cell.
    text: String,
    /// Whether this cell is a continuation column for a previous wide glyph.
    continuation: bool,
    /// Whether terminal output explicitly wrote this cell.
    ///
    /// Written default spaces remain part of a logical line during reflow,
    /// while untouched or erased padding can still be omitted.
    written: bool,
}

impl TerminalScreenCell {
    /// Builds a blank leading screen cell.
    fn blank() -> Self {
        Self {
            text: " ".to_string(),
            continuation: false,
            written: false,
        }
    }

    /// Builds a wide-grapheme continuation sentinel.
    fn continuation() -> Self {
        Self {
            text: String::new(),
            continuation: true,
            written: true,
        }
    }

    /// Builds a leading cell containing complete terminal text.
    fn text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            continuation: false,
            written: true,
        }
    }

    /// Returns whether terminal output explicitly occupied this cell.
    fn is_written(&self) -> bool {
        self.written
    }

    /// Returns whether the cell is a default blank leading cell.
    fn is_blank(&self) -> bool {
        !self.continuation && self.text == " "
    }

    /// Returns the display width occupied by this cell's leading text.
    fn width(&self) -> usize {
        if self.continuation {
            0
        } else {
            terminal_grapheme_width(&self.text)
        }
    }
}

/// Builds a screen-sized cell grid initialized to blank leading cells.
fn blank_screen_cells(size: Size) -> Vec<Vec<TerminalScreenCell>> {
    (0..size.rows)
        .map(|_| blank_screen_row(size.columns))
        .collect()
}

/// Builds one row initialized to blank leading cells.
fn blank_screen_row(columns: u16) -> Vec<TerminalScreenCell> {
    vec![TerminalScreenCell::blank(); usize::from(columns)]
}

/// Builds a screen-sized cell grid initialized to blank leading cells.
fn blank_cells(size: Size) -> Vec<Vec<TerminalScreenCell>> {
    blank_screen_cells(size)
}

/// Builds one row initialized to blank leading cells.
fn blank_row(columns: u16) -> Vec<TerminalScreenCell> {
    blank_screen_row(columns)
}

/// Carries Alternate Screen State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateScreenState {
    /// Stores the active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active: bool,
    /// Saved normal-screen state restored when alternate mode exits.
    pub(super) saved_normal_screen: Option<SavedNormalScreenState>,
}

/// Saved normal-screen content and cursor state restored after alternate mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SavedNormalScreenState {
    /// Stored visible cell contents for the normal screen.
    pub(super) cells: Vec<Vec<TerminalScreenCell>>,
    /// Stored per-cell renditions for the normal screen.
    pub(super) renditions: Vec<Vec<GraphicRendition>>,
    /// Stored soft-wrap flags for the normal screen.
    pub(super) line_wraps: Vec<bool>,
    /// Stored copy-mode text overrides for the normal screen.
    pub(super) line_copy_texts: Vec<Option<String>>,
    /// Stored visible cursor position for the normal screen.
    pub(super) cursor: Cursor,
    /// Stored cursor visibility requested by the pane application.
    pub(super) cursor_visible: bool,
    /// Stored deferred autowrap state for the normal screen.
    pub(super) wrap_pending: bool,
    /// Stored saved cursor position for later ESC restore operations.
    pub(super) saved_cursor: Option<Cursor>,
    /// Stored active SGR rendition carried by subsequent printable cells.
    pub(super) graphic_rendition: GraphicRendition,
    /// Stored detached-scrollback state for shell-clear restoration behavior.
    pub(super) normal_viewport_detached_from_history: bool,
    /// Stored terminal size for the normal screen.
    pub(super) size: Size,
    /// Stored DEC autowrap mode for the normal screen.
    pub(super) autowrap_enabled: bool,
    /// Stored DEC origin mode for the normal screen.
    pub(super) origin_mode_enabled: bool,
    /// Stored active scroll region for the normal screen.
    pub(super) scroll_region: Option<(usize, usize)>,
}

impl AlternateScreenState {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new() -> Self {
        Self {
            active: false,
            saved_normal_screen: None,
        }
    }

    /// Runs the enter operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enter(&mut self) {
        self.active = true;
    }

    /// Runs the enter with saved normal screen operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn enter_with_saved_normal_screen(&mut self, state: SavedNormalScreenState) {
        if !self.active {
            self.saved_normal_screen = Some(state);
        }
        self.active = true;
    }

    /// Runs the leave operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn leave(&mut self) -> Option<SavedNormalScreenState> {
        self.active = false;
        self.saved_normal_screen.take()
    }

    /// Runs the active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Returns whether normal-screen output should be recorded to history.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn should_record_to_history(&self) -> bool {
        !self.active
    }

    /// Returns whether alternate-screen scroll-off rows should be recorded to
    /// history.
    pub fn should_record_scroll_off_to_history(&self) -> bool {
        !self.active
    }
}

impl Default for AlternateScreenState {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::new()
    }
}

/// Carries Cursor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cursor {
    /// Stores the row value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) row: usize,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) column: usize,
}

/// Carries Parser State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParserState {
    /// Represents the Ground case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ground,
    /// Represents the Escape case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Escape,
    /// Represents the Escape Charset G0 case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EscapeCharsetG0,
    /// Represents the Escape Charset G1 case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EscapeCharsetG1,
    /// Represents the Csi case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Csi,
    /// Represents the Osc case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Osc,
    /// Represents the Osc Escape case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OscEscape,
    /// Represents the Dcs case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Dcs,
    /// Represents the Dcs Escape case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DcsEscape,
}

/// Runs the decode standard base64 utf8 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn decode_standard_base64_utf8(encoded: &str) -> Option<String> {
    let bytes = decode_standard_base64(encoded)?;
    String::from_utf8(bytes).ok()
}

/// Runs the decode standard base64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn decode_standard_base64(encoded: &str) -> Option<Vec<u8>> {
    if encoded == "?" {
        return None;
    }
    let mut output = Vec::with_capacity(encoded.len().saturating_mul(3) / 4);
    let mut quartet = [0u8; 4];
    let mut quartet_len = 0usize;
    for byte in encoded.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => return None,
        };
        quartet[quartet_len] = value;
        quartet_len += 1;
        if quartet_len == 4 {
            push_decoded_base64_quartet(&quartet, &mut output)?;
            quartet_len = 0;
        }
    }
    if quartet_len != 0 {
        return None;
    }
    Some(output)
}

/// Runs the push decoded base64 quartet operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_decoded_base64_quartet(quartet: &[u8; 4], output: &mut Vec<u8>) -> Option<()> {
    if quartet[0] == 64 || quartet[1] == 64 {
        return None;
    }
    output.push((quartet[0] << 2) | (quartet[1] >> 4));
    if quartet[2] != 64 {
        output.push((quartet[1] << 4) | (quartet[2] >> 2));
    }
    if quartet[3] != 64 {
        if quartet[2] == 64 {
            return None;
        }
        output.push((quartet[2] << 6) | quartet[3]);
    }
    Some(())
}

/// Carries Physical Styled Line state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalStyledLine {
    /// Stores the line value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    line: TerminalStyledLine,
    /// Stores the wraps to next value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    wraps_to_next: bool,
}

/// Identifies one normal-screen physical row for copy-text metadata updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalPhysicalLineIndex {
    /// History row index.
    History(usize),
    /// Visible grid row index.
    Visible(usize),
}

/// Captures enough physical-row metadata to find recent logical lines.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalPhysicalLineTarget {
    /// Stores the physical row index this target addresses.
    index: NormalPhysicalLineIndex,
    /// Presented row text after trimming terminal padding.
    text: String,
    /// Whether this physical row wraps into the next physical row.
    wraps_to_next: bool,
}

/// Carries Terminal Screen state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScreen {
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) size: Size,
    /// Stores the cells value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cells: Vec<Vec<TerminalScreenCell>>,
    /// Stores the renditions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) renditions: Vec<Vec<GraphicRendition>>,
    /// Stores the line wraps value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) line_wraps: Vec<bool>,
    /// Optional raw-copy text associated with visible normal-screen rows.
    ///
    /// Entries are kept parallel to `cells` and `renditions`; `None` means copy
    /// mode should use the presented line text.
    pub(super) line_copy_texts: Vec<Option<String>>,
    /// Optional styled line prefix repeated on soft-wrapped continuation rows.
    ///
    /// The prefix is presentation policy supplied by the product layer. The
    /// terminal parser recognizes it only when the first physical row carries
    /// explicit styling, so ordinary pane output remains unaffected.
    pub(super) wrap_continuation_prefix: Option<String>,
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor: Cursor,
    /// Stores whether the pane application has requested cursor visibility.
    ///
    /// The value follows DEC private mode 25 (`CSI ?25h` / `CSI ?25l`) and is
    /// separate from Mezzanine-owned prompt and overlay cursor presentation.
    pub(super) cursor_visible: bool,
    /// Whether DEC autowrap mode is active.
    ///
    /// The value follows DEC private mode 7 (`CSI ?7h` / `CSI ?7l`). When it
    /// is disabled, printing at the right margin stays on the last column
    /// instead of deferring a wrap to the next printable character.
    pub(super) autowrap_enabled: bool,
    /// Stores the wrap pending value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) wrap_pending: bool,
    /// Stores the saved cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) saved_cursor: Option<Cursor>,
    /// Stores the parser state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) parser_state: ParserState,
    /// Stores the csi buffer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) csi_buffer: String,
    /// Whether the current CSI sequence exceeded the bounded parser buffer.
    ///
    /// Once set, the parser ignores bytes until the final byte and then drops
    /// the complete malformed sequence rather than dispatching truncated state.
    pub(super) csi_buffer_truncated: bool,
    /// Stores the osc buffer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) osc_buffer: String,
    /// Whether the current OSC payload exceeded the bounded parser buffer.
    ///
    /// Once set, the parser keeps consuming bytes until the OSC terminator but
    /// drops the whole payload instead of dispatching truncated content.
    pub(super) osc_buffer_truncated: bool,
    /// Stores the osc events value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) osc_events: Vec<TerminalOscEvent>,
    /// Terminal-generated response bytes that must be written back to the pane.
    pub(super) terminal_response_bytes: Vec<u8>,
    /// Stores the title value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) title: Option<String>,
    /// Stores the graphic rendition value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) graphic_rendition: GraphicRendition,
    /// Stores the bracketed paste enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bracketed_paste_enabled: bool,
    /// Stores the DECSET 1000 normal mouse tracking value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) normal_mouse_tracking_enabled: bool,
    /// Stores the DECSET 1002 button-event mouse tracking value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) button_event_mouse_tracking_enabled: bool,
    /// Stores the DECSET 1003 any-event mouse tracking value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) any_event_mouse_tracking_enabled: bool,
    /// Stores the sgr mouse enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sgr_mouse_enabled: bool,
    /// Stores the application cursor enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) application_cursor_enabled: bool,
    /// Stores the origin mode enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) origin_mode_enabled: bool,
    /// Whether ANSI line-feed/new-line mode is active.
    ///
    /// When active, LF behaves like CRLF for line-oriented shell output. When
    /// reset with `CSI 20 l`, LF preserves the current column and behaves like
    /// VT index.
    pub(super) line_feed_newline_enabled: bool,
    /// Stores the application keypad enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) application_keypad_enabled: bool,
    /// Stores the focus events enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) focus_events_enabled: bool,
    /// Stores the saved dec private modes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) saved_dec_private_modes: BTreeMap<u16, bool>,
    /// Stores the scroll region value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scroll_region: Option<(usize, usize)>,
    /// Stores the alternate value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) alternate: AlternateScreenState,
    /// Monotonic generation advanced by every effective screen-buffer switch.
    ///
    /// Retained renderers use this value to reject diffs whose baseline came
    /// from a different normal or alternate screen generation.
    pub(super) alternate_screen_generation: u64,
    /// Stores the history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) history: HistoryBuffer,
    /// Whether the normal-screen viewport was detached from scrollback by a
    /// full-screen clear such as shell `Ctrl+L`.
    pub(super) normal_viewport_detached_from_history: bool,
    /// Stores the activity events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) activity_events: u64,
    /// Stores the bell events value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bell_events: u64,
    /// Charset designated into the G0 slot.
    pub(super) g0_charset: TerminalCharset,
    /// Charset designated into the G1 slot.
    pub(super) g1_charset: TerminalCharset,
    /// Whether SO currently invokes G1 into GL.
    pub(super) shift_out: bool,
    /// Stores incomplete UTF-8 bytes retained across `feed` calls.
    ///
    /// PTY reads can split one multibyte scalar across separate input chunks.
    /// The decoder must retain that trailing prefix instead of emitting a
    /// replacement character before the remaining bytes arrive.
    pub(super) utf8_tail: Vec<u8>,
}

mod cells;
mod content;
mod editing;
mod lifecycle;
mod parser;
mod state;
mod wrap;

/// Builds a screen-sized rendition grid initialized to one rendition value.
fn blank_renditions(size: Size, rendition: GraphicRendition) -> Vec<Vec<GraphicRendition>> {
    (0..size.rows)
        .map(|_| blank_rendition_row(size.columns, rendition))
        .collect()
}

/// Builds one rendition row initialized to one rendition value.
fn blank_rendition_row(columns: u16, rendition: GraphicRendition) -> Vec<GraphicRendition> {
    vec![rendition; usize::from(columns)]
}

/// Builds a visible styled line from one terminal row and optional copy source.
fn styled_line_from_row_with_copy_text(
    cells: &[TerminalScreenCell],
    renditions: &[GraphicRendition],
    copy_text: Option<String>,
) -> TerminalStyledLine {
    let visible_columns = cells
        .iter()
        .zip(renditions.iter())
        .rposition(|(cell, rendition)| {
            cell.is_written() || !cell.is_blank() || *rendition != GraphicRendition::default()
        })
        .map(|index| index.saturating_add(1))
        .unwrap_or_default();
    let limited_cells = &cells[..visible_columns.min(cells.len())];
    let limited_renditions = &renditions[..visible_columns.min(renditions.len())];
    let text = collect_screen_cell_text(limited_cells, false);
    let mut style_spans = Vec::new();
    let mut cell = 0usize;
    let mut display_column = 0usize;

    while cell < limited_cells.len() {
        let rendition = limited_renditions
            .get(cell)
            .copied()
            .unwrap_or_else(GraphicRendition::default);

        if limited_cells[cell].continuation {
            cell = cell.saturating_add(1);
            continue;
        }

        let span_start = display_column;
        let mut span_width = limited_cells[cell].width().max(1);
        cell = cell.saturating_add(1);

        while cell < limited_cells.len()
            && limited_renditions
                .get(cell)
                .copied()
                .unwrap_or_else(GraphicRendition::default)
                == rendition
        {
            if limited_cells[cell].continuation {
                cell = cell.saturating_add(1);
                continue;
            }
            span_width = span_width.saturating_add(limited_cells[cell].width().max(1));
            cell = cell.saturating_add(1);
        }

        if rendition != GraphicRendition::default() {
            style_spans.push(TerminalStyleSpan {
                start: span_start,
                length: span_width,
                rendition,
            });
        }

        display_column = display_column.saturating_add(span_width);
    }

    TerminalStyledLine {
        text,
        style_spans,
        copy_text,
    }
}

/// Collects leading screen-cell text while omitting continuation sentinels.
fn collect_screen_cell_text(
    cells: &[TerminalScreenCell],
    trim_trailing_whitespace: bool,
) -> String {
    let mut output = String::new();
    for cell in cells {
        if !cell.continuation {
            output.push_str(&cell.text);
        }
    }
    if trim_trailing_whitespace {
        output = output.trim_end().to_string();
    }
    output
}

/// Collects one visible row as plain text, trimming terminal padding.
fn trim_screen_row(cells: &[TerminalScreenCell]) -> String {
    let visible_columns = cells
        .iter()
        .rposition(|cell| !cell.is_blank())
        .map(|index| index.saturating_add(1))
        .unwrap_or_default();
    collect_screen_cell_text(&cells[..visible_columns.min(cells.len())], true)
}

/// Runs the write styled line to row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_styled_line_to_row(
    line: &TerminalStyledLine,
    cells: &mut [TerminalScreenCell],
    renditions: &mut [GraphicRendition],
) {
    let columns = cells.len();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(&line.text) {
        let width = terminal_grapheme_width(grapheme);
        if width == 0 || column.saturating_add(width) > columns {
            break;
        }
        let rendition = styled_line_rendition_at(line, column);
        cells[column] = TerminalScreenCell::text(grapheme);
        renditions[column] = rendition;
        for offset in 1..width {
            cells[column.saturating_add(offset)] = TerminalScreenCell::continuation();
            renditions[column.saturating_add(offset)] = rendition;
        }
        column = column.saturating_add(width);
    }
    for span in &line.style_spans {
        let start = span.start.min(columns);
        let end = span.start.saturating_add(span.length).min(columns);
        for rendition in renditions.iter_mut().take(end).skip(start) {
            *rendition = span.rendition;
        }
    }
}

/// Runs the merge wrapped physical lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn merge_wrapped_physical_lines(
    rows: &[PhysicalStyledLine],
    continuation_prefix: Option<&str>,
) -> Vec<TerminalStyledLine> {
    let mut logical_lines = Vec::new();
    let mut current: Option<TerminalStyledLine> = None;
    let mut current_has_continuation_prefix = false;
    for row in rows {
        let starts_logical_line = current.is_none();
        if starts_logical_line {
            current_has_continuation_prefix = continuation_prefix.is_some_and(|prefix| {
                continuation_prefix_from_styled_line(&row.line, prefix).is_some()
            });
        }
        let current_line = current.get_or_insert_with(|| TerminalStyledLine::plain(String::new()));
        let source_line = if !starts_logical_line && current_has_continuation_prefix {
            continuation_prefix
                .and_then(|prefix| strip_continuation_prefix_from_styled_line(&row.line, prefix))
                .unwrap_or_else(|| row.line.clone())
        } else {
            row.line.clone()
        };
        append_styled_line(current_line, &source_line);
        if !row.wraps_to_next {
            logical_lines.push(
                current
                    .take()
                    .unwrap_or_else(|| TerminalStyledLine::plain("")),
            );
            current_has_continuation_prefix = false;
        }
    }
    if let Some(line) = current {
        logical_lines.push(line);
    }
    if logical_lines.is_empty() {
        logical_lines.push(TerminalStyledLine::plain(""));
    }
    logical_lines
}

/// Runs the append styled line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn append_styled_line(target: &mut TerminalStyledLine, source: &TerminalStyledLine) {
    let offset = styled_line_width(target);
    target.text.push_str(&source.text);
    match (&mut target.copy_text, &source.copy_text) {
        (Some(target_copy), Some(source_copy)) => target_copy.push_str(source_copy),
        (None, Some(source_copy)) => target.copy_text = Some(source_copy.clone()),
        _ => {}
    }
    target
        .style_spans
        .extend(source.style_spans.iter().map(|span| {
            let mut span = *span;
            span.start = span.start.saturating_add(offset);
            span
        }));
}

/// Runs the reflow logical lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn reflow_logical_lines(
    lines: &[TerminalStyledLine],
    columns: usize,
    continuation_prefix: Option<&str>,
) -> Vec<PhysicalStyledLine> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    for line in lines {
        reflow_one_logical_line(line, columns, continuation_prefix, &mut rows);
    }
    rows
}

/// Reflows one logical line into physical rows, preserving a configured
/// styled prefix on every soft-wrapped continuation row.
fn reflow_one_logical_line(
    line: &TerminalStyledLine,
    columns: usize,
    continuation_prefix: Option<&str>,
    rows: &mut Vec<PhysicalStyledLine>,
) {
    let continuation_prefix = continuation_prefix
        .and_then(|prefix| continuation_prefix_from_styled_line(line, prefix))
        .filter(|prefix| prefix_width(prefix) < columns);
    let mut source_column = 0usize;
    let mut current = TerminalStyledLine::plain(String::new());
    current.copy_text = line.copy_text.clone();
    let mut current_width = 0usize;
    for grapheme in terminal_graphemes(&line.text) {
        let width = terminal_grapheme_width(grapheme);
        if width == 0 {
            source_column = source_column.saturating_add(width);
            continue;
        }
        if current_width > 0 && current_width.saturating_add(width) > columns {
            rows.push(PhysicalStyledLine {
                line: current,
                wraps_to_next: true,
            });
            current = TerminalStyledLine::plain(String::new());
            current_width = 0;
            if let Some(prefix) = continuation_prefix.as_deref() {
                push_styled_prefix(&mut current, prefix);
                current_width = prefix_width(prefix);
            }
        }
        let rendition = styled_line_rendition_at(line, source_column);
        push_styled_grapheme(&mut current, grapheme, width, rendition);
        current_width = current_width.saturating_add(width);
        source_column = source_column.saturating_add(width);
    }
    rows.push(PhysicalStyledLine {
        line: current,
        wraps_to_next: false,
    });
}

/// Returns the display width occupied by a styled prefix.
fn prefix_width(prefix: &[StyledPrefixCell]) -> usize {
    prefix.iter().map(|cell| cell.width).sum()
}

/// Copies a styled prefix into a logical row under construction.
fn push_styled_prefix(line: &mut TerminalStyledLine, prefix: &[StyledPrefixCell]) {
    for cell in prefix {
        push_styled_char(line, cell.ch, cell.width, cell.rendition);
    }
}

/// Extracts a configured styled continuation prefix from a styled line.
fn continuation_prefix_from_styled_line(
    line: &TerminalStyledLine,
    configured_prefix: &str,
) -> Option<Vec<StyledPrefixCell>> {
    let mut source_column = 0usize;
    let mut line_chars = line.text.chars();
    let mut prefix = Vec::new();
    for expected in configured_prefix.chars() {
        let ch = line_chars.next()?;
        let width = terminal_char_width(ch);
        if ch != expected || width == 0 {
            return None;
        }
        prefix.push(StyledPrefixCell {
            ch,
            width,
            rendition: styled_line_rendition_at(line, source_column),
        });
        source_column = source_column.saturating_add(width);
    }
    Some(prefix).filter(|prefix| styled_prefix_is_non_default(prefix))
}

/// Removes a configured display-only prefix from a wrapped continuation line
/// before wrapped rows are merged back into their logical line.
fn strip_continuation_prefix_from_styled_line(
    line: &TerminalStyledLine,
    configured_prefix: &str,
) -> Option<TerminalStyledLine> {
    let prefix = continuation_prefix_from_styled_line(line, configured_prefix)?;
    let prefix_width = prefix_width(&prefix);
    let text = line.text.strip_prefix(configured_prefix)?.to_string();
    let style_spans = line
        .style_spans
        .iter()
        .filter_map(|span| {
            let start = span.start.max(prefix_width);
            let end = span.start.saturating_add(span.length);
            (start < end).then_some(TerminalStyleSpan {
                start: start.saturating_sub(prefix_width),
                length: end.saturating_sub(start),
                rendition: span.rendition,
            })
        })
        .collect();
    Some(TerminalStyledLine {
        text,
        style_spans,
        copy_text: line.copy_text.clone(),
    })
}

/// Returns whether a styled prefix carries explicit non-default cell styling.
fn styled_prefix_is_non_default(prefix: &[StyledPrefixCell]) -> bool {
    prefix
        .iter()
        .any(|cell| cell.rendition != GraphicRendition::default())
}

/// Runs the push styled char operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_styled_char(
    line: &mut TerminalStyledLine,
    ch: char,
    width: usize,
    rendition: GraphicRendition,
) {
    let mut buffer = [0; 4];
    let grapheme = ch.encode_utf8(&mut buffer);
    push_styled_grapheme(line, grapheme, width, rendition);
}

/// Pushes one grapheme cluster and its display width onto a styled line.
///
/// # Parameters
/// - `line`: The styled line receiving the grapheme.
/// - `grapheme`: The grapheme cluster to append.
/// - `width`: The display width occupied by the grapheme.
/// - `rendition`: The graphic rendition applied to the grapheme cells.
fn push_styled_grapheme(
    line: &mut TerminalStyledLine,
    grapheme: &str,
    width: usize,
    rendition: GraphicRendition,
) {
    let start = styled_line_width(line);
    line.text.push_str(grapheme);
    if rendition == GraphicRendition::default() {
        return;
    }
    if let Some(last) = line.style_spans.last_mut()
        && last.start.saturating_add(last.length) == start
        && last.rendition == rendition
    {
        last.length = last.length.saturating_add(width);
        return;
    }
    line.style_spans.push(TerminalStyleSpan {
        start,
        length: width,
        rendition,
    });
}

/// Runs the styled line width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_line_width(line: &TerminalStyledLine) -> usize {
    terminal_text_width(&line.text)
}

/// Runs the styled line rendition at operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_line_rendition_at(line: &TerminalStyledLine, column: usize) -> GraphicRendition {
    line.style_spans
        .iter()
        .rev()
        .find(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .map(|span| span.rendition)
        .unwrap_or_default()
}

/// Runs the cursor logical position operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_logical_position(
    rows: &[PhysicalStyledLine],
    source_cursor_row: usize,
    source_cursor_column: usize,
) -> Option<(usize, usize)> {
    let mut logical_line = 0usize;
    let mut logical_column = 0usize;
    for (row_index, row) in rows.iter().enumerate() {
        if row_index == source_cursor_row {
            return Some((
                logical_line,
                logical_column.saturating_add(source_cursor_column),
            ));
        }
        logical_column = logical_column.saturating_add(styled_line_width(&row.line));
        if !row.wraps_to_next {
            logical_line = logical_line.saturating_add(1);
            logical_column = 0;
        }
    }
    None
}

/// Runs the physical position for logical cursor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn physical_position_for_logical_cursor(
    logical_lines: &[TerminalStyledLine],
    logical_line: usize,
    logical_column: usize,
    columns: usize,
    continuation_prefix: Option<&str>,
) -> (usize, usize) {
    let columns = columns.max(1);
    let row_offset = logical_lines
        .iter()
        .take(logical_line)
        .map(|line| physical_row_count_for_line(line, columns, continuation_prefix))
        .sum::<usize>();
    let (line_row, column) = physical_position_in_line(
        logical_lines.get(logical_line),
        logical_column,
        columns,
        continuation_prefix,
    );
    (row_offset.saturating_add(line_row), column)
}

/// Returns the physical row count for one logical line after reflow.
fn physical_row_count_for_line(
    line: &TerminalStyledLine,
    columns: usize,
    continuation_prefix: Option<&str>,
) -> usize {
    let mut rows = Vec::new();
    reflow_one_logical_line(line, columns.max(1), continuation_prefix, &mut rows);
    rows.len().max(1)
}

/// Maps a logical cursor column inside one logical line to its reflowed
/// physical row and column.
fn physical_position_in_line(
    line: Option<&TerminalStyledLine>,
    logical_column: usize,
    columns: usize,
    continuation_prefix: Option<&str>,
) -> (usize, usize) {
    let Some(line) = line else {
        return (0, 0);
    };
    let continuation_prefix = continuation_prefix
        .and_then(|prefix| continuation_prefix_from_styled_line(line, prefix))
        .filter(|prefix| prefix_width(prefix) < columns);
    let mut row = 0usize;
    let mut source_column = 0usize;
    let mut current_width = 0usize;
    for grapheme in terminal_graphemes(&line.text) {
        if source_column >= logical_column {
            return (row, current_width.min(columns.saturating_sub(1)));
        }
        let width = terminal_grapheme_width(grapheme);
        if width == 0 {
            continue;
        }
        if current_width > 0 && current_width.saturating_add(width) > columns {
            row = row.saturating_add(1);
            current_width = continuation_prefix
                .as_deref()
                .map(prefix_width)
                .unwrap_or(0);
        }
        current_width = current_width.saturating_add(width);
        source_column = source_column.saturating_add(width);
    }
    while source_column < logical_column {
        if current_width >= columns {
            row = row.saturating_add(1);
            current_width = continuation_prefix
                .as_deref()
                .map(prefix_width)
                .unwrap_or(0);
        }
        current_width = current_width.saturating_add(1);
        source_column = source_column.saturating_add(1);
    }
    (row, current_width.min(columns.saturating_sub(1)))
}

/// Runs the csi count operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn csi_count(params: &str) -> usize {
    first_csi_param(params).max(1)
}

/// Runs the first csi param operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn first_csi_param(params: &str) -> usize {
    params
        .split(';')
        .next()
        .filter(|part| !part.is_empty())
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(0)
}

/// Runs the sgr params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sgr_params(params: &str) -> Vec<u16> {
    if params.is_empty() {
        return vec![0];
    }

    params
        .split(';')
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect()
}

/// Runs the parse extended sgr color operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_extended_sgr_color(params: &[u16]) -> Option<(TerminalColor, usize)> {
    match params {
        [5, color, ..] => Some((TerminalColor::Indexed((*color).min(255) as u8), 2)),
        [2, red, green, blue, ..] => Some((
            TerminalColor::Rgb(
                (*red).min(255) as u8,
                (*green).min(255) as u8,
                (*blue).min(255) as u8,
            ),
            4,
        )),
        _ => None,
    }
}
