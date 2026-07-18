//! Regression tests for terminal screen protocol strings behavior.

use crate::{TerminalOscEvent, TerminalScreen, TerminalSize as Size};

/// Verifies terminal screen tracks osc title with bel and st terminators.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_osc_title_with_bel_and_st_terminators() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1b]0;window title\x07after");

    assert_eq!(screen.title(), Some("window title"));
    assert_eq!(screen.visible_lines()[0], "beforeafter");

    screen.feed(b"\x1b]2;renamed\x1b\\");

    assert_eq!(screen.title(), Some("renamed"));
    assert_eq!(screen.visible_lines()[0], "beforeafter");
}

/// Verifies terminal screen tracks mezzanine shell transaction osc events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_mezzanine_shell_transaction_osc_events() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]133;A\x1b\\");
    screen.feed(b"\x1b]133;B\x1b\\");
    screen.feed(b"\x1b]133;C\x1b\\");
    screen
        .feed(b"\x1b]133;C;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1\x1b\\");
    screen
        .feed(b"\x1b]133;D;7;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1\x07");

    assert_eq!(
        screen.drain_osc_events(),
        vec![
            TerminalOscEvent::ShellIntegration {
                payload: "A".to_string(),
            },
            TerminalOscEvent::ShellIntegration {
                payload: "B".to_string(),
            },
            TerminalOscEvent::ShellIntegration {
                payload: "C".to_string(),
            },
            TerminalOscEvent::ShellIntegration {
                payload: "C;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1"
                    .to_string(),
            },
            TerminalOscEvent::ShellIntegration {
                payload: "D;7;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1"
                    .to_string(),
            },
        ]
    );
    assert_eq!(screen.visible_lines()[0], "");
}

/// Verifies terminal screen handles fragmented and ignored osc strings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_fragmented_and_ignored_osc_strings() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;c;ignored");
    screen.feed(b"\x07text");

    assert_eq!(screen.title(), None);
    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
    assert_eq!(screen.visible_lines()[0], "text");

    screen.feed(b"\x1b]2;split");
    screen.feed(b" title\x1b\\tail");

    assert_eq!(screen.title(), Some("split title"));
    assert_eq!(screen.visible_lines()[0], "texttail");
}

/// Verifies terminal screen parses osc52 clipboard payloads.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_parses_osc52_clipboard_payloads() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;c;aGVsbG8=\x07after");

    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::Clipboard(
            crate::protocol::TerminalClipboardRequest::Write {
                selection: crate::protocol::TerminalClipboardSelection::new("c"),
                content: crate::protocol::TerminalClipboardContent::new("hello"),
            }
        )]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies an empty OSC 52 write remains a valid typed write with the default
/// empty selection parameter.
///
/// Empty content is distinct from a malformed protocol payload: downstream
/// policy may intentionally clear an internal or external clipboard and must
/// receive the operation rather than having the parser silently discard it.
#[test]
fn terminal_screen_preserves_empty_osc52_clipboard_writes() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;;\x07after");

    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::Clipboard(
            crate::protocol::TerminalClipboardRequest::Write {
                selection: crate::protocol::TerminalClipboardSelection::new(""),
                content: crate::protocol::TerminalClipboardContent::new(""),
            }
        )]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies terminal screen distinguishes OSC 52 clipboard queries from
/// writes without reading or returning clipboard data inside the parser.
///
/// Query authorization and response effects belong outside `mez-terminal`, so
/// the protocol surface must retain only the selection requested by the pane.
#[test]
fn terminal_screen_parses_osc52_clipboard_queries_as_typed_requests() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;p;?\x07after");

    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::Clipboard(
            crate::protocol::TerminalClipboardRequest::Query {
                selection: crate::protocol::TerminalClipboardSelection::new("p"),
            }
        )]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies malformed base64 and decoded binary OSC 52 writes are discarded.
///
/// The clipboard event contract carries UTF-8 text. Silently replacing binary
/// bytes or dispatching malformed content would corrupt the clipboard and blur
/// the terminal/product boundary, so both inputs must produce no event.
#[test]
fn terminal_screen_drops_malformed_and_binary_osc52_clipboard_writes() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;c;not-base64!\x07");
    screen.feed(b"\x1b]52;c;/w==\x07after");

    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies debug output reports clipboard payload size without revealing the
/// clipboard text itself.
///
/// Terminal protocol events routinely appear in assertion and diagnostic
/// output, so a sensitive OSC 52 payload must not leak through derived debug
/// formatting even though authorized adapters can still read it explicitly.
#[test]
fn terminal_clipboard_content_debug_output_is_redacted() {
    let event = TerminalOscEvent::Clipboard(crate::protocol::TerminalClipboardRequest::Write {
        selection: crate::protocol::TerminalClipboardSelection::new("c"),
        content: crate::protocol::TerminalClipboardContent::new("secret-token"),
    });

    let diagnostic = format!("{event:?}");
    assert!(diagnostic.contains("bytes: 12"), "{diagnostic}");
    assert!(!diagnostic.contains("secret-token"), "{diagnostic}");
}

/// Verifies oversized OSC payloads are dropped instead of dispatched in
/// truncated form.
///
/// OSC 52 clipboard content is base64 encoded, so silently dispatching the
/// bounded prefix can produce a valid but corrupted clipboard event. The parser
/// must consume through the terminator, skip dispatch for that payload, and
/// resume ordinary text parsing afterward.
#[test]
fn terminal_screen_drops_truncated_osc52_clipboard_payloads() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    let oversized_encoded = "A".repeat(4096);
    let sequence = format!("\x1b]52;;{oversized_encoded}\x07after");

    screen.feed(sequence.as_bytes());

    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies an OSC payload exactly at the parser byte limit still dispatches.
///
/// The truncation guard must reject only payloads that exceed the bounded OSC
/// buffer. This protects title and clipboard sequences that fit exactly within
/// the parser limit from being treated as overflow cases.
#[test]
fn terminal_screen_dispatches_osc_payload_at_exact_limit() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    let title = "t".repeat(4094);
    let sequence = format!("\x1b]2;{title}\x07after");

    screen.feed(sequence.as_bytes());

    assert_eq!(screen.title(), Some(title.as_str()));
    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::TitleChanged { title }]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies terminal screen nested multiplexer passthrough payload is bounded and ignored.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_nested_multiplexer_passthrough_payload_is_bounded_and_ignored() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1bPtmux;\x1b\x1b[31mnested\x1b\\after");

    assert_eq!(screen.visible_lines()[0], "beforeafter");
    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
}

/// Verifies terminal screen ignores dcs string controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_ignores_dcs_string_controls() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1bP1$r q\x1b\\after");

    assert_eq!(screen.visible_lines()[0], "beforeafter");

    screen.feed(b"\x1bPignored");
    screen.feed(b" payload\x1b\\tail");

    assert_eq!(screen.visible_lines()[0], "beforeaftertail");

    screen.feed(b"\x1bPbell\x07still ignored\x1b\\ok");

    assert_eq!(screen.visible_lines()[0], "beforeaftertailok");
    assert_eq!(screen.bell_events(), 0);
}

/// Verifies terminal screen ignores unsupported string controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_ignores_unsupported_string_controls() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"a\x1bXignored\x1b\\b\x1b^private\x1b\\c\x1b_apc\x1b\\d");

    assert_eq!(screen.visible_lines()[0], "abcd");
}

/// Verifies that OSC 0 and OSC 2 title-setting sequences update the terminal
/// title to the specified value and that empty titles fall back to the default.
#[test]
fn terminal_screen_osc_title_setting() {
    let size = Size::new(10, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b]0;project\x07");
    assert_eq!(screen.title(), Some("project"));

    screen.feed(b"\x1b]2;build\x1b\\");
    assert_eq!(screen.title(), Some("build"));

    screen.feed(b"\x1b]0;\x07");
    assert_eq!(screen.title(), Some("")); // empty title stored as-is

    screen.feed(b"\x1b]2;project-name\x1b\\");
    assert_eq!(screen.title(), Some("project-name"));
}
