//! Regression tests for synchronized terminal output.

use crate::{TerminalScreen, TerminalSize as Size};

/// Verifies DECSET freezes the published projection while the authoritative
/// terminal model continues accepting repaint bytes until DEC reset.
#[test]
fn terminal_screen_synchronized_output_freezes_presentation_until_decreset() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before");
    let begin = screen.feed(b"\x1b[?2026h");
    screen.feed(b"\r\x1b[2Jafter");

    assert!(screen.synchronized_output_active());
    assert_eq!(begin.begin_epoch, Some(1));
    assert!(begin.rearm_timeout);
    assert_eq!(screen.visible_lines()[0], "after");
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "before");

    let end = screen.feed(b"\x1b[?2026l");
    assert!(!end.rearm_timeout);
    assert!(end.released);
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "after");
}

/// Verifies DEC synchronization markers remain correct when delivered across
/// arbitrary PTY feed boundaries.
#[test]
fn terminal_screen_synchronized_output_recognizes_every_fragmented_dec_boundary() {
    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";

    for sequence in [BEGIN, END] {
        for split in 1..sequence.len() {
            let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
            if sequence == END {
                screen.feed(BEGIN);
            }
            screen.feed(&sequence[..split]);
            screen.feed(&sequence[split..]);
            if sequence == BEGIN {
                assert!(screen.synchronized_output_active(), "split {split}");
            } else {
                assert!(!screen.synchronized_output_active(), "split {split}");
            }
        }
    }
}

/// Verifies legacy DCS markers are recognized across a fragmented ST terminator
/// and an open transaction can be idempotently force-released.
#[test]
fn terminal_screen_synchronized_output_recognizes_legacy_dcs_and_force_releases() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1bP=1s");
    screen.feed(b"\x1b\\");
    assert!(screen.synchronized_output_active());

    screen.feed(b"live");
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "");
    assert!(screen.force_release_synchronized_output());
    assert!(!screen.force_release_synchronized_output());
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "live");
}

/// Verifies repeated begin markers preserve the original published projection,
/// advance the recovery epoch, and allow one matching end marker to release it.
#[test]
fn terminal_screen_synchronized_output_rearms_without_nesting() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before");
    let first = screen.feed(b"\x1b[?2026h");
    screen.feed(b"\r\x1b[2Kafter");
    let repeated = screen.feed(b"\x1b[?2026h");

    assert_eq!(first.begin_epoch, Some(1));
    assert_eq!(repeated.begin_epoch, Some(2));
    assert!(repeated.rearm_timeout);
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "before");

    let release = screen.feed(b"\x1b[?2026l");
    assert!(release.released);
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "after");
}

/// Verifies a complete transaction in one feed releases the frozen projection.
#[test]
fn terminal_screen_synchronized_output_releases_when_markers_share_one_feed() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before");
    let outcome = screen.feed(b"\x1b[?2026h\r\x1b[2Jafter\x1b[?2026l");

    assert_eq!(outcome.begin_epoch, Some(1));
    assert!(outcome.rearm_timeout);
    assert!(outcome.released);
    assert!(!screen.synchronized_output_active());
    assert_eq!(screen.presentation_visible_styled_lines()[0].text, "after");
}

/// Verifies hidden protocol feeds retain the prior published viewport after a
/// synchronization begin marker and hidden content.
#[test]
fn terminal_screen_synchronized_output_preserves_protocol_feed_presentation() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"visible");
    screen.feed_protocol_preserving_content(b"\x1b[?2026hhidden");

    assert!(screen.synchronized_output_active());
    assert_eq!(screen.visible_lines()[0], "visible");
    assert_eq!(
        screen.presentation_visible_styled_lines()[0].text,
        "visible"
    );
}

/// Verifies an oversized legacy DCS payload is discarded without activating
/// synchronization when its string terminator eventually arrives.
#[test]
fn terminal_screen_synchronized_output_rejects_oversized_legacy_dcs() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    let mut payload = b"\x1bP=1s".to_vec();
    payload.extend(std::iter::repeat_n(b'x', 1025));
    payload.extend_from_slice(b"\x1b\\");

    screen.feed(&payload);

    assert!(!screen.synchronized_output_active());
}
