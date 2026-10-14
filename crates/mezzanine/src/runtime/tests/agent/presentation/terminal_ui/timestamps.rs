//! Fixed occurrence-time decoration at the server's visual boundary.
//!
//! Canonical screen/source rows remain undecorated. These regressions exercise
//! the actual client view, not a timestamp-looking string in authored content.

use super::*;

/// Once a source-backed entry has acquired owned reflow associations, hiding
/// timestamps cannot reinterpret those payloads as string fragments. No-store
/// ON resize -> same-size OFF -> ON preserves original source for both first
/// and continuation-only selections, without encoded reference bytes.
#[test]
fn runtime_log_timestamps_toggle_preserves_no_store_source_associations() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    service.presentation.settings.terminal_agent_log_timestamps = true;
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    let source = "exacttoken".repeat(18);
    service
        .append_agent_assistant_content_to_terminal_buffer("%1", &source, "text/markdown")
        .unwrap();
    service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .resize(Size::new(32, 24).unwrap())
        .unwrap();
    for enabled in [true, false, true] {
        service.presentation.settings.terminal_agent_log_timestamps = enabled;
        service.refresh_agent_log_frame_settings().unwrap();
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
        copy.set_agent_surface(true);
        let rows = copy
            .lines()
            .iter()
            .enumerate()
            .filter_map(|(row, text)| text.contains("token").then_some(row))
            .collect::<Vec<_>>();
        assert!(rows.len() >= 2);
        for row in [rows[0], rows[1]] {
            let end = mez_mux::render::char_count(&copy.lines()[row]);
            copy.select_range(
                CopyPosition {
                    line: row,
                    column: if enabled { 11 } else { 2 },
                },
                CopyPosition {
                    line: row,
                    column: end,
                },
            )
            .unwrap();
            assert_eq!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap(),
                source,
                "enabled={enabled} selected={row}"
            );
        }
    }
}

/// A full multiline content selection preserves interior authored blank rows
/// with the option on exactly as with it off. Margin-only endpoints remain
/// empty, but blank body rows between endpoints must not collapse paragraphs.
#[test]
fn runtime_log_timestamps_preserve_rendered_interior_blank_rows() {
    let mut copies = Vec::new();
    for enabled in [false, true] {
        let mut service = test_runtime_service();
        service.presentation.settings.terminal_agent_log_timestamps = enabled;
        service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 16).unwrap(), 100).unwrap(),
        );
        service
            .append_agent_assistant_text_to_terminal_buffer("%1", "first\n\nsecond")
            .unwrap();
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 16).unwrap();
        copy.set_agent_surface(true);
        let first = copy
            .lines()
            .iter()
            .position(|row| row.contains("first"))
            .unwrap();
        let last = copy
            .lines()
            .iter()
            .position(|row| row.contains("second"))
            .unwrap();
        let last_column = mez_mux::render::char_count(&copy.lines()[last]);
        copy.select_range(
            CopyPosition {
                line: first,
                column: 0,
            },
            CopyPosition {
                line: last,
                column: last_column,
            },
        )
        .unwrap();
        copies.push(copy.copy_selection().unwrap());
    }
    assert_eq!(copies[0], "first\n\nsecond");
    assert_eq!(copies[1], copies[0]);
}

/// A source-backed unbroken line wholly or partly in history retains one raw
/// source group after a narrower copy projection. First and continuation-only
/// source selections recover exact authored bytes, never encoded references.
#[test]
fn runtime_log_timestamps_history_reflow_preserves_source_association() {
    for live_split in [false, true] {
        let mut service = test_runtime_service();
        service.presentation.settings.terminal_agent_log_timestamps = true;
        service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 8).unwrap(), 200).unwrap(),
        );
        let source = "token".repeat(if live_split { 95 } else { 40 });
        service
            .append_agent_assistant_content_to_terminal_buffer("%1", &source, "text/markdown")
            .unwrap();
        if !live_split {
            service
                .agent_pane_screen_mut("%1")
                .unwrap()
                .clear_visible_into_history();
        }
        service
            .agent_pane_screen_mut("%1")
            .unwrap()
            .resize(Size::new(32, 8).unwrap())
            .unwrap();
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 8).unwrap();
        copy.set_agent_surface(true);
        let rows = copy
            .lines()
            .iter()
            .enumerate()
            .filter_map(|(row, text)| text.contains("token").then_some(row))
            .collect::<Vec<_>>();
        assert!(rows.len() >= 2);
        for row in [rows[0], rows[1]] {
            let last_column = mez_mux::render::char_count(&copy.lines()[row]);
            copy.select_range(
                CopyPosition {
                    line: row,
                    column: 11,
                },
                CopyPosition {
                    line: row,
                    column: last_column,
                },
            )
            .unwrap();
            assert_eq!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap(),
                source,
                "live_split={live_split} selected_row={row} canonical={:?}",
                service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines()
            );
        }
    }
}

/// Owned timestamp/gutter cells are absent from full and partial content copy,
/// margin-only source selection is empty, and an identical authored clock text
/// remains searchable at the shifted body column instead of matching framing.
#[test]
fn runtime_log_timestamps_copy_search_and_authored_clock_are_distinct() {
    let mut service = test_runtime_service();
    service.presentation.settings.terminal_agent_log_timestamps = true;
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 authored exact")
        .unwrap();
    let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 12).unwrap();
    copy.set_agent_surface(true);
    let row = copy
        .lines()
        .iter()
        .position(|row| row.contains("authored exact"))
        .unwrap();
    copy.select_range(
        CopyPosition {
            line: row,
            column: 0,
        },
        CopyPosition {
            line: row,
            column: 9,
        },
    )
    .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "");
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        ""
    );
    let position = copy
        .search("01:02:03", mez_mux::copy::SearchDirection::Forward)
        .unwrap()
        .unwrap();
    assert!(position.column >= 11);
    assert_eq!(copy.copy_selection().unwrap(), "01:02:03");
    let last_column = mez_mux::render::char_count(&copy.lines()[row]);
    copy.select_range(
        CopyPosition {
            line: row,
            column: 0,
        },
        CopyPosition {
            line: row,
            column: last_column,
        },
    )
    .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "01:02:03 authored exact");
    copy.select_word_at(CopyPosition {
        line: row,
        column: 3,
    })
    .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "");
    copy.select_word_at(CopyPosition {
        line: row,
        column: 26,
    })
    .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "authored");
}

/// A deterministic first observation decorates one semantic entry exactly once,
/// even when its authored newline and unbroken token create physical rows. The
/// server view must reserve blank continuation margins, while canonical rows
/// retain their original column-zero gutter and never gain timestamp bytes.
#[test]
fn runtime_log_timestamps_first_multiline_and_softwrap_are_visual_only() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    service.presentation.settings.terminal_agent_log_timestamps = true;
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 20).unwrap(), 120).unwrap(),
    );
    let source = format!("first authored row\n{}", "x".repeat(100));
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", &source)
        .unwrap();
    let canonical = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(canonical.iter().all(|row| !row.text.contains("01:02:03")));
    assert!(canonical.iter().any(|row| {
        row.presentation
            .as_ref()
            .is_some_and(|entry| !entry.first_row)
    }));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 32).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        view.lines
            .iter()
            .filter(|row| row.contains("01:02:03 │ "))
            .count(),
        1,
        "one captured stamp must appear on the original first visual row; view={:?}; canonical={canonical:?}",
        view.lines
    );
    assert!(view.lines.iter().any(|row| row.contains("         │ ")));
}

/// The original small authoritative window crops canonical row zero behind
/// the pinned composer/frame allocation. Its retained continuation must show
/// a blank margin, not a freshly repeated timestamp at the viewport boundary.
#[test]
fn runtime_log_timestamps_cropped_first_row_never_repeats() {
    let mut service = test_runtime_service();
    service.presentation.settings.terminal_agent_log_timestamps = true;
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 20).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            &format!("first authored row\n{}", "x".repeat(100)),
        )
        .unwrap();
    let canonical = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(canonical[0].presentation.as_ref().unwrap().first_row);
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 32).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        !view
            .lines
            .iter()
            .any(|row| row.contains("first authored row"))
    );
    assert!(!view.lines.iter().any(|row| row.contains("01:02:03 │ ")));
    assert!(view.lines.iter().any(|row| row.contains("         │ ")));
}
