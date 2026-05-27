//! Runtime render selector input decoding.
//!
//! This module owns keyboard decoding for display overlays and pane status
//! selectors. It intentionally has no render-state dependencies, keeping
//! navigation semantics reusable across overlay surfaces.

/// Display-overlay navigation action decoded from terminal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeDisplayOverlayInputAction {
    /// Exit the overlay.
    Exit,
    /// Enter command-output pager search editing.
    StartSearch,
    /// Append printable text to the active pager search query.
    EditSearchText,
    /// Delete the previous character from the active pager search query.
    EditSearchBackspace,
    /// Select the currently active row.
    SelectActive,
    /// Move selection to the previous selectable row.
    SelectPrevious,
    /// Move selection to the next selectable row.
    SelectNext,
    /// Move to the first selectable row when a selector is active.
    SelectFirst,
    /// Move to the last selectable row when a selector is active.
    SelectLast,
    /// Scroll the overlay by the signed row delta.
    ScrollBy(isize),
    /// Ignore this input for overlay purposes.
    Ignore,
}

/// Converts raw terminal input into a display-overlay action.
pub(super) fn runtime_display_overlay_input_action(
    input: &[u8],
) -> RuntimeDisplayOverlayInputAction {
    if input == b"q" {
        return RuntimeDisplayOverlayInputAction::Exit;
    }
    if input == b"/" {
        return RuntimeDisplayOverlayInputAction::StartSearch;
    }
    if input == b"\x7f" || input == b"\x08" {
        return RuntimeDisplayOverlayInputAction::EditSearchBackspace;
    }
    if std::str::from_utf8(input)
        .is_ok_and(|text| !text.is_empty() && text.chars().all(|ch| !ch.is_control()))
    {
        return RuntimeDisplayOverlayInputAction::EditSearchText;
    }
    match runtime_selector_input_action(input) {
        RuntimeSelectorInputAction::Exit => RuntimeDisplayOverlayInputAction::Exit,
        RuntimeSelectorInputAction::Select => RuntimeDisplayOverlayInputAction::SelectActive,
        RuntimeSelectorInputAction::Previous => RuntimeDisplayOverlayInputAction::SelectPrevious,
        RuntimeSelectorInputAction::Next => RuntimeDisplayOverlayInputAction::SelectNext,
        RuntimeSelectorInputAction::First => RuntimeDisplayOverlayInputAction::SelectFirst,
        RuntimeSelectorInputAction::Last => RuntimeDisplayOverlayInputAction::SelectLast,
        RuntimeSelectorInputAction::Ignore => match input {
            b"\x1b[5~" => RuntimeDisplayOverlayInputAction::ScrollBy(-10),
            b"\x1b[6~" => RuntimeDisplayOverlayInputAction::ScrollBy(10),
            _ => RuntimeDisplayOverlayInputAction::Ignore,
        },
    }
}

/// Selector navigation action shared by dropdown and command overlay controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeSelectorInputAction {
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

/// Converts raw terminal input into selector navigation.
pub(super) fn runtime_selector_input_action(input: &[u8]) -> RuntimeSelectorInputAction {
    match input {
        b"\x1b" | b"\x03" => RuntimeSelectorInputAction::Exit,
        b"\r" | b"\n" => RuntimeSelectorInputAction::Select,
        b"\x1b[A" | b"\x1bOA" | b"\x1b[D" | b"\x1bOD" => RuntimeSelectorInputAction::Previous,
        b"\x1b[B" | b"\x1bOB" | b"\x1b[C" | b"\x1bOC" => RuntimeSelectorInputAction::Next,
        b"\x1b[H" | b"\x1b[1~" => RuntimeSelectorInputAction::First,
        b"\x1b[F" | b"\x1b[4~" => RuntimeSelectorInputAction::Last,
        _ => RuntimeSelectorInputAction::Ignore,
    }
}

/// Moves a bounded selector index by one item.
pub(super) fn runtime_selector_step_index(active: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta.is_negative() {
        active.saturating_sub(delta.unsigned_abs())
    } else {
        active
            .saturating_add(usize::try_from(delta).unwrap_or(usize::MAX))
            .min(len.saturating_sub(1))
    }
}
