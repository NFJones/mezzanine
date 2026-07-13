//! Regression tests for terminal input key bindings behavior.

use crate::terminal::{
    GroupFocusTarget, KeyBindings, KeyChord, KeyCode, MuxAction, PaneFocusDirection,
    PasteBufferTarget, TerminalInputClassification, WindowFocusTarget, classify_terminal_input,
};

/// Verifies parses key binding notation for default surface.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_key_binding_notation_for_default_surface() {
    assert_eq!(
        KeyChord::parse("C-a").unwrap(),
        KeyBindings::default().escape
    );
    assert_eq!(
        KeyChord::parse("Alt+\\").unwrap(),
        KeyChord::alt(KeyCode::Char('\\'))
    );
    assert_eq!(KeyBindings::default().split_vertical, None);
    assert_eq!(KeyBindings::default().new_window, None);
    assert_eq!(
        KeyChord::parse("A--").unwrap(),
        KeyChord::alt(KeyCode::Char('-'))
    );
    assert_eq!(
        KeyChord::parse("C-A-PageDown").unwrap(),
        KeyChord::ctrl_alt(KeyCode::PageDown)
    );
    assert_eq!(KeyBindings::default().new_group, None);
    assert_eq!(KeyBindings::default().agent_shell, None);
    assert_eq!(KeyBindings::default().focus_previous_group, None);
    assert_eq!(KeyBindings::default().focus_next_group, None);
    assert_eq!(
        KeyChord::parse("Ctrl+Alt+Up").unwrap(),
        KeyChord::ctrl_alt(KeyCode::Up)
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("C-a").unwrap()).unwrap(),
        b"\x01"
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("A--").unwrap()).unwrap(),
        b"\x1b-"
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("C-A-PageDown").unwrap()).unwrap(),
        b"\x1b[6;7~"
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("A-S-=").unwrap()).unwrap(),
        b"\x1b+"
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("C-A-S-PageUp").unwrap()).unwrap(),
        b"\x1b[5;8~"
    );
    assert_eq!(
        KeyChord::parse("Home").unwrap(),
        KeyChord::new(KeyCode::Home)
    );
    assert_eq!(KeyChord::parse("End").unwrap(), KeyChord::new(KeyCode::End));
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("Home").unwrap()).unwrap(),
        b"\x1b[H"
    );
    assert_eq!(
        crate::terminal::key_chord_input_bytes(KeyChord::parse("C-End").unwrap()).unwrap(),
        b"\x1b[1;5F"
    );
    assert_eq!(
        KeyChord::parse("C-C-a").unwrap_err().kind(),
        mez_mux::MuxErrorKind::InvalidArgs
    );
    assert_eq!(
        KeyChord::parse("DefinitelyNotAKey").unwrap_err().kind(),
        mez_mux::MuxErrorKind::InvalidArgs
    );
}

/// Verifies empty key-chord input is rejected without indexing past the slice.
///
/// This regression scenario protects the parser entry point against empty input
/// so callers can treat a missing byte stream as an ordinary parse miss rather
/// than a panic hazard.
#[test]
fn rejects_empty_key_chord_input() {
    assert_eq!(crate::terminal::parse_key_chord_bytes(b""), None);
}

/// Verifies classifies default direct mux key bindings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn classifies_default_direct_mux_key_bindings() {
    let bindings = KeyBindings::default();

    assert_eq!(
        classify_terminal_input(b"\x1b\\", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b-", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b=", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b+", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b]", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7A", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7B", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7D", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7C", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[5;7~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[6;7~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[5;8~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[6;8~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"ordinary input", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b]0;title\x07", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
}

/// Verifies classifies established mux-compatible default prefix key bindings.
///
/// The default prefix table is the primary keyboard contract for users who
/// expect default mux navigation and pane/window commands. This test keeps the
/// broad table pinned so direct convenience bindings cannot accidentally
/// replace the prefix surface.
#[test]
fn classifies_default_prefix_key_bindings() {
    let bindings = KeyBindings::default();
    let assert_prefix = |input: &[u8], action: MuxAction| {
        assert_eq!(
            classify_terminal_input(input, &bindings).unwrap(),
            TerminalInputClassification::Mux(action)
        );
    };

    assert_eq!(
        classify_terminal_input(b"\x01", &bindings).unwrap(),
        TerminalInputClassification::PrefixKeyMode
    );
    assert_prefix(b"\x01\x01", MuxAction::SendPrefixToPane);
    assert_prefix(b"\x01:", MuxAction::EnterCommandPrompt);
    assert_prefix(b"\x01?", MuxAction::ListKeyBindings);
    assert_prefix(b"\x01d", MuxAction::DetachPrimaryClient);
    assert_prefix(b"\x01D", MuxAction::ChooseClientOrObserverToDetach);
    assert_prefix(b"\x01c", MuxAction::NewWindow);
    assert_prefix(b"\x01,", MuxAction::RenameWindow);
    assert_prefix(b"\x01&", MuxAction::KillWindowAfterConfirmation);
    assert_prefix(
        b"\x01w",
        MuxAction::FocusWindow(WindowFocusTarget::ChooseInteractively),
    );
    assert_prefix(
        b"\x01G",
        MuxAction::FocusGroup(GroupFocusTarget::ChooseInteractively),
    );
    assert_prefix(b"\x01C", MuxAction::NewGroup);
    assert_prefix(b"\x01(", MuxAction::FocusGroup(GroupFocusTarget::Previous));
    assert_prefix(b"\x01)", MuxAction::FocusGroup(GroupFocusTarget::Next));
    assert_prefix(b"\x01a", MuxAction::ToggleAgentShell);
    assert_prefix(b"\x01n", MuxAction::FocusWindow(WindowFocusTarget::Next));
    assert_prefix(
        b"\x01p",
        MuxAction::FocusWindow(WindowFocusTarget::Previous),
    );
    assert_prefix(
        b"\x01l",
        MuxAction::FocusWindow(WindowFocusTarget::LastActive),
    );
    assert_prefix(
        b"\x014",
        MuxAction::FocusWindow(WindowFocusTarget::Index(4)),
    );
    assert_prefix(
        b"\x01'",
        MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex),
    );
    assert_prefix(
        b"\x01.",
        MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex),
    );
    assert_prefix(b"\x01%", MuxAction::SplitPaneVertical);
    assert_prefix(b"\x01\"", MuxAction::SplitPaneHorizontal);
    assert_prefix(b"\x01\x1bOA", MuxAction::FocusPane(PaneFocusDirection::Up));
    assert_prefix(
        b"\x01\x1bOB",
        MuxAction::FocusPane(PaneFocusDirection::Down),
    );
    assert_prefix(
        b"\x01\x1bOD",
        MuxAction::FocusPane(PaneFocusDirection::Left),
    );
    assert_prefix(
        b"\x01\x1bOC",
        MuxAction::FocusPane(PaneFocusDirection::Right),
    );
    assert_prefix(b"\x01o", MuxAction::CyclePane);
    assert_prefix(b"\x01;", MuxAction::FocusLastPane);
    assert_prefix(b"\x01q", MuxAction::ShowPaneIndexes);
    assert_prefix(b"\x01z", MuxAction::TogglePaneZoom);
    assert_prefix(b"\x01 ", MuxAction::CycleLayouts);
    assert_prefix(b"\x01x", MuxAction::KillPaneAfterConfirmation);
    assert_prefix(b"\x01!", MuxAction::BreakPaneToNewWindow);
    assert_prefix(b"\x01{", MuxAction::SwapPanePrevious);
    assert_prefix(b"\x01}", MuxAction::SwapPaneNext);
    assert_prefix(b"\x01\x1b[5~", MuxAction::EnterCopyModeAndPageUp);
    assert_prefix(b"\x01[", MuxAction::EnterCopyMode);
    assert_prefix(
        b"\x01]",
        MuxAction::PasteBuffer(PasteBufferTarget::MostRecent),
    );
    assert_prefix(b"\x01#", MuxAction::ListPasteBuffers);
    assert_prefix(
        b"\x01=",
        MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively),
    );
    assert_prefix(b"\x01-", MuxAction::DeleteMostRecentPasteBuffer);
    assert_prefix(b"\x01O", MuxAction::ChoosePendingObservers);
    assert_prefix(b"\x01~", MuxAction::ShowMessages);
    assert_eq!(
        classify_terminal_input(b"\x01e", &bindings).unwrap(),
        TerminalInputClassification::UnboundPrefix(KeyChord::new(KeyCode::Char('e')))
    );
}
