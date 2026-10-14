//! Immutable source ownership retained by terminal rows, not a separate store.

use crate::*;
use std::sync::Arc;

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
    screen.feed_presentation(format!("{}\r\n", "x".repeat(200)).as_bytes(), entry.clone());
    assert!(screen.set_recent_normal_copy_source_associations(&entry, &[Some(source.clone())]));
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
    let before = screen.normal_styled_content_lines();
    let wrong = PresentationEntry {
        id: "foreign".into(),
        ..entry
    };
    let source = TerminalCopySource::new(
        TerminalCopySourceId::new(&wrong, TerminalCopySourceGroup::WholeEntry).unwrap(),
        "unrelated",
    );
    assert!(!screen.set_recent_normal_copy_source_associations(&wrong, &[Some(source)]));
    assert_eq!(screen.normal_styled_content_lines(), before);
}
