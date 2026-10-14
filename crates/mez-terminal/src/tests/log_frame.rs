//! Scoped framing geometry without visual bytes in canonical terminal state.

use crate::*;
use mez_core::presentation::LogFramePolicy;

/// Format width, not just pane dimensions or enablement, changes the tagged
/// insertion edge. Existing content must reflow before the next scalar, and
/// an equal-width token reorder must dirty visual generation without reflow.
/// A wide suppressed format and the return trip preserve all inserted bytes.
#[test]
fn log_timestamp_format_effective_edge_cursor_survives_width_changes() {
    let mut screen = TerminalScreen::new(TerminalSize::new(24, 8).unwrap(), 100).unwrap();
    let entry = PresentationEntry {
        id: "format-insertion".into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let policy = |format| LogFramePolicy {
        timestamps: true,
        wrap_cap: 24,
        timestamp_format: mez_core::presentation::LogTimestampFormat::compile(format).unwrap(),
    };
    screen.set_log_frame_settings(policy("%H:%M"), GraphicRendition::default());
    screen.feed_presentation(b"abcdefghijklmnopqr", entry.clone());
    screen.set_log_frame_settings(policy("[%H:%M:%S]"), GraphicRendition::default());
    assert!(
        screen
            .visible_styled_lines()
            .iter()
            .filter(|row| row.presentation.is_some())
            .all(|row| terminal_text_width(&row.text, terminal_emoji_width()) <= 13)
    );
    screen.feed_presentation(b"Z", entry.clone());
    assert!(
        screen
            .visible_lines()
            .join("")
            .contains("abcdefghijklmnopqrZ")
    );
    let rows = screen.normal_styled_content_lines();
    let generation = screen.render_generation();
    screen.set_log_frame_settings(policy("[%S:%M:%H]"), GraphicRendition::default());
    assert_eq!(screen.normal_styled_content_lines(), rows);
    assert_ne!(screen.render_generation(), generation);
    for format in ["12345678901234567890123456789012", "%H:%M"] {
        screen.set_log_frame_settings(policy(format), GraphicRendition::default());
    }
    screen.feed_presentation(b"END", entry);
    assert!(
        screen
            .visible_lines()
            .join("")
            .contains("abcdefghijklmnopqrZEND")
    );
    assert_eq!(screen.size(), TerminalSize::new(24, 8).unwrap());
}

/// A normalized tagged cursor can sit immediately after the effective right
/// edge while still inside the physical grid. Width resize with the same cap
/// and a same-size OFF->ON replacement must preserve that insertion point;
/// the next tagged scalar may wrap but must not overwrite the preceding cell.
#[test]
fn log_frame_effective_edge_cursor_survives_resize_and_toggle() {
    for toggle in [false, true] {
        let mut screen = TerminalScreen::new(TerminalSize::new(24, 8).unwrap(), 100).unwrap();
        screen.set_log_frame_settings(
            LogFramePolicy {
                timestamps: !toggle,
                wrap_cap: 24,
                ..LogFramePolicy::default()
            },
            GraphicRendition::default(),
        );
        let entry = PresentationEntry {
            id: "insertion".into(),
            occurrence: PresentationOccurrence::default(),
            first_row: true,
        };
        screen.feed_presentation(b"abcdefghijklmno", entry.clone());
        if toggle {
            screen.set_log_frame_settings(
                LogFramePolicy {
                    timestamps: true,
                    wrap_cap: 24,
                    ..LogFramePolicy::default()
                },
                GraphicRendition::default(),
            );
        } else {
            screen.resize(TerminalSize::new(25, 8).unwrap()).unwrap();
        }
        screen.feed_presentation(b"Z", entry);
        assert!(
            screen.visible_lines().join("").contains("abcdefghijklmnoZ"),
            "toggle={toggle} rows={:?}",
            screen.visible_lines()
        );
    }
}

/// Width-expanding selectors relocate the complete grapheme at the tagged
/// right edge. Both one feed and split feeds retain the original semantic
/// boundary and stay inside the canonical capacity; a scalar-only budget check
/// would incorrectly produce a sixteenth canonical cell in this fixture.
#[test]
fn log_frame_expanding_grapheme_at_edge_stays_inside_budget() {
    for split in [false, true] {
        for (base, suffix) in [("❤", "\u{fe0f}"), ("1", "\u{fe0f}\u{20e3}")] {
            let mut screen = TerminalScreen::new(TerminalSize::new(24, 8).unwrap(), 100).unwrap();
            screen.set_log_frame_settings(
                LogFramePolicy {
                    timestamps: true,
                    wrap_cap: 24,
                    ..LogFramePolicy::default()
                },
                GraphicRendition::default(),
            );
            let entry = PresentationEntry {
                id: "expanded".into(),
                occurrence: PresentationOccurrence::default(),
                first_row: true,
            };
            let first = format!("abcdefghijklmn{base}");
            if split {
                screen.feed_presentation(first.as_bytes(), entry.clone());
                screen.feed_presentation(suffix.as_bytes(), entry);
            } else {
                screen.feed_presentation(format!("{first}{suffix}").as_bytes(), entry);
            }
            let rows = screen.visible_styled_lines();
            assert!(
                rows.iter()
                    .any(|row| row.text.contains(&format!("{base}{suffix}")))
            );
            assert!(
                rows.iter()
                    .filter(|row| row.presentation.is_some())
                    .all(|row| terminal_text_width(&row.text, terminal_emoji_width()) <= 15)
            );
            assert_eq!(
                rows.iter()
                    .filter_map(|row| row.presentation.as_ref())
                    .filter(|entry| entry.first_row)
                    .count(),
                1
            );
        }
    }
}

/// Default-off keeps the original pane-width soft wrapping even above the cap.
/// Enabling reserves only tagged rows; raw output still uses the full physical
/// grid. A captured continuation can never become a new entry on resize.
#[test]
fn log_frame_disabled_parity_and_tagged_raw_widths() {
    let size = TerminalSize::new(40, 8).unwrap();
    let entry = PresentationEntry {
        id: "entry".into(),
        occurrence: PresentationOccurrence::default(),
        first_row: true,
    };
    let bytes = format!("\x1b[2m│ {}", "x".repeat(80));
    let mut original = TerminalScreen::new(size, 100).unwrap();
    original.set_wrap_continuation_prefix("│ ");
    original.feed_presentation(bytes.as_bytes(), entry.clone());
    let mut off = TerminalScreen::new(size, 100).unwrap();
    off.set_wrap_continuation_prefix("│ ");
    off.set_log_frame_settings(
        LogFramePolicy {
            timestamps: false,
            wrap_cap: 20,
            ..LogFramePolicy::default()
        },
        GraphicRendition::default(),
    );
    off.feed_presentation(bytes.as_bytes(), entry.clone());
    assert_eq!(
        off.normal_styled_content_lines(),
        original.normal_styled_content_lines()
    );
    let mut on = TerminalScreen::new(size, 100).unwrap();
    on.set_wrap_continuation_prefix("│ ");
    on.set_log_frame_settings(
        LogFramePolicy {
            timestamps: true,
            wrap_cap: 24,
            ..LogFramePolicy::default()
        },
        GraphicRendition::default(),
    );
    on.feed_presentation(bytes.as_bytes(), entry);
    assert_eq!(on.size(), size);
    assert!(
        on.normal_styled_content_lines()
            .iter()
            .filter(|row| row.presentation.is_some())
            .all(|row| terminal_text_width(&row.text, terminal_emoji_width()) <= 15)
    );
    on.feed(b"\r\nraw-012345678901234567890123456789\r\n");
    assert!(
        on.visible_lines()
            .iter()
            .any(|row| row == "raw-012345678901234567890123456789")
    );
    on.resize(TerminalSize::new(30, 8).unwrap()).unwrap();
    assert_eq!(
        on.normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .count(),
        1
    );
}

/// At an effective right edge, a following tagged feed wraps but raw output
/// continues at the physical cursor. Combining marks extend the last grapheme
/// rather than introducing another occurrence or losing its final scalar.
#[test]
fn log_frame_tagged_pending_edge_and_combining_are_lossless() {
    for raw in [false, true] {
        let mut screen = TerminalScreen::new(TerminalSize::new(24, 8).unwrap(), 100).unwrap();
        screen.set_log_frame_settings(
            LogFramePolicy {
                timestamps: true,
                wrap_cap: 24,
                ..LogFramePolicy::default()
            },
            GraphicRendition::default(),
        );
        let entry = PresentationEntry {
            id: "edge".into(),
            occurrence: PresentationOccurrence::default(),
            first_row: true,
        };
        screen.feed_presentation(b"abcdefghijklmno", entry.clone());
        if raw {
            screen.feed("\u{301}RAW".as_bytes());
        } else {
            screen.feed_presentation("\u{301}NEXT".as_bytes(), entry);
        }
        let text = screen.visible_lines().join("");
        assert!(text.contains("abcdefghijklmno\u{301}"));
        assert!(text.contains(if raw { "RAW" } else { "NEXT" }));
        if !raw {
            assert_eq!(
                screen
                    .visible_styled_lines()
                    .iter()
                    .filter_map(|row| row.presentation.as_ref())
                    .filter(|entry| entry.first_row)
                    .count(),
                1
            );
        }
    }
}
