//! Modal overlay and anchored-selector input transitions.
//!
//! This module owns product-independent decoding and deterministic state
//! reduction for mux overlay controls. Callers retain terminal transport and
//! execute returned command or selection intents; the reducer only mutates
//! overlay/selector state and reports typed outcomes.

use crate::layout::Size;

use super::{
    AnchoredSelector, DisplayOverlay, apply_overlay_scroll_delta, clamp_overlay_scroll,
    overlay_next_search_match, overlay_selection_index_is_visible, scroll_overlay_to_line,
};

/// Display-overlay navigation action decoded from terminal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayInputAction {
    /// Exit the overlay or active search editor.
    Exit,
    /// Enter command-output pager search editing.
    StartSearch,
    /// Append printable text to the active pager search query.
    EditSearchText,
    /// Delete the previous character from the active pager search query.
    EditSearchBackspace,
    /// Select the currently active row or submit search.
    SelectActive,
    /// Move selection to the previous selectable row.
    SelectPrevious,
    /// Move selection to the next selectable row.
    SelectNext,
    /// Move to the first selectable row.
    SelectFirst,
    /// Move to the last selectable row.
    SelectLast,
    /// Scroll the overlay by the signed row delta.
    ScrollBy(isize),
    /// Ignore this input for overlay purposes.
    Ignore,
}

/// Selector navigation action shared by anchored and embedded controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorInputAction {
    /// Close the active selector without applying a value.
    Exit,
    /// Apply the active selector value.
    Select,
    /// Move to the previous selector item.
    Previous,
    /// Move to the next selector item.
    Next,
    /// Move to the first selector item.
    First,
    /// Move to the last selector item.
    Last,
    /// Input is not selector navigation.
    Ignore,
}

/// Decodes raw terminal input into one modal-overlay action.
///
/// Printable UTF-8 is reported as search-editor input without retaining the
/// bytes. The caller supplies the original text to `apply_overlay_input`, so
/// the mux reducer remains independent from product transport buffering.
pub fn overlay_input_action(input: &[u8]) -> OverlayInputAction {
    if input == b"q" {
        return OverlayInputAction::Exit;
    }
    if input == b"/" {
        return OverlayInputAction::StartSearch;
    }
    if input == b"\x7f" || input == b"\x08" {
        return OverlayInputAction::EditSearchBackspace;
    }
    if std::str::from_utf8(input)
        .is_ok_and(|text| !text.is_empty() && text.chars().all(|ch| !ch.is_control()))
    {
        return OverlayInputAction::EditSearchText;
    }
    match selector_input_action(input) {
        SelectorInputAction::Exit => OverlayInputAction::Exit,
        SelectorInputAction::Select => OverlayInputAction::SelectActive,
        SelectorInputAction::Previous => OverlayInputAction::SelectPrevious,
        SelectorInputAction::Next => OverlayInputAction::SelectNext,
        SelectorInputAction::First => OverlayInputAction::SelectFirst,
        SelectorInputAction::Last => OverlayInputAction::SelectLast,
        SelectorInputAction::Ignore => match input {
            b"\x1b[5~" => OverlayInputAction::ScrollBy(-10),
            b"\x1b[6~" => OverlayInputAction::ScrollBy(10),
            _ => OverlayInputAction::Ignore,
        },
    }
}

/// Decodes raw terminal input into one anchored-selector action.
pub fn selector_input_action(input: &[u8]) -> SelectorInputAction {
    match input {
        b"\x1b" | b"\x03" => SelectorInputAction::Exit,
        b"\r" | b"\n" => SelectorInputAction::Select,
        b"\x1b[A" | b"\x1bOA" | b"\x1b[D" | b"\x1bOD" => SelectorInputAction::Previous,
        b"\x1b[B" | b"\x1bOB" | b"\x1b[C" | b"\x1bOC" => SelectorInputAction::Next,
        b"\x1b[H" | b"\x1b[1~" => SelectorInputAction::First,
        b"\x1b[F" | b"\x1b[4~" => SelectorInputAction::Last,
        _ => SelectorInputAction::Ignore,
    }
}

/// Typed effect or state result from one overlay action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayInputOutcome {
    /// Caller should close the overlay.
    Close,
    /// Caller should execute the selected opaque command.
    Invoke { command: String },
    /// Mux-owned overlay state changed.
    Updated,
    /// The action was recognized but did not change state.
    Unchanged,
    /// The action is not meaningful for the current overlay state.
    Ignored,
}

/// Typed effect or state result from one selector action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorInputOutcome {
    /// Caller should close the selector.
    Close,
    /// Caller should apply the active item.
    Select { index: usize },
    /// Mux-owned selector state changed.
    Updated,
    /// The action was recognized but did not change state.
    Unchanged,
    /// The input is not a selector action.
    Ignored,
}

/// Moves a bounded selector index by one signed delta.
pub fn selector_step_index(active: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let active = active.min(len.saturating_sub(1));
    if delta.is_negative() {
        active.saturating_sub(delta.unsigned_abs())
    } else {
        active
            .saturating_add(usize::try_from(delta).unwrap_or(usize::MAX))
            .min(len.saturating_sub(1))
    }
}

/// Applies one generic selector action and returns caller-owned effect intent.
pub fn apply_selector_input<Field>(
    selector: &mut AnchoredSelector<Field>,
    action: SelectorInputAction,
    visible_rows: usize,
) -> SelectorInputOutcome {
    match action {
        SelectorInputAction::Exit => SelectorInputOutcome::Close,
        SelectorInputAction::Select if selector.items.get(selector.active_index).is_some() => {
            SelectorInputOutcome::Select {
                index: selector.active_index,
            }
        }
        SelectorInputAction::Select => SelectorInputOutcome::Unchanged,
        SelectorInputAction::Previous => {
            selector_movement_outcome(move_selector(selector, -1, visible_rows))
        }
        SelectorInputAction::Next => {
            selector_movement_outcome(move_selector(selector, 1, visible_rows))
        }
        SelectorInputAction::First => {
            selector_movement_outcome(set_selector_index(selector, 0, visible_rows))
        }
        SelectorInputAction::Last => {
            let last_index = selector.items.len().saturating_sub(1);
            selector_movement_outcome(set_selector_index(selector, last_index, visible_rows))
        }
        SelectorInputAction::Ignore => SelectorInputOutcome::Ignored,
    }
}

/// Moves one selector highlight and keeps it inside the viewport.
pub fn move_selector<Field>(
    selector: &mut AnchoredSelector<Field>,
    delta: isize,
    visible_rows: usize,
) -> bool {
    let previous = selector.active_index;
    selector.active_index = selector_step_index(previous, selector.items.len(), delta);
    keep_selector_active_visible(selector, visible_rows);
    selector.active_index != previous
}

/// Sets one bounded selector highlight and keeps it inside the viewport.
pub fn set_selector_index<Field>(
    selector: &mut AnchoredSelector<Field>,
    index: usize,
    visible_rows: usize,
) -> bool {
    let previous = selector.active_index;
    selector.active_index = index.min(selector.items.len().saturating_sub(1));
    keep_selector_active_visible(selector, visible_rows);
    selector.active_index != previous
}

/// Scrolls an anchored selector without changing its active item.
pub fn scroll_selector<Field>(
    selector: &mut AnchoredSelector<Field>,
    lines: isize,
    visible_rows: usize,
) -> bool {
    let previous = selector.scroll_offset;
    let max_offset = selector.items.len().saturating_sub(visible_rows.max(1));
    if lines.is_negative() {
        selector.scroll_offset = selector.scroll_offset.saturating_sub(lines.unsigned_abs());
    } else {
        selector.scroll_offset = selector
            .scroll_offset
            .saturating_add(lines as usize)
            .min(max_offset);
    }
    selector.scroll_offset != previous
}

/// Keeps the active selector item within a bounded viewport.
pub fn keep_selector_active_visible<Field>(
    selector: &mut AnchoredSelector<Field>,
    visible_rows: usize,
) {
    let visible_rows = visible_rows.max(1);
    let max_offset = selector.items.len().saturating_sub(visible_rows);
    if selector.active_index < selector.scroll_offset {
        selector.scroll_offset = selector.active_index;
    } else if selector.active_index >= selector.scroll_offset.saturating_add(visible_rows) {
        selector.scroll_offset = selector
            .active_index
            .saturating_add(1)
            .saturating_sub(visible_rows);
    }
    selector.scroll_offset = selector.scroll_offset.min(max_offset);
}

/// Applies one generic overlay action and returns caller-owned command intent.
pub fn apply_overlay_input<Source>(
    overlay: &mut DisplayOverlay<Source>,
    action: OverlayInputAction,
    input_text: Option<&str>,
    input_present: bool,
    size: Size,
) -> OverlayInputOutcome {
    if overlay.dismiss_on_any_input && input_present {
        return OverlayInputOutcome::Close;
    }
    if overlay.search_input.is_some() {
        return apply_overlay_search_input(overlay, action, input_text, size);
    }
    match action {
        OverlayInputAction::Exit => OverlayInputOutcome::Close,
        OverlayInputAction::StartSearch => {
            overlay.search_input = Some(String::new());
            overlay.search_status = None;
            OverlayInputOutcome::Updated
        }
        OverlayInputAction::SelectActive => active_overlay_command(overlay, size)
            .map(|command| OverlayInputOutcome::Invoke { command })
            .unwrap_or(OverlayInputOutcome::Unchanged),
        OverlayInputAction::SelectPrevious => {
            overlay_changed_outcome(move_overlay_selection(overlay, -1, size))
        }
        OverlayInputAction::SelectNext => {
            overlay_changed_outcome(move_overlay_selection(overlay, 1, size))
        }
        OverlayInputAction::SelectFirst => {
            overlay_changed_outcome(set_overlay_selection_index(overlay, 0, size))
        }
        OverlayInputAction::SelectLast => {
            let last_index = overlay.selections.len().saturating_sub(1);
            overlay_changed_outcome(set_overlay_selection_index(overlay, last_index, size))
        }
        OverlayInputAction::ScrollBy(delta) => {
            overlay_changed_outcome(apply_overlay_scroll_delta(overlay, delta, size))
        }
        OverlayInputAction::EditSearchText
        | OverlayInputAction::EditSearchBackspace
        | OverlayInputAction::Ignore => OverlayInputOutcome::Ignored,
    }
}

/// Applies input while the overlay search editor is active.
fn apply_overlay_search_input<Source>(
    overlay: &mut DisplayOverlay<Source>,
    action: OverlayInputAction,
    input_text: Option<&str>,
    size: Size,
) -> OverlayInputOutcome {
    match action {
        OverlayInputAction::Exit => {
            overlay.search_input = None;
            overlay.search_status = None;
            OverlayInputOutcome::Updated
        }
        OverlayInputAction::SelectActive => {
            submit_overlay_search(overlay, size);
            OverlayInputOutcome::Updated
        }
        OverlayInputAction::EditSearchBackspace => {
            let changed = overlay
                .search_input
                .as_mut()
                .is_some_and(|input| input.pop().is_some());
            overlay_changed_outcome(changed)
        }
        OverlayInputAction::EditSearchText => {
            let Some(text) = input_text.filter(|text| !text.is_empty()) else {
                return OverlayInputOutcome::Unchanged;
            };
            let Some(search_input) = overlay.search_input.as_mut() else {
                return OverlayInputOutcome::Ignored;
            };
            search_input.push_str(text);
            OverlayInputOutcome::Updated
        }
        OverlayInputAction::StartSearch
        | OverlayInputAction::SelectPrevious
        | OverlayInputAction::SelectNext
        | OverlayInputAction::SelectFirst
        | OverlayInputAction::SelectLast
        | OverlayInputAction::ScrollBy(_)
        | OverlayInputAction::Ignore => OverlayInputOutcome::Ignored,
    }
}

/// Submits or repeats the active overlay search query.
fn submit_overlay_search<Source>(overlay: &mut DisplayOverlay<Source>, size: Size) {
    let submitted = overlay.search_input.take().unwrap_or_default();
    let query = if submitted.is_empty() {
        let Some(query) = overlay.search_query.clone() else {
            overlay.search_status = Some("search: enter a query".to_string());
            return;
        };
        query
    } else {
        overlay.search_query = Some(submitted.clone());
        submitted
    };
    let start_line = overlay
        .search_match
        .map(|search_match| search_match.line_index)
        .or_else(|| overlay.scroll_offset.checked_sub(1))
        .unwrap_or(overlay.scroll_offset);
    let Some(search_match) = overlay_next_search_match(overlay, &query, start_line) else {
        overlay.search_status = Some(format!("pattern not found: {query}"));
        return;
    };
    overlay.search_match = Some(search_match);
    overlay.scroll_offset = search_match.line_index;
    clamp_overlay_scroll(overlay, size);
    overlay.search_status = None;
}

/// Moves overlay selection or plain pager scrolling by one signed delta.
fn move_overlay_selection<Source>(
    overlay: &mut DisplayOverlay<Source>,
    delta: isize,
    size: Size,
) -> bool {
    if overlay.selections.is_empty() {
        return apply_overlay_scroll_delta(overlay, delta, size);
    }
    let previous = overlay
        .active_selection_index
        .unwrap_or(0)
        .min(overlay.selections.len().saturating_sub(1));
    let active_logical_id = overlay.selections[previous].logical_id;
    let next = if delta.is_negative() {
        let target = overlay.selections[..previous]
            .iter()
            .rposition(|selection| selection.logical_id != active_logical_id)
            .unwrap_or(previous);
        let target_logical_id = overlay.selections[target].logical_id;
        overlay
            .selections
            .iter()
            .position(|selection| selection.logical_id == target_logical_id)
            .unwrap_or(target)
    } else {
        overlay.selections[previous.saturating_add(1)..]
            .iter()
            .position(|selection| selection.logical_id != active_logical_id)
            .map(|offset| previous.saturating_add(1).saturating_add(offset))
            .unwrap_or(previous)
    };
    overlay.active_selection_index = Some(next);
    if let Some(line_index) = overlay
        .selections
        .get(next)
        .map(|selection| selection.line_index)
    {
        scroll_overlay_to_line(overlay, line_index, size);
    }
    next != previous
}

/// Sets overlay selection or jumps a plain pager to one boundary.
fn set_overlay_selection_index<Source>(
    overlay: &mut DisplayOverlay<Source>,
    index: usize,
    size: Size,
) -> bool {
    if overlay.selections.is_empty() {
        let next = if index == 0 {
            0
        } else {
            crate::render::modal_overlay_max_scroll(overlay.lines.len(), size)
        };
        let changed = next != overlay.scroll_offset;
        overlay.scroll_offset = next;
        return changed;
    }
    let previous = overlay.active_selection_index.unwrap_or(0);
    let target = index.min(overlay.selections.len().saturating_sub(1));
    let target_logical_id = overlay.selections[target].logical_id;
    let next = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == target_logical_id)
        .unwrap_or(target);
    overlay.active_selection_index = Some(next);
    if let Some(line_index) = overlay
        .selections
        .get(next)
        .map(|selection| selection.line_index)
    {
        scroll_overlay_to_line(overlay, line_index, size);
    }
    next != previous
}

/// Returns a visible active command without executing it.
fn active_overlay_command<Source>(overlay: &DisplayOverlay<Source>, size: Size) -> Option<String> {
    let index = overlay.active_selection_index?;
    if !overlay_selection_index_is_visible(overlay, index, size) {
        return None;
    }
    overlay
        .selections
        .get(index)
        .map(|selection| selection.command.clone())
}

/// Maps a state-change flag to a stable overlay outcome.
fn overlay_changed_outcome(changed: bool) -> OverlayInputOutcome {
    if changed {
        OverlayInputOutcome::Updated
    } else {
        OverlayInputOutcome::Unchanged
    }
}

/// Maps a state-change flag to a stable selector outcome.
fn selector_movement_outcome(changed: bool) -> SelectorInputOutcome {
    if changed {
        SelectorInputOutcome::Updated
    } else {
        SelectorInputOutcome::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{OverlaySelection, OverlaySelectionKind};

    /// Builds neutral overlay state for input transition tests.
    fn overlay(lines: &[&str]) -> DisplayOverlay<()> {
        DisplayOverlay {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            line_style_spans: vec![Vec::new(); lines.len()],
            line_copy_texts: vec![None; lines.len()],
            scroll_offset: 0,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            record_browser: None,
        }
    }

    /// Verifies terminal bytes decode to mux-owned navigation, search, paging,
    /// cancellation, and unknown-input actions before product state is read.
    #[test]
    fn overlay_and_selector_input_decoding_is_product_independent() {
        assert_eq!(
            selector_input_action(b"\x1b[A"),
            SelectorInputAction::Previous
        );
        assert_eq!(
            selector_input_action(b"\x1b[6~"),
            SelectorInputAction::Ignore
        );
        assert_eq!(overlay_input_action(b"/"), OverlayInputAction::StartSearch);
        assert_eq!(
            overlay_input_action(b"\x1b[6~"),
            OverlayInputAction::ScrollBy(10)
        );
        assert_eq!(overlay_input_action(b"\0"), OverlayInputAction::Ignore);
    }

    /// Verifies selector transitions clamp empty and boundary state, keep the
    /// active item visible, and return selection intent without product calls.
    #[test]
    fn selector_reducer_bounds_navigation_and_returns_typed_intents() {
        let mut selector = AnchoredSelector {
            pane_id: "pane".to_string(),
            pane_index: 0,
            field: (),
            items: vec!["one".to_string(), "two".to_string(), "three".to_string()],
            active_index: 0,
            scroll_offset: 0,
            anchor_column: 0,
            anchor_row: 0,
            anchor_width: 1,
        };

        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Previous, 2),
            SelectorInputOutcome::Unchanged
        );
        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Last, 2),
            SelectorInputOutcome::Updated
        );
        assert_eq!(selector.active_index, 2);
        assert_eq!(selector.scroll_offset, 1);
        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Select, 2),
            SelectorInputOutcome::Select { index: 2 }
        );
    }

    /// Verifies empty and stale selector snapshots never emit an invalid
    /// product selection and that subsequent navigation normalizes stale state
    /// against the current item collection.
    #[test]
    fn selector_reducer_rejects_empty_and_stale_selection_intents() {
        let mut selector = AnchoredSelector {
            pane_id: "pane".to_string(),
            pane_index: 0,
            field: (),
            items: Vec::new(),
            active_index: 7,
            scroll_offset: 4,
            anchor_column: 0,
            anchor_row: 0,
            anchor_width: 1,
        };

        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Select, 2),
            SelectorInputOutcome::Unchanged
        );
        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Next, 2),
            SelectorInputOutcome::Updated
        );
        assert_eq!((selector.active_index, selector.scroll_offset), (0, 0));

        selector.items = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        selector.active_index = 9;
        assert_eq!(
            apply_selector_input(&mut selector, SelectorInputAction::Previous, 2),
            SelectorInputOutcome::Updated
        );
        assert_eq!(selector.active_index, 1);
    }

    /// Verifies viewport reduction adapts when available rows shrink or grow,
    /// preserving the active item while clamping stale scroll offsets after a
    /// terminal resize.
    #[test]
    fn selector_viewport_reducer_clamps_state_across_resizes() {
        let mut selector = AnchoredSelector {
            pane_id: "pane".to_string(),
            pane_index: 0,
            field: (),
            items: (0..5).map(|index| index.to_string()).collect(),
            active_index: 4,
            scroll_offset: 0,
            anchor_column: 0,
            anchor_row: 0,
            anchor_width: 1,
        };

        keep_selector_active_visible(&mut selector, 2);
        assert_eq!(selector.scroll_offset, 3);
        keep_selector_active_visible(&mut selector, 5);
        assert_eq!(selector.scroll_offset, 0);
    }

    /// Verifies overlay transitions own search, selection, scrolling, and
    /// close intent while leaving opaque command execution to the caller.
    #[test]
    fn overlay_reducer_mutates_state_and_returns_command_intent() {
        let size = Size::new(20, 3).unwrap();
        let mut overlay = overlay(&["alpha", "beta", "gamma"]);
        overlay.selections.push(OverlaySelection {
            logical_id: 0,
            line_index: 1,
            start_column: 0,
            width: 4,
            command: "open-beta".to_string(),
            kind: OverlaySelectionKind::Primary,
        });
        overlay.active_selection_index = Some(0);

        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectActive,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Unchanged
        );
        overlay.scroll_offset = 1;
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectActive,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Invoke {
                command: "open-beta".to_string()
            }
        );
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::StartSearch,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(overlay.search_input.as_deref(), Some(""));
    }

    /// Verifies keyboard navigation treats wrapped physical fragments as one
    /// logical choice while preserving a physical index for command execution.
    #[test]
    fn overlay_reducer_skips_fragments_of_one_logical_selection() {
        let size = Size::new(20, 8).unwrap();
        let mut overlay = overlay(&["first", "continued", "second"]);
        overlay.selections = vec![
            OverlaySelection {
                logical_id: 10,
                line_index: 0,
                start_column: 0,
                width: 5,
                command: "open-first".to_string(),
                kind: OverlaySelectionKind::Primary,
            },
            OverlaySelection {
                logical_id: 10,
                line_index: 1,
                start_column: 0,
                width: 9,
                command: "open-first".to_string(),
                kind: OverlaySelectionKind::Primary,
            },
            OverlaySelection {
                logical_id: 11,
                line_index: 2,
                start_column: 0,
                width: 6,
                command: "open-second".to_string(),
                kind: OverlaySelectionKind::Primary,
            },
        ];
        overlay.active_selection_index = Some(0);

        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectNext,
                None,
                true,
                size
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(overlay.active_selection_index, Some(2));
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectPrevious,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(overlay.active_selection_index, Some(0));
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectFirst,
                None,
                true,
                size
            ),
            OverlayInputOutcome::Unchanged
        );
        assert_eq!(overlay.active_selection_index, Some(0));
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectLast,
                None,
                true,
                size
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(overlay.active_selection_index, Some(2));
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectActive,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Invoke {
                command: "open-second".to_string()
            }
        );
    }

    /// Verifies pager filtering is reduced entirely inside mux state and an
    /// unknown event leaves every overlay field unchanged.
    #[test]
    fn overlay_reducer_applies_search_and_ignores_unknown_events() {
        let size = Size::new(20, 4).unwrap();
        let mut overlay = overlay(&["alpha", "beta", "gamma"]);

        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::StartSearch,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::EditSearchText,
                Some("beta"),
                true,
                size,
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectActive,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Updated
        );
        assert_eq!(overlay.search_query.as_deref(), Some("beta"));
        assert_eq!(
            overlay.search_match.map(|matched| matched.line_index),
            Some(1)
        );

        let unchanged = overlay.clone();
        assert_eq!(
            apply_overlay_input(&mut overlay, OverlayInputAction::Ignore, None, true, size,),
            OverlayInputOutcome::Ignored
        );
        assert_eq!(overlay, unchanged);
    }

    /// Verifies cancellation and stale command selections return typed caller
    /// intents without executing or fabricating a product command.
    #[test]
    fn overlay_reducer_closes_and_rejects_stale_selection() {
        let size = Size::new(20, 4).unwrap();
        let mut overlay = overlay(&["alpha"]);
        overlay.active_selection_index = Some(4);

        assert_eq!(
            apply_overlay_input(
                &mut overlay,
                OverlayInputAction::SelectActive,
                None,
                true,
                size,
            ),
            OverlayInputOutcome::Unchanged
        );
        assert_eq!(
            apply_overlay_input(&mut overlay, OverlayInputAction::Exit, None, true, size,),
            OverlayInputOutcome::Close
        );
    }
}
