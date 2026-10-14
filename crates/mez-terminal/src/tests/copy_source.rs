//! Immutable source ownership retained by terminal rows, not a separate store.

use crate::*;
use std::sync::Arc;

/// Split UTF-8 and width-expanding clusters retain one proven source while
/// physical wraps prune the original boundary. Alternate/frozen projections
/// share row Arcs; arbitrary overwrite/erase never keeps entry authority.
#[test]
fn copy_source_scoped_feed_unicode_pruning_and_frozen_projection() {
    let entry = PresentationEntry {
        id: "unicode-owned".into(),
        occurrence: Default::default(),
        first_row: true,
    };
    let source = TerminalCopySource::new(
        TerminalCopySourceId::new(&entry, TerminalCopySourceGroup::WholeEntry).unwrap(),
        "雪✈️1️⃣👩‍💻",
    );
    let mut screen =
        TerminalScreen::new_with_history_config(TerminalSize::new(24, 3).unwrap(), 2, 1).unwrap();
    screen.set_log_frame_settings(
        mez_core::presentation::LogFramePolicy {
            timestamps: true,
            wrap_cap: 24,
        },
        GraphicRendition::default(),
    );
    let bytes = source.text.as_bytes();
    for byte in bytes {
        assert!(screen.feed_presentation_with_copy_source(
            &[*byte],
            entry.clone(),
            Some(source.clone())
        ));
    }
    assert!(
        screen
            .normal_content_lines()
            .join("")
            .contains(source.text.as_ref())
    );
    let mut continuation = entry.clone();
    continuation.first_row = false;
    assert!(screen.feed_presentation_with_copy_source(
        "long wrapped owner ".repeat(40).as_bytes(),
        continuation,
        Some(source.clone())
    ));
    let retained = screen.normal_styled_content_lines();
    assert!(!retained.iter().any(|row| {
        row.presentation
            .as_ref()
            .is_some_and(|entry| entry.first_row)
    }));
    assert!(
        retained
            .iter()
            .filter_map(|row| row.copy_source.as_ref())
            .all(|owner| Arc::ptr_eq(owner, &source))
    );
    screen.feed(b"\x1b[?2026h");
    let frozen = screen.presentation_visible_styled_lines();
    screen.feed(b"\x1b[1;1Hraw\x1b[K");
    assert_eq!(screen.presentation_visible_styled_lines(), frozen);
    screen.feed(b"\x1b[?2026l");
    assert!(screen.visible_styled_lines()[0].copy_source.is_none());
    assert!(
        frozen
            .iter()
            .filter_map(|row| row.copy_source.as_ref())
            .all(|owner| Arc::ptr_eq(owner, &source))
    );
}

/// Associations are captured OFF, survive ON/OFF reflow, shallow clones,
/// alternate-screen save/restore and head pruning, and share one exact payload.
/// Original first-row time is not promoted when only continuations remain.
#[test]
fn copy_source_row_ownership_survives_clone_reflow_pruning_and_alternate() {
    let entry = PresentationEntry {
        id: "source-owner".into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let source = TerminalCopySource::new(
        TerminalCopySourceId::new(&entry, TerminalCopySourceGroup::WholeEntry).unwrap(),
        Arc::<str>::from("exact\r\nraw source\n"),
    );
    let mut screen = TerminalScreen::new(TerminalSize::new(24, 4).unwrap(), 8).unwrap();
    assert!(screen.feed_presentation_with_copy_source(
        format!("{}\r\n", "x".repeat(200)).as_bytes(),
        entry.clone(),
        Some(source.clone())
    ));
    let cloned = screen.clone();
    screen.set_log_frame_settings(
        mez_core::presentation::LogFramePolicy {
            timestamps: true,
            wrap_cap: 24,
        },
        GraphicRendition::default(),
    );
    screen.resize(TerminalSize::new(25, 4).unwrap()).unwrap();
    screen.set_log_frame_settings(
        mez_core::presentation::LogFramePolicy {
            timestamps: false,
            wrap_cap: 24,
        },
        GraphicRendition::default(),
    );
    screen.feed(b"\x1b[?1049hALT\x1b[?1049l");
    let rows = screen.normal_styled_content_lines();
    let retained = rows
        .iter()
        .filter_map(|row| row.copy_source.as_ref())
        .collect::<Vec<_>>();
    assert!(!retained.is_empty());
    assert!(
        retained
            .iter()
            .all(|row| row.id == source.id && Arc::ptr_eq(&row.text, &source.text))
    );
    assert!(
        cloned
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.copy_source.as_ref())
            .all(|row| Arc::ptr_eq(&row.text, &source.text))
    );
    screen.feed(b"\x1b[1;1Hraw");
    assert!(screen.visible_styled_lines()[0].copy_source.is_none());
}

/// Wrong entry/time targets reject the entire bounded plan before any partial
/// mutation. Untagged process/legacy rows never acquire a source by resemblance.
#[test]
fn copy_source_assignment_rejects_wrong_owner_atomically() {
    let entry = PresentationEntry {
        id: "original".into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let mut screen = TerminalScreen::new(TerminalSize::new(24, 4).unwrap(), 16).unwrap();
    screen.feed_presentation(b"entry\r\nsecond\r\n", entry.clone());
    let before = screen.clone();
    let wrong = PresentationEntry {
        id: "foreign".into(),
        ..entry
    };
    let source = TerminalCopySource::new(
        TerminalCopySourceId::new(&wrong, TerminalCopySourceGroup::WholeEntry).unwrap(),
        "unrelated",
    );
    assert!(!screen.feed_presentation_with_copy_source(
        b"unrelated",
        PresentationEntry {
            id: "original".into(),
            ..wrong
        },
        Some(source)
    ));
    assert_eq!(screen, before);
}
