//! Terminal Screen implementation.
//!
//! This module owns the terminal screen boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_COPY_SKIP_LINE, BTreeMap, DEFAULT_HISTORY_ROTATE_LINES, HistoryBuffer, MezError, Result,
    Size, terminal_char_width, terminal_grapheme_width, terminal_graphemes, terminal_text_width,
};

pub use mez_terminal::{
    GraphicRendition, MAX_OSC_STRING_BYTES, TerminalColor, TerminalCursorState, TerminalModeState,
    TerminalOscEvent, TerminalSavedDecPrivateMode, TerminalSavedState, TerminalStyleSpan,
    TerminalStyledLine, tracked_dec_private_mode,
};

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
}

impl TerminalScreenCell {
    /// Builds a blank leading screen cell.
    fn blank() -> Self {
        Self {
            text: " ".to_string(),
            continuation: false,
        }
    }

    /// Builds a wide-grapheme continuation sentinel.
    fn continuation() -> Self {
        Self {
            text: String::new(),
            continuation: true,
        }
    }

    /// Builds a leading cell containing complete terminal text.
    fn text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            continuation: false,
        }
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

/// Runs the parse mez shell transaction osc operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn parse_mez_shell_transaction_osc(payload: &str) -> Option<TerminalOscEvent> {
    let mut fields = payload.split(';');
    if fields.next()? != "133" {
        return None;
    }
    let kind = fields.next()?;
    match kind {
        "A" => Some(TerminalOscEvent::ShellPromptStart),
        "B" => Some(TerminalOscEvent::ShellPromptEnd),
        "C" => {
            let values = parse_semicolon_key_values(fields);
            if values.contains_key("mez_marker") {
                Some(TerminalOscEvent::ShellTransactionStart {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                })
            } else {
                Some(TerminalOscEvent::ShellCommandOutputStart)
            }
        }
        "D" => {
            let parts = fields.collect::<Vec<_>>();
            let exit_code = parts.first().and_then(|field| field.parse::<i32>().ok());
            let key_value_start = usize::from(exit_code.is_some());
            let values = parse_semicolon_key_values(parts.iter().skip(key_value_start).copied());
            if values.contains_key("mez_marker") {
                Some(TerminalOscEvent::ShellTransactionEnd {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                    exit_code: exit_code?,
                })
            } else {
                Some(TerminalOscEvent::ShellCommandFinished { exit_code })
            }
        }
        _ => None,
    }
}

/// Runs the parse semicolon key values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_semicolon_key_values<'a>(
    fields: impl Iterator<Item = &'a str>,
) -> BTreeMap<&'a str, &'a str> {
    fields
        .filter_map(|field| field.split_once('='))
        .collect::<BTreeMap<_, _>>()
}

/// Runs the required marker field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn required_marker_field(values: &BTreeMap<&str, &str>, key: &str) -> Option<String> {
    values
        .get(key)
        .copied()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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

impl TerminalScreen {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(size: Size, history_limit: usize) -> Result<Self> {
        Self::new_with_history_config(size, history_limit, DEFAULT_HISTORY_ROTATE_LINES)
    }

    /// Builds a terminal screen with explicit history limit and rotation
    /// settings so runtime configuration can control bounded-history eviction.
    pub fn new_with_history_config(
        size: Size,
        history_limit: usize,
        history_rotate_lines: usize,
    ) -> Result<Self> {
        Ok(Self {
            size,
            cells: blank_cells(size),
            renditions: blank_renditions(size, GraphicRendition::default()),
            line_wraps: vec![false; usize::from(size.rows)],
            line_copy_texts: vec![None; usize::from(size.rows)],
            wrap_continuation_prefix: None,
            cursor: Cursor { row: 0, column: 0 },
            cursor_visible: true,
            autowrap_enabled: true,
            wrap_pending: false,
            saved_cursor: None,
            parser_state: ParserState::Ground,
            csi_buffer: String::new(),
            csi_buffer_truncated: false,
            osc_buffer: String::new(),
            osc_buffer_truncated: false,
            osc_events: Vec::new(),
            terminal_response_bytes: Vec::new(),
            title: None,
            graphic_rendition: GraphicRendition::default(),
            bracketed_paste_enabled: false,
            normal_mouse_tracking_enabled: false,
            button_event_mouse_tracking_enabled: false,
            any_event_mouse_tracking_enabled: false,
            sgr_mouse_enabled: false,
            application_cursor_enabled: false,
            origin_mode_enabled: false,
            line_feed_newline_enabled: true,
            application_keypad_enabled: false,
            focus_events_enabled: false,
            saved_dec_private_modes: BTreeMap::new(),
            scroll_region: None,
            alternate: AlternateScreenState::new(),
            history: HistoryBuffer::new_with_rotation(history_limit, history_rotate_lines)
                .map_err(|error| MezError::invalid_args(error.message()))?,
            normal_viewport_detached_from_history: false,
            activity_events: 0,
            bell_events: 0,
            g0_charset: TerminalCharset::Ascii,
            g1_charset: TerminalCharset::Ascii,
            shift_out: false,
            utf8_tail: Vec::new(),
        })
    }

    /// Configures a styled prefix to repeat on soft-wrapped continuation rows.
    ///
    /// An empty prefix disables the policy. Callers must write the same prefix
    /// with a non-default rendition at the start of the logical line for the
    /// screen to recognize it.
    pub fn set_wrap_continuation_prefix(&mut self, prefix: impl Into<String>) {
        let prefix = prefix.into();
        self.wrap_continuation_prefix = (!prefix.is_empty()).then_some(prefix);
    }

    /// Runs the feed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn feed(&mut self, input: &[u8]) {
        if !input.is_empty() {
            self.activity_events = self.activity_events.saturating_add(1);
        }
        let mut bytes = Vec::with_capacity(self.utf8_tail.len().saturating_add(input.len()));
        if !self.utf8_tail.is_empty() {
            bytes.extend_from_slice(&self.utf8_tail);
            self.utf8_tail.clear();
        }
        bytes.extend_from_slice(input);

        let mut offset = 0;
        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(text) => {
                    for ch in text.chars() {
                        self.feed_char(ch);
                    }
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let text = std::str::from_utf8(&bytes[offset..offset + valid_up_to])
                            .expect("valid UTF-8 prefix must decode");
                        for ch in text.chars() {
                            self.feed_char(ch);
                        }
                        offset += valid_up_to;
                    }

                    match error.error_len() {
                        Some(error_len) => {
                            let invalid_end = offset + error_len;
                            let text = String::from_utf8_lossy(&bytes[offset..invalid_end]);
                            for ch in text.chars() {
                                self.feed_char(ch);
                            }
                            offset = invalid_end;
                        }
                        None => {
                            self.utf8_tail.extend_from_slice(&bytes[offset..]);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize(&mut self, size: Size) {
        if self.size == size {
            return;
        }

        if self.alternate.active() {
            self.resize_alternate_screen(size);
            return;
        }

        if !self.alternate.active() && self.scroll_region.is_none() {
            if self.normal_viewport_detached_from_history {
                self.resize_detached_normal_screen(size);
                return;
            }
            if self.normal_screen_viewport_is_cleared() {
                self.resize_cleared_normal_screen(size);
                return;
            }
            if self.size.columns == size.columns {
                self.resize_normal_screen_rows_only(size);
                return;
            }
            self.resize_normal_screen_reflowing(size);
            return;
        }

        self.resize_grid_preserving_cells(size);
    }

    /// Resizes the live alternate screen while preserving its top-left grid.
    ///
    /// Full-screen alternate-buffer applications own their viewport and redraw
    /// against pane coordinates. Resizes therefore keep row zero and column zero
    /// anchored instead of applying normal-screen bottom-preservation or history
    /// reflow heuristics.
    fn resize_alternate_screen(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let mut cells = blank_cells(size);
        let mut renditions = blank_renditions(size, GraphicRendition::default());
        let mut line_wraps = vec![false; new_rows];
        let mut line_copy_texts = vec![None; new_rows];
        let rows = old_rows.min(cells.len());
        let columns = self
            .cells
            .first()
            .map(Vec::len)
            .unwrap_or_default()
            .min(cells.first().map(Vec::len).unwrap_or_default());
        for (row_index, row) in cells.iter_mut().enumerate().take(rows) {
            row[..columns].clone_from_slice(&self.cells[row_index][..columns]);
            renditions[row_index][..columns]
                .copy_from_slice(&self.renditions[row_index][..columns]);
            line_wraps[row_index] = self.line_wraps.get(row_index).copied().unwrap_or(false);
            line_copy_texts[row_index] = self.line_copy_texts.get(row_index).cloned().flatten();
        }

        self.size = size;
        self.cells = cells;
        self.renditions = renditions;
        self.line_wraps = line_wraps;
        self.line_copy_texts = line_copy_texts;
        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self.cursor.row.min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        self.wrap_pending = false;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }
    /// Returns whether the live normal-screen viewport is intentionally blank.
    ///
    /// Pane-local clears such as `Ctrl+L` move visible rows into scrollback and
    /// reset the cursor to the origin. Subsequent resizes must preserve that
    /// cleared viewport instead of pulling scrollback back into view.
    fn normal_screen_viewport_is_cleared(&self) -> bool {
        self.last_significant_row().is_none() && self.cursor.row == 0 && self.cursor.column == 0
    }
    /// Resizes an intentionally cleared normal-screen viewport.
    ///
    /// The resize keeps scrollback untouched and preserves the blank live pane
    /// presentation expected after pane-local clear operations.
    fn resize_cleared_normal_screen(&mut self, size: Size) {
        self.size = size;
        self.clear_screen();
        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Resizes a normal-screen viewport that has been detached from scrollback.
    ///
    /// Shell clears such as `Ctrl+L` erase the live viewport while preserving
    /// scrollback. Until new output scrolls the pane again, row-only resizes
    /// must preserve the exact live viewport position, and width changes must
    /// reflow only the live rows without pulling adjacent history rows back
    /// into the visible grid.
    fn resize_detached_normal_screen(&mut self, size: Size) {
        if self.size.columns == size.columns {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let source_rows = self.current_visible_rows();
        let cursor = cursor_logical_position(&source_rows, self.cursor.row, self.cursor.column);
        let logical_lines =
            merge_wrapped_physical_lines(&source_rows, self.wrap_continuation_prefix.as_deref());
        let physical_rows = reflow_logical_lines(
            &logical_lines,
            usize::from(size.columns),
            self.wrap_continuation_prefix.as_deref(),
        );
        let visible_start = if preserve_bottom || physical_rows.len() > new_rows {
            physical_rows.len().saturating_sub(new_rows)
        } else {
            0
        };

        self.size = size;
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in physical_rows
            .iter()
            .skip(visible_start)
            .take(new_rows)
            .enumerate()
        {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some((logical_line, logical_column)) = cursor {
            let (absolute_row, column) = physical_position_for_logical_cursor(
                &logical_lines,
                logical_line,
                logical_column,
                usize::from(size.columns),
                self.wrap_continuation_prefix.as_deref(),
            );
            self.cursor.row = absolute_row.saturating_sub(visible_start).min(max_row);
            self.cursor.column = column.min(max_column);
        } else {
            self.cursor.row = self.cursor.row.min(max_row);
            self.cursor.column = self.cursor.column.min(max_column);
        }
        self.wrap_pending = false;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Runs the resize grid preserving cells operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn resize_grid_preserving_cells(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let row_offset = if preserve_bottom {
            old_rows.saturating_sub(new_rows)
        } else {
            0
        };
        let mut cells = blank_cells(size);
        let mut renditions = blank_renditions(size, GraphicRendition::default());
        let mut line_wraps = vec![false; usize::from(size.rows)];
        let mut line_copy_texts = vec![None; usize::from(size.rows)];
        let rows = old_rows.saturating_sub(row_offset).min(cells.len());
        let columns = self
            .cells
            .first()
            .map(Vec::len)
            .unwrap_or_default()
            .min(cells.first().map(Vec::len).unwrap_or_default());
        for (row_index, row) in cells.iter_mut().enumerate().take(rows) {
            let source_row = row_index.saturating_add(row_offset);
            row[..columns].clone_from_slice(&self.cells[source_row][..columns]);
            renditions[row_index][..columns]
                .copy_from_slice(&self.renditions[source_row][..columns]);
            line_wraps[row_index] = self.line_wraps.get(source_row).copied().unwrap_or(false);
            line_copy_texts[row_index] = self.line_copy_texts.get(source_row).cloned().flatten();
        }

        // Commit content-bearing dropped rows to history so shrink content is preserved.
        if new_rows < old_rows && self.alternate.should_record_to_history() {
            let dropped_rows = if preserve_bottom {
                0..row_offset
            } else {
                new_rows..old_rows
            };
            for row in dropped_rows {
                let copy_text = self.line_copy_texts.get(row).cloned().flatten();
                if copy_text.is_some() || self.cells[row].iter().any(|cell| !cell.is_blank()) {
                    self.history.push_styled_line_with_wrap(
                        styled_line_from_row_with_copy_text(
                            &self.cells[row],
                            &self.renditions[row],
                            copy_text,
                        ),
                        self.line_wraps.get(row).copied().unwrap_or(false),
                    );
                }
            }
        }

        self.size = size;
        self.cells = cells;
        self.renditions = renditions;
        self.line_wraps = line_wraps;
        self.line_copy_texts = line_copy_texts;
        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self.cursor.row.saturating_sub(row_offset).min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        self.wrap_pending = false;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.saturating_sub(row_offset).min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }
    /// Resizes a normal screen when only the row count changes.
    ///
    /// With a stable column count the physical wrap boundaries do not change.
    /// Pane growth must keep the currently rendered viewport stationary, while
    /// pane shrink may bottom-anchor only when the visible tail would otherwise
    /// be truncated.
    fn resize_normal_screen_rows_only(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        if new_rows > old_rows {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let live_bottom = self
            .last_significant_row()
            .map(|row| row.max(self.cursor.row))
            .unwrap_or(self.cursor.row);
        if new_rows < old_rows && live_bottom < new_rows {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let preserve_bottom = new_rows < old_rows && live_bottom >= new_rows;
        if new_rows == old_rows {
            self.size = size;
            return;
        }

        let mut visible_rows = self.current_visible_rows();
        let visible_len = visible_rows.len();
        let visible_start = if preserve_bottom || visible_len > new_rows {
            visible_len.saturating_sub(new_rows)
        } else {
            0
        };
        let moved_to_history = visible_start;
        let retained_visible = visible_rows.len().saturating_sub(visible_start);
        let pulled_from_history = new_rows.saturating_sub(retained_visible);
        let history_append_rows = visible_rows.drain(..moved_to_history).collect::<Vec<_>>();

        let mut next_visible_rows = Vec::with_capacity(new_rows);
        let mut restored_history_rows = Vec::with_capacity(pulled_from_history);
        for _ in 0..pulled_from_history {
            let Some((line, wraps_to_next)) = self.history.pop_styled_line() else {
                break;
            };
            restored_history_rows.push(PhysicalStyledLine {
                line,
                wraps_to_next,
            });
        }
        restored_history_rows.reverse();
        let restored_history_row_count = restored_history_rows.len();
        next_visible_rows.extend(restored_history_rows);
        next_visible_rows.extend(visible_rows);

        self.size = size;
        for row in &history_append_rows {
            self.history
                .push_styled_line_with_wrap(row.line.clone(), row.wraps_to_next);
        }
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in next_visible_rows.iter().take(new_rows).enumerate() {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self
            .cursor
            .row
            .saturating_add(restored_history_row_count)
            .saturating_sub(moved_to_history)
            .min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        self.wrap_pending = false;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor
                .row
                .saturating_add(restored_history_row_count)
                .saturating_sub(moved_to_history)
                .min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Reflows live normal-screen rows after a width-changing resize.
    ///
    /// Resize latency must not scale with the configured scrollback limit, and
    /// resizing must not pull retained scrollback into the live viewport. Only
    /// rows that were visible before the resize participate in synchronous
    /// reflow; older history remains stored in its existing physical row form.
    fn resize_normal_screen_reflowing(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let source_rows = self.current_visible_rows();
        let cursor = cursor_logical_position(&source_rows, self.cursor.row, self.cursor.column);
        let logical_lines =
            merge_wrapped_physical_lines(&source_rows, self.wrap_continuation_prefix.as_deref());
        let physical_rows = reflow_logical_lines(
            &logical_lines,
            usize::from(size.columns),
            self.wrap_continuation_prefix.as_deref(),
        );
        let visible_start = if preserve_bottom || physical_rows.len() > new_rows {
            physical_rows.len().saturating_sub(new_rows)
        } else {
            0
        };

        self.size = size;
        for row in physical_rows.iter().take(visible_start) {
            self.history
                .push_styled_line_with_wrap(row.line.clone(), row.wraps_to_next);
        }
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in physical_rows
            .iter()
            .skip(visible_start)
            .take(new_rows)
            .enumerate()
        {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some((logical_line, logical_column)) = cursor {
            let (absolute_row, column) = physical_position_for_logical_cursor(
                &logical_lines,
                logical_line,
                logical_column,
                usize::from(size.columns),
                self.wrap_continuation_prefix.as_deref(),
            );
            self.cursor.row = absolute_row.saturating_sub(visible_start).min(max_row);
            self.cursor.column = column.min(max_column);
        } else {
            self.cursor.row = self.cursor.row.min(max_row);
            self.cursor.column = self.cursor.column.min(max_column);
        }
        self.wrap_pending = false;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }
    /// Returns the currently visible physical rows with their style metadata.
    fn current_visible_rows(&self) -> Vec<PhysicalStyledLine> {
        let last_visible_row = self
            .last_significant_row()
            .map(|row| row.max(self.cursor.row))
            .unwrap_or(self.cursor.row)
            .min(self.cells.len().saturating_sub(1));
        (0..=last_visible_row)
            .map(|row| PhysicalStyledLine {
                line: styled_line_from_row_with_copy_text(
                    &self.cells[row],
                    &self.renditions[row],
                    self.line_copy_texts.get(row).cloned().flatten(),
                ),
                wraps_to_next: self.line_wraps.get(row).copied().unwrap_or(false),
            })
            .collect()
    }

    /// Returns normal-screen physical rows with addresses for copy metadata.
    fn normal_physical_line_targets(&self) -> Vec<NormalPhysicalLineTarget> {
        let mut targets = self
            .history
            .styled_lines_with_wraps()
            .enumerate()
            .map(|(index, (line, wraps_to_next))| NormalPhysicalLineTarget {
                index: NormalPhysicalLineIndex::History(index),
                text: line.text,
                wraps_to_next,
            })
            .collect::<Vec<_>>();
        if !self.alternate.active() {
            targets.extend(self.visible_styled_lines().into_iter().enumerate().map(
                |(index, line)| NormalPhysicalLineTarget {
                    index: NormalPhysicalLineIndex::Visible(index),
                    text: line.text,
                    wraps_to_next: self.line_wraps.get(index).copied().unwrap_or(false),
                },
            ));
        }
        targets
    }

    /// Updates the raw-copy text associated with one normal physical row.
    fn assign_normal_physical_copy_text(
        &mut self,
        index: NormalPhysicalLineIndex,
        copy_text: Option<String>,
    ) {
        match index {
            NormalPhysicalLineIndex::History(row) => {
                self.history.set_copy_text(row, copy_text);
            }
            NormalPhysicalLineIndex::Visible(row) => {
                if let Some(slot) = self.line_copy_texts.get_mut(row) {
                    *slot = copy_text;
                }
            }
        }
    }

    /// Clears raw-copy metadata for a visible row after terminal mutation.
    fn clear_line_copy_text(&mut self, row: usize) {
        if let Some(copy_text) = self.line_copy_texts.get_mut(row) {
            *copy_text = None;
        }
    }

    /// Returns the leading cell column for the grapheme occupying `column`.
    fn leading_column_for_cell(&self, row: usize, column: usize) -> Option<usize> {
        let row_cells = self.cells.get(row)?;
        if row_cells.is_empty() {
            return None;
        }
        let mut leading_column = column.min(row_cells.len().saturating_sub(1));
        while leading_column > 0
            && row_cells
                .get(leading_column)
                .is_some_and(|cell| cell.continuation)
        {
            leading_column = leading_column.saturating_sub(1);
        }
        Some(leading_column)
    }

    /// Clears the complete grapheme footprint touching one display column.
    fn clear_cell_footprint(&mut self, row: usize, column: usize, rendition: GraphicRendition) {
        let Some(leading_column) = self.leading_column_for_cell(row, column) else {
            return;
        };
        let width = self.cells[row][leading_column].width().max(1);
        let end = leading_column
            .saturating_add(width)
            .min(self.cells[row].len());
        for clear_column in leading_column..end {
            self.cells[row][clear_column] = TerminalScreenCell::blank();
            self.renditions[row][clear_column] = rendition;
        }
    }

    /// Repairs continuation sentinels after column insertion or deletion.
    fn repair_row_continuations(&mut self, row: usize) {
        let Some(row_cells) = self.cells.get(row) else {
            return;
        };
        let columns = row_cells.len();
        let mut column = 0usize;
        while column < columns {
            if self.cells[row][column].continuation {
                self.cells[row][column] = TerminalScreenCell::blank();
                column = column.saturating_add(1);
                continue;
            }
            let width = self.cells[row][column].width();
            if width <= 1 {
                column = column.saturating_add(1);
                continue;
            }
            if column.saturating_add(width) > columns {
                for clear_column in column..columns {
                    self.cells[row][clear_column] = TerminalScreenCell::blank();
                }
                break;
            }
            let rendition = self.renditions[row][column];
            for offset in 1..width {
                self.cells[row][column.saturating_add(offset)] = TerminalScreenCell::continuation();
                self.renditions[row][column.saturating_add(offset)] = rendition;
            }
            column = column.saturating_add(width);
        }
    }

    /// Extends the previous leading cell when a scalar completes its grapheme.
    fn try_extend_previous_grapheme(&mut self, ch: char) -> bool {
        let Some(row_cells) = self.cells.get(self.cursor.row) else {
            return false;
        };
        if row_cells.is_empty() {
            return false;
        }
        let start_column = if self.wrap_pending {
            self.cursor.column.min(row_cells.len().saturating_sub(1))
        } else if let Some(column) = self.cursor.column.checked_sub(1) {
            column.min(row_cells.len().saturating_sub(1))
        } else {
            return false;
        };
        let Some(leading_column) = self.leading_column_for_cell(self.cursor.row, start_column)
        else {
            return false;
        };
        if self.cells[self.cursor.row][leading_column].is_blank() {
            return false;
        }
        let mut candidate = self.cells[self.cursor.row][leading_column].text.clone();
        candidate.push(ch);
        let mut graphemes = terminal_graphemes(&candidate);
        if graphemes.next() != Some(candidate.as_str()) || graphemes.next().is_some() {
            return false;
        }
        let old_width = self.cells[self.cursor.row][leading_column].width().max(1);
        let new_width = terminal_grapheme_width(&candidate);
        if new_width == 0 || leading_column.saturating_add(new_width) > row_cells.len() {
            return false;
        }

        self.clear_line_copy_text(self.cursor.row);
        self.cells[self.cursor.row][leading_column] = TerminalScreenCell::text(&candidate);
        let rendition = self.renditions[self.cursor.row][leading_column];
        for offset in 1..new_width {
            self.cells[self.cursor.row][leading_column.saturating_add(offset)] =
                TerminalScreenCell::continuation();
            self.renditions[self.cursor.row][leading_column.saturating_add(offset)] = rendition;
        }
        for offset in new_width..old_width {
            let column = leading_column.saturating_add(offset);
            if column < self.cells[self.cursor.row].len() {
                self.cells[self.cursor.row][column] = TerminalScreenCell::blank();
                self.renditions[self.cursor.row][column] = rendition;
            }
        }
        let next_column = leading_column.saturating_add(new_width);
        if next_column > self.max_column() {
            self.cursor.column = leading_column;
            self.wrap_pending = true;
        } else {
            self.cursor.column = next_column;
            self.wrap_pending = false;
        }
        true
    }

    /// Runs the last significant row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn last_significant_row(&self) -> Option<usize> {
        self.cells
            .iter()
            .zip(self.renditions.iter())
            .rposition(|(cells, renditions)| {
                cells
                    .iter()
                    .zip(renditions.iter())
                    .any(|(cell, rendition)| {
                        !cell.is_blank() || *rendition != GraphicRendition::default()
                    })
            })
    }

    /// Returns the current terminal grid dimensions.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Runs the visible lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn visible_lines(&self) -> Vec<String> {
        self.cells.iter().map(|row| trim_screen_row(row)).collect()
    }

    /// Returns visible lines with non-default SGR style spans preserved.
    pub fn visible_styled_lines(&self) -> Vec<TerminalStyledLine> {
        self.cells
            .iter()
            .zip(self.renditions.iter())
            .enumerate()
            .map(|(row, (cells, renditions))| {
                styled_line_from_row_with_copy_text(
                    cells,
                    renditions,
                    self.line_copy_texts.get(row).cloned().flatten(),
                )
            })
            .collect()
    }

    /// Runs the normal content lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn normal_content_lines(&self) -> Vec<String> {
        let mut lines = self
            .history
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !self.alternate.active() {
            lines.extend(self.visible_lines());
        }
        lines
    }

    /// Returns normal-screen history and visible rows with SGR style spans.
    pub fn normal_styled_content_lines(&self) -> Vec<TerminalStyledLine> {
        let mut lines = self.history.styled_lines().collect::<Vec<_>>();
        if !self.alternate.active() {
            lines.extend(self.visible_styled_lines());
        }
        lines
    }

    /// Assigns raw-copy text to the most recent normal-screen logical lines.
    ///
    /// Presentation renderers use this after feeding transformed display text
    /// into the terminal screen. Copy mode can then recover the source text
    /// even when the visible line has been styled or simplified for display.
    pub fn set_recent_normal_copy_texts(&mut self, copy_texts: &[String]) {
        if copy_texts.is_empty() || self.alternate.active() {
            return;
        }
        let mut targets = self.normal_physical_line_targets();
        while targets
            .last()
            .is_some_and(|target| !target.wraps_to_next && target.text.trim().is_empty())
        {
            targets.pop();
        }

        let mut target_end = targets.len();
        for copy_text in copy_texts.iter().rev() {
            if target_end == 0 {
                break;
            }
            let mut start = target_end.saturating_sub(1);
            while start > 0 && targets[start.saturating_sub(1)].wraps_to_next {
                start = start.saturating_sub(1);
            }
            self.assign_normal_physical_copy_text(targets[start].index, Some(copy_text.clone()));
            for target in targets
                .iter()
                .take(target_end)
                .skip(start.saturating_add(1))
            {
                self.assign_normal_physical_copy_text(
                    target.index,
                    Some(AGENT_COPY_SKIP_LINE.to_string()),
                );
            }
            target_end = start;
        }
    }

    /// Runs the history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn history(&self) -> &HistoryBuffer {
        &self.history
    }

    /// Runs the history limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn history_limit(&self) -> usize {
        self.history.limit()
    }

    /// Returns the configured history rotation batch size.
    pub fn history_rotate_lines(&self) -> usize {
        self.history.rotate_lines()
    }

    /// Runs the set history limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_history_limit(&mut self, limit: usize) -> Result<()> {
        self.history
            .set_limit(limit)
            .map_err(|error| MezError::invalid_args(error.message()))
    }

    /// Updates the history rotation batch size.
    pub fn set_history_rotate_lines(&mut self, rotate_lines: usize) -> Result<()> {
        self.history
            .set_rotate_lines(rotate_lines)
            .map_err(|error| MezError::invalid_args(error.message()))
    }

    /// Runs the clear history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.normal_viewport_detached_from_history = false;
    }

    /// Scrolls the used normal-screen viewport into history and blanks it.
    ///
    /// Pane-local UI clears, such as entering agent mode or handling `Ctrl+L`,
    /// should remove existing text from the live viewport without erasing it
    /// from copyable pane logs. Alternate-screen contents are intentionally not
    /// recorded to normal history.
    pub fn clear_visible_into_history(&mut self) {
        if self.alternate.active() {
            self.clear_screen();
            return;
        }
        if let Some(last_row) = self.last_significant_row() {
            for row in 0..=last_row {
                self.history.push_styled_line_with_wrap(
                    styled_line_from_row_with_copy_text(
                        &self.cells[row],
                        &self.renditions[row],
                        self.line_copy_texts.get(row).cloned().flatten(),
                    ),
                    self.line_wraps.get(row).copied().unwrap_or(false),
                );
            }
        }
        self.clear_screen();
        self.normal_viewport_detached_from_history = true;
    }

    /// Restores plain normal-screen history and styled visible rows.
    ///
    /// Snapshot resume uses this path to rebuild a non-live pane's rendered
    /// terminal contents without replaying the original PTY byte stream when
    /// the persisted history source has no style metadata.
    pub fn restore_normal_styled_content(
        &mut self,
        history_lines: &[String],
        visible_lines: &[TerminalStyledLine],
    ) {
        let history_lines = history_lines
            .iter()
            .map(|line| TerminalStyledLine::plain(line.clone()))
            .collect::<Vec<_>>();
        self.restore_normal_styled_history_content(&history_lines, visible_lines);
    }

    /// Restores styled normal-screen history and visible rows.
    pub fn restore_normal_styled_history_content(
        &mut self,
        history_lines: &[TerminalStyledLine],
        visible_lines: &[TerminalStyledLine],
    ) {
        self.history.clear();
        for line in history_lines {
            self.history.push_styled_line(line.clone());
        }

        self.alternate = AlternateScreenState::new();
        self.cells = blank_cells(self.size);
        self.renditions = blank_renditions(self.size, GraphicRendition::default());
        self.line_wraps = vec![false; usize::from(self.size.rows)];
        self.line_copy_texts = vec![None; usize::from(self.size.rows)];
        self.cursor = Cursor { row: 0, column: 0 };
        self.cursor_visible = true;
        self.wrap_pending = false;
        self.saved_cursor = None;
        self.parser_state = ParserState::Ground;
        self.csi_buffer.clear();
        self.osc_buffer.clear();
        self.osc_buffer_truncated = false;
        self.osc_events.clear();
        self.bracketed_paste_enabled = false;
        self.normal_mouse_tracking_enabled = false;
        self.button_event_mouse_tracking_enabled = false;
        self.any_event_mouse_tracking_enabled = false;
        self.sgr_mouse_enabled = false;
        self.application_cursor_enabled = false;
        self.origin_mode_enabled = false;
        self.application_keypad_enabled = false;
        self.focus_events_enabled = false;
        self.g0_charset = TerminalCharset::Ascii;
        self.g1_charset = TerminalCharset::Ascii;
        self.shift_out = false;
        self.saved_dec_private_modes.clear();
        self.scroll_region = None;
        self.normal_viewport_detached_from_history = false;

        let rows = usize::from(self.size.rows);
        let start = visible_lines.len().saturating_sub(rows);
        for (row_index, line) in visible_lines.iter().skip(start).take(rows).enumerate() {
            write_styled_line_to_row(
                line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_copy_texts[row_index] = line.copy_text.clone();
        }
    }

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
    fn active_charset(&self) -> TerminalCharset {
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
    fn report_device_status(&mut self, params: &str) {
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
    fn saved_normal_screen_state(&self) -> SavedNormalScreenState {
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
    fn restore_saved_normal_screen_state(&mut self, state: SavedNormalScreenState) {
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
    fn apply_dec_private_mode(&mut self, mode: u16, enabled: bool) {
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
                    self.clear_screen();
                } else if let Some(state) = self.alternate.leave() {
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
    fn dec_private_mode_enabled(&self, mode: u16) -> Option<bool> {
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

    /// Runs the print operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn print(&mut self, ch: char) {
        if self.try_extend_previous_grapheme(ch) {
            return;
        }
        let translated = match self.active_charset() {
            TerminalCharset::Ascii => ch,
            TerminalCharset::DecSpecialGraphics => dec_special_graphics_char(ch).unwrap_or(ch),
        };
        let text = translated.to_string();
        let width = terminal_grapheme_width(&text);
        if width == 0 {
            return;
        }
        if self.wrap_pending {
            self.wrap_to_next_line();
        }
        if self.autowrap_enabled
            && self.cursor.column.saturating_add(width).saturating_sub(1) > self.max_column()
        {
            self.wrap_to_next_line();
        }
        self.clear_line_copy_text(self.cursor.row);
        let row = self.cursor.row;
        let column = self.cursor.column;
        if !self.autowrap_enabled && column.saturating_add(width) > self.cells[row].len() {
            for target_column in column..self.cells[row].len() {
                self.clear_cell_footprint(row, target_column, self.graphic_rendition);
            }
            self.cursor.column = self.max_column();
            self.wrap_pending = false;
            return;
        }
        for target_column in column..column.saturating_add(width).min(self.cells[row].len()) {
            self.clear_cell_footprint(row, target_column, self.graphic_rendition);
        }
        self.cells[row][column] = TerminalScreenCell::text(&text);
        self.renditions[row][column] = self.graphic_rendition;
        for offset in 1..width {
            let column = column.saturating_add(offset);
            if column <= self.max_column() {
                self.cells[row][column] = TerminalScreenCell::continuation();
                self.renditions[row][column] = self.graphic_rendition;
            }
        }
        let next_column = self.cursor.column.saturating_add(width);
        if next_column > self.max_column() {
            if !self.autowrap_enabled {
                self.cursor.column = self.max_column();
                self.wrap_pending = false;
                return;
            }
            self.wrap_pending = true;
        } else {
            self.cursor.column = next_column;
        }
    }

    /// Runs the newline operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn newline(&mut self) {
        self.next_line();
    }

    /// Runs the index operation for this subsystem.
    ///
    /// The function keeps VT-style vertical movement separate from carriage
    /// return so LF and IND can preserve the current column while NEL and the
    /// legacy `newline` helper can still move to the first column.
    pub(super) fn index(&mut self) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row == bottom {
            self.scroll_region_up_from(top, bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_add(1).min(bottom);
        }
    }

    /// Runs the next-line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn next_line(&mut self) {
        self.wrap_pending = false;
        self.cursor.column = 0;
        self.index();
    }

    /// Runs the wrap to next line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn wrap_to_next_line(&mut self) {
        let continuation_prefix = self.current_wrap_continuation_prefix();
        if let Some(wraps) = self.line_wraps.get_mut(self.cursor.row) {
            *wraps = true;
        }
        self.newline();
        self.wrap_pending = false;
        if let Some(prefix) = continuation_prefix {
            self.write_wrap_continuation_prefix(&prefix);
        }
    }

    /// Runs the reverse index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn reverse_index(&mut self) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row == top {
            self.scroll_region_down_from(top, bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    /// Runs the active scroll region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_scroll_region(&self) -> (usize, usize) {
        self.scroll_region.unwrap_or((0, self.max_row()))
    }

    /// Runs the scroll region up operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_up(&mut self, count: usize) {
        let (top, bottom) = self.active_scroll_region();
        self.scroll_region_up_from(top, bottom, count);
    }

    /// Runs the scroll region down operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_down(&mut self, count: usize) {
        let (top, bottom) = self.active_scroll_region();
        self.scroll_region_down_from(top, bottom, count);
    }

    /// Runs the scroll region up from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_up_from(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom || bottom > self.max_row() {
            return;
        }
        let count = count.min(bottom.saturating_sub(top).saturating_add(1));
        for _ in 0..count {
            if top == 0
                && bottom == self.max_row()
                && self.alternate.should_record_scroll_off_to_history()
            {
                self.normal_viewport_detached_from_history = false;
                self.history.push_styled_line_with_wrap(
                    styled_line_from_row_with_copy_text(
                        &self.cells[0],
                        &self.renditions[0],
                        self.line_copy_texts.first().cloned().flatten(),
                    ),
                    self.line_wraps.first().copied().unwrap_or(false),
                );
            }
            self.cells.remove(top);
            self.renditions.remove(top);
            self.line_wraps.remove(top);
            self.line_copy_texts.remove(top);
            self.cells.insert(bottom, blank_row(self.size.columns));
            self.renditions.insert(
                bottom,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(bottom, false);
            self.line_copy_texts.insert(bottom, None);
        }
    }

    /// Runs the scroll region down from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_down_from(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom || bottom > self.max_row() {
            return;
        }
        let count = count.min(bottom.saturating_sub(top).saturating_add(1));
        for _ in 0..count {
            self.cells.remove(bottom);
            self.renditions.remove(bottom);
            self.line_wraps.remove(bottom);
            self.line_copy_texts.remove(bottom);
            self.cells.insert(top, blank_row(self.size.columns));
            self.renditions.insert(
                top,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(top, false);
            self.line_copy_texts.insert(top, None);
        }
    }

    /// Runs the insert blank chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_blank_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let column = self.cursor.column;
        let width = usize::from(self.size.columns);
        let count = count.min(width.saturating_sub(column));
        self.clear_line_copy_text(row);
        if count > 0
            && self.cells[row]
                .get(column)
                .is_some_and(|cell| cell.continuation)
        {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
        }
        for _ in 0..count {
            self.cells[row].insert(column, TerminalScreenCell::blank());
            self.renditions[row].insert(column, self.graphic_rendition);
            self.cells[row].truncate(width);
            self.renditions[row].truncate(width);
        }
        self.repair_row_continuations(row);
    }

    /// Runs the delete chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn delete_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let column = self.cursor.column;
        let width = usize::from(self.size.columns);
        let count = count.min(width.saturating_sub(column));
        self.clear_line_copy_text(row);
        if count > 0
            && self.cells[row]
                .get(column)
                .is_some_and(|cell| cell.continuation)
        {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
        }
        for _ in 0..count {
            self.cells[row].remove(column);
            self.renditions[row].remove(column);
            self.cells[row].push(TerminalScreenCell::blank());
            self.renditions[row].push(self.graphic_rendition);
        }
        self.repair_row_continuations(row);
    }

    /// Runs the erase chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let start = self.cursor.column;
        let end = start
            .saturating_add(count.saturating_sub(1))
            .min(self.max_column());
        for column in start..=end {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
            self.cells[row][column] = TerminalScreenCell::blank();
            self.renditions[row][column] = self.graphic_rendition;
        }
        self.clear_line_copy_text(row);
    }

    /// Runs the insert lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_lines(&mut self, count: usize) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }
        let count = count.min(bottom.saturating_sub(self.cursor.row).saturating_add(1));
        for _ in 0..count {
            self.cells
                .insert(self.cursor.row, blank_row(self.size.columns));
            self.renditions.insert(
                self.cursor.row,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(self.cursor.row, false);
            self.line_copy_texts.insert(self.cursor.row, None);
            self.cells.remove(bottom.saturating_add(1));
            self.renditions.remove(bottom.saturating_add(1));
            self.line_wraps.remove(bottom.saturating_add(1));
            self.line_copy_texts.remove(bottom.saturating_add(1));
        }
    }

    /// Runs the delete lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn delete_lines(&mut self, count: usize) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }
        let count = count.min(bottom.saturating_sub(self.cursor.row).saturating_add(1));
        for _ in 0..count {
            self.cells.remove(self.cursor.row);
            self.renditions.remove(self.cursor.row);
            self.line_wraps.remove(self.cursor.row);
            self.line_copy_texts.remove(self.cursor.row);
            self.cells.insert(bottom, blank_row(self.size.columns));
            self.renditions.insert(
                bottom,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(bottom, false);
            self.line_copy_texts.insert(bottom, None);
        }
    }

    /// Runs the set scroll region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_scroll_region(&mut self, params: &str) {
        if params.is_empty() {
            self.scroll_region = None;
            self.cursor = Cursor { row: 0, column: 0 };
            self.wrap_pending = false;
            return;
        }
        let mut parts = params.split(';');
        let top = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let bottom = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or_else(|| usize::from(self.size.rows))
            .saturating_sub(1)
            .min(self.max_row());
        if top < bottom {
            self.scroll_region = Some((top, bottom));
            self.cursor = Cursor {
                row: if self.origin_mode_enabled { top } else { 0 },
                column: 0,
            };
            self.wrap_pending = false;
        }
    }

    /// Runs the move cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor(&mut self, params: &str) {
        self.wrap_pending = false;
        let mut parts = params.split(';');
        let row = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1);
        let column = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1);
        self.cursor.row = if self.origin_mode_enabled {
            let (top, bottom) = self.active_scroll_region();
            top.saturating_add(row.saturating_sub(1)).min(bottom)
        } else {
            row.saturating_sub(1).min(self.max_row())
        };
        self.cursor.column = column.saturating_sub(1).min(self.max_column());
    }

    /// Runs the move cursor column operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_column(&mut self, params: &str) {
        self.wrap_pending = false;
        let column = first_csi_param(params).max(1);
        self.cursor.column = column.saturating_sub(1).min(self.max_column());
    }

    /// Runs the move cursor row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_row(&mut self, params: &str) {
        self.wrap_pending = false;
        let row = first_csi_param(params).max(1);
        self.cursor.row = if self.origin_mode_enabled {
            let (top, bottom) = self.active_scroll_region();
            top.saturating_add(row.saturating_sub(1)).min(bottom)
        } else {
            row.saturating_sub(1).min(self.max_row())
        };
    }

    /// Runs the move cursor next line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_next_line(&mut self, params: &str) {
        self.move_cursor_relative(params, 1, 0);
        self.cursor.column = 0;
    }

    /// Runs the move cursor previous line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_previous_line(&mut self, params: &str) {
        self.move_cursor_relative(params, -1, 0);
        self.cursor.column = 0;
    }

    /// Runs the move cursor relative operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_relative(
        &mut self,
        params: &str,
        row_direction: isize,
        column_direction: isize,
    ) {
        self.wrap_pending = false;
        let amount = csi_count(params);
        let (min_row, max_row) = if self.origin_mode_enabled {
            self.active_scroll_region()
        } else {
            (0, self.max_row())
        };
        if row_direction < 0 {
            self.cursor.row = self.cursor.row.saturating_sub(amount).max(min_row);
        } else if row_direction > 0 {
            self.cursor.row = self.cursor.row.saturating_add(amount).min(max_row);
        }

        if column_direction < 0 {
            self.cursor.column = self.cursor.column.saturating_sub(amount);
        } else if column_direction > 0 {
            self.cursor.column = self
                .cursor
                .column
                .saturating_add(amount)
                .min(self.max_column());
        }
    }

    /// Runs the clear screen operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_screen(&mut self) {
        self.cells = blank_cells(self.size);
        self.renditions = blank_renditions(self.size, self.graphic_rendition);
        self.line_wraps = vec![false; usize::from(self.size.rows)];
        self.line_copy_texts = vec![None; usize::from(self.size.rows)];
        self.cursor = Cursor { row: 0, column: 0 };
        self.wrap_pending = false;
    }

    /// Runs the erase display operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_display(&mut self, params: &str) {
        self.wrap_pending = false;
        match first_csi_param(params) {
            0 => {
                self.erase_line_range(self.cursor.row, self.cursor.column, self.max_column());
                for row in self.cursor.row.saturating_add(1)..=self.max_row() {
                    self.erase_line_range(row, 0, self.max_column());
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.erase_line_range(row, 0, self.max_column());
                }
                self.erase_line_range(self.cursor.row, 0, self.cursor.column);
            }
            2 => {
                for row in 0..=self.max_row() {
                    self.erase_line_range(row, 0, self.max_column());
                }
                if !self.alternate.active() {
                    self.normal_viewport_detached_from_history = true;
                }
            }
            3 if !self.alternate.active() => {
                self.history.clear();
                self.normal_viewport_detached_from_history = false;
            }
            _ => {}
        }
    }

    /// Runs the erase line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_line(&mut self, params: &str) {
        self.wrap_pending = false;
        match first_csi_param(params) {
            0 => self.erase_line_range(self.cursor.row, self.cursor.column, self.max_column()),
            1 => self.erase_line_range(self.cursor.row, 0, self.cursor.column),
            2 => self.erase_line_range(self.cursor.row, 0, self.max_column()),
            _ => {}
        }
    }

    /// Runs the erase line range operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_line_range(&mut self, row: usize, start_column: usize, end_column: usize) {
        let end_column = end_column.min(self.max_column());
        for column in start_column.min(end_column)..=end_column {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
            self.cells[row][column] = TerminalScreenCell::blank();
            self.renditions[row][column] = self.graphic_rendition;
        }
        self.clear_line_copy_text(row);
        if end_column == self.max_column()
            && let Some(wraps) = self.line_wraps.get_mut(row)
        {
            *wraps = false;
        }
    }

    /// Runs the save cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
    }

    /// Runs the restore cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn restore_cursor(&mut self) {
        if let Some(cursor) = self.saved_cursor {
            self.cursor = Cursor {
                row: cursor.row.min(self.max_row()),
                column: cursor.column.min(self.max_column()),
            };
            self.wrap_pending = false;
        }
    }

    /// Runs the apply sgr operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_sgr(&mut self, params: &str) {
        let values = sgr_params(params);
        let mut index = 0;
        while index < values.len() {
            match values[index] {
                0 => self.graphic_rendition = GraphicRendition::default(),
                1 => self.graphic_rendition.bold = true,
                2 => self.graphic_rendition.dim = true,
                3 => self.graphic_rendition.italic = true,
                4 => self.graphic_rendition.underline = true,
                7 => self.graphic_rendition.inverse = true,
                8 => self.graphic_rendition.hidden = true,
                9 => self.graphic_rendition.strikethrough = true,
                21 => {
                    self.graphic_rendition.underline = true;
                    self.graphic_rendition.double_underline = true;
                }
                22 => {
                    self.graphic_rendition.bold = false;
                    self.graphic_rendition.dim = false;
                }
                23 => self.graphic_rendition.italic = false,
                24 => {
                    self.graphic_rendition.underline = false;
                    self.graphic_rendition.double_underline = false;
                }
                27 => self.graphic_rendition.inverse = false,
                28 => self.graphic_rendition.hidden = false,
                29 => self.graphic_rendition.strikethrough = false,
                30..=37 => {
                    self.graphic_rendition.foreground =
                        Some(TerminalColor::Indexed((values[index] - 30) as u8));
                }
                39 => self.graphic_rendition.foreground = None,
                40..=47 => {
                    self.graphic_rendition.background =
                        Some(TerminalColor::Indexed((values[index] - 40) as u8));
                }
                49 => self.graphic_rendition.background = None,
                90..=97 => {
                    self.graphic_rendition.foreground =
                        Some(TerminalColor::Indexed((values[index] - 90 + 8) as u8));
                }
                100..=107 => {
                    self.graphic_rendition.background =
                        Some(TerminalColor::Indexed((values[index] - 100 + 8) as u8));
                }
                38 | 48 => {
                    if let Some((color, consumed)) = parse_extended_sgr_color(&values[index + 1..])
                    {
                        if values[index] == 38 {
                            self.graphic_rendition.foreground = Some(color);
                        } else {
                            self.graphic_rendition.background = Some(color);
                        }
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    /// Runs the max row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn max_row(&self) -> usize {
        usize::from(self.size.rows.saturating_sub(1))
    }

    /// Runs the max column operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn max_column(&self) -> usize {
        usize::from(self.size.columns.saturating_sub(1))
    }

    /// Returns the configured styled prefix for the current wrapped logical
    /// line when the first physical row matches the configured policy.
    fn current_wrap_continuation_prefix(&self) -> Option<Vec<StyledPrefixCell>> {
        let prefix = self.wrap_continuation_prefix.as_deref()?;
        if usize::from(self.size.columns) <= terminal_text_width(prefix) {
            return None;
        }
        let mut row = self.cursor.row.min(self.cells.len().saturating_sub(1));
        while row > 0 && self.line_wraps.get(row.saturating_sub(1)).copied() == Some(true) {
            row = row.saturating_sub(1);
        }
        self.wrap_continuation_prefix_from_row(row, prefix)
    }

    /// Reads the configured styled continuation prefix from one visible row.
    fn wrap_continuation_prefix_from_row(
        &self,
        row: usize,
        configured_prefix: &str,
    ) -> Option<Vec<StyledPrefixCell>> {
        let cells = self.cells.get(row)?;
        let renditions = self.renditions.get(row)?;
        let mut column = 0usize;
        let mut prefix = Vec::new();
        for expected in configured_prefix.chars() {
            let width = terminal_char_width(expected);
            let cell = cells.get(column)?;
            if width == 0 || cell.continuation || cell.text != expected.to_string() {
                return None;
            }
            prefix.push(StyledPrefixCell {
                ch: expected,
                width,
                rendition: renditions.get(column).copied().unwrap_or_default(),
            });
            column = column.saturating_add(width);
        }
        styled_prefix_is_non_default(&prefix).then_some(prefix)
    }

    /// Writes a display-only continuation prefix at the cursor after a soft
    /// wrap without changing the current SGR state for the wrapped content.
    fn write_wrap_continuation_prefix(&mut self, prefix: &[StyledPrefixCell]) {
        if prefix_width(prefix) >= usize::from(self.size.columns) {
            return;
        }
        for cell in prefix {
            if self
                .cursor
                .column
                .saturating_add(cell.width)
                .saturating_sub(1)
                > self.max_column()
            {
                return;
            }
            self.clear_line_copy_text(self.cursor.row);
            let text = cell.ch.to_string();
            self.cells[self.cursor.row][self.cursor.column] = TerminalScreenCell::text(&text);
            self.renditions[self.cursor.row][self.cursor.column] = cell.rendition;
            for offset in 1..cell.width {
                let column = self.cursor.column.saturating_add(offset);
                self.cells[self.cursor.row][column] = TerminalScreenCell::continuation();
                self.renditions[self.cursor.row][column] = cell.rendition;
            }
            self.cursor.column = self.cursor.column.saturating_add(cell.width);
        }
    }
}

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
            !cell.is_blank() || *rendition != GraphicRendition::default()
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
