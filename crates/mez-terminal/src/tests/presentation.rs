//! Presentation metadata retention independent of terminal text and clocks.

use crate::{
    PresentationEntry, PresentationOccurrence, PresentationOccurrenceBasis, TerminalScreen,
    TerminalSize,
};

/// A semantic multiline entry is annotated once, including explicit empty
/// lines and wraps. Shared history chunks and reflow must carry the original
/// stamp, while arbitrary ANSI output must never invent an entry boundary.
#[test]
fn presentation_occurrence_survives_history_clone_and_reflow() {
    let mut screen = TerminalScreen::new(TerminalSize::new(30, 8).unwrap(), 400).unwrap();
    for index in 0..240 {
        screen.feed_presentation(
            format!("entry-{index}\r\nsecond\r\n").as_bytes(),
            PresentationEntry {
                id: format!("entry-{index}"),
                occurrence: PresentationOccurrence::new(
                    Some(1000 - index),
                    PresentationOccurrenceBasis::LocalAcceptance,
                ),
                first_row: true,
            },
        );
    }
    let original = screen.normal_styled_content_lines();
    let mut cloned = screen.clone();
    cloned.resize(TerminalSize::new(12, 8).unwrap()).unwrap();
    let rows = cloned.normal_styled_content_lines();
    for annotation in original.iter().filter_map(|row| row.presentation.as_ref()) {
        assert!(rows.iter().any(|row| {
            row.presentation.as_ref().is_some_and(|retained| {
                retained.id == annotation.id && retained.occurrence == annotation.occurrence
            })
        }));
    }
    assert_eq!(
        screen.normal_styled_content_lines(),
        original,
        "reflow must not mutate a shared history clone"
    );
    cloned.feed(b"\x1b[Hoverwrite");
    assert!(cloned.visible_styled_lines()[0].presentation.is_none());
}

/// Equal timestamps and unknown clocks do not collapse independently accepted
/// semantic entries. Explicit newlines share an identity; arbitrary PTY lines
/// remain untagged, and copy source bytes are unchanged by annotation.
#[test]
fn presentation_occurrence_identity_is_not_a_clock_or_text_key() {
    let mut screen = TerminalScreen::new(TerminalSize::new(30, 8).unwrap(), 100).unwrap();
    for id in ["first", "second"] {
        screen.feed_presentation(
            b"same\r\n",
            PresentationEntry {
                id: id.into(),
                occurrence: PresentationOccurrence::default(),
                first_row: true,
            },
        );
        screen.set_recent_normal_copy_texts(&["exact source".into()], "skip");
    }
    let rows = screen.normal_styled_content_lines();
    let entries = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].id, entries[1].id);
    assert!(
        entries
            .iter()
            .all(|entry| entry.occurrence.unix_millis.is_none())
    );
    assert!(
        rows.iter()
            .filter(|row| row.presentation.is_some())
            .all(|row| row.copy_text.as_deref() == Some("exact source"))
    );
    screen.feed(b"raw PTY\r\n");
    assert!(
        screen
            .normal_styled_content_lines()
            .iter()
            .find(|row| row.text == "raw PTY")
            .unwrap()
            .presentation
            .is_none()
    );
}

/// Trailing authored empty lines cannot relabel an older occurrence. Entries
/// whose original row is pruned retain only continuation metadata, and a raw
/// edit on a wrapped continuation cannot regain provenance through reflow.
#[test]
fn presentation_boundaries_survive_blanks_pruning_and_raw_wrapped_edits() {
    let annotation = |id: &str| PresentationEntry {
        id: id.into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let mut screen = TerminalScreen::new(TerminalSize::new(10, 4).unwrap(), 20).unwrap();
    screen.feed_presentation(b"old\r\n", annotation("A"));
    screen.feed_presentation(b"new\r\n\r\n", annotation("B"));
    let rows = screen.normal_styled_content_lines();
    assert_eq!(
        rows.iter()
            .find(|row| row.text == "old")
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .id,
        "A"
    );
    let b = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .filter(|entry| entry.id == "B")
        .collect::<Vec<_>>();
    assert_eq!(b.len(), 2);
    assert_eq!(b.iter().filter(|entry| entry.first_row).count(), 1);
    let mut clipped = TerminalScreen::new(TerminalSize::new(10, 2).unwrap(), 2).unwrap();
    clipped.feed_presentation("row\r\n".repeat(40).as_bytes(), annotation("clipped"));
    assert!(
        clipped
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .all(|entry| !entry.first_row)
    );
    let mut edited = TerminalScreen::new(TerminalSize::new(10, 4).unwrap(), 20).unwrap();
    edited.feed_presentation(b"abcdefghijklmnop", annotation("wrapped"));
    edited.feed(b"\x1b[2;2HX");
    edited.resize(TerminalSize::new(20, 4).unwrap()).unwrap();
    assert!(
        edited
            .normal_styled_content_lines()
            .iter()
            .filter(|row| row.text.contains('X'))
            .all(|row| row.presentation.is_none())
    );
}

/// Reusing one entry over partial feeds must not manufacture another boundary.
/// Alternate-screen traffic remains untagged even if entered inside a feed,
/// while the saved normal buffer retains its original annotation on return.
#[test]
fn presentation_continuation_and_alternate_feed_keep_original_boundary() {
    let entry = PresentationEntry {
        id: "component".into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let mut screen = TerminalScreen::new(TerminalSize::new(10, 4).unwrap(), 20).unwrap();
    screen.feed_presentation(b"abc", entry.clone());
    screen.feed_presentation(b"def\r\nghi", entry.clone());
    assert_eq!(
        screen
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .count(),
        1
    );
    screen.feed_presentation(b"\x1b[?1049hX", entry.clone());
    assert!(
        screen
            .visible_styled_lines()
            .iter()
            .all(|row| row.presentation.is_none())
    );
    screen.feed(b"\x1b[?1049l");
    assert_eq!(
        screen
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .count(),
        1
    );
    let mut clipped = TerminalScreen::new(TerminalSize::new(4, 2).unwrap(), 2).unwrap();
    clipped.feed_presentation("x".repeat(200).as_bytes(), entry);
    assert!(
        clipped
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .all(|entry| !entry.first_row)
    );
}
