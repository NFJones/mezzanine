//! Neutral overlay records.

use mez_terminal::TerminalStyleSpan;

use crate::copy::CopyPosition;
use crate::record_browser::RecordBrowser;

/// Actor-owned full-window display overlay state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayOverlay<Source> {
    /// Unprefixed overlay content rows.
    pub lines: Vec<String>,
    /// Visible styles for each content row.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// First visible content row.
    pub scroll_offset: usize,
    /// Search query currently being edited.
    pub search_input: Option<String>,
    /// Last submitted search query.
    pub search_query: Option<String>,
    /// Last matched text range.
    pub search_match: Option<OverlaySearchMatch>,
    /// Transient search feedback.
    pub search_status: Option<String>,
    /// Active mouse text selection in content coordinates.
    pub mouse_selection: Option<(CopyPosition, CopyPosition)>,
    /// Selectable command rows.
    pub selections: Vec<OverlaySelection>,
    /// Active index into `selections`.
    pub active_selection_index: Option<usize>,
    /// Whether any input dismisses this overlay.
    pub dismiss_on_any_input: bool,
    /// Optional interactive record-browser state.
    pub record_browser: Option<RecordBrowserOverlayState<Source>>,
}

/// Retained state for one interactive record-browser overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowserOverlayState<Source> {
    /// Pane whose product shell opened the browser.
    pub pane_id: String,
    /// Product command backing the browser.
    pub command: String,
    /// Product query context used to refresh records.
    pub source: Option<Source>,
    /// Backend-neutral browser state.
    pub browser: RecordBrowser,
    /// Parent views restored when leaving the current view.
    pub stack: Vec<RecordBrowserOverlayFrame<Source>>,
}

/// One preserved record-browser view below the active overlay frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowserOverlayFrame<Source> {
    /// Product command backing the preserved browser.
    pub command: String,
    /// Product query context retained for refreshes.
    pub source: Option<Source>,
    /// Backend-neutral preserved browser.
    pub browser: RecordBrowser,
    /// Scroll offset restored with the view.
    pub scroll_offset: usize,
    /// Active selection restored with the view.
    pub active_selection_index: Option<usize>,
}

/// Render-cell range for one submitted overlay search match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlaySearchMatch {
    /// Zero-based content line containing the match.
    pub line_index: usize,
    /// Zero-based display column where the match begins.
    pub start_column: usize,
    /// Display-cell width of the matched text.
    pub width: usize,
}

/// One selectable command-output overlay range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlaySelection {
    /// Zero-based content line containing the selection.
    pub line_index: usize,
    /// Display column where the interactive range begins.
    pub start_column: usize,
    /// Display-cell width of the interactive range.
    pub width: usize,
    /// Opaque product command executed on selection.
    pub command: String,
    /// Visual importance of this action.
    pub kind: OverlaySelectionKind,
}

/// Visual category for one command-output overlay choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlaySelectionKind {
    /// Routine primary action.
    Primary,
    /// Secondary action.
    Secondary,
    /// Destructive or authority-changing action.
    Danger,
}
