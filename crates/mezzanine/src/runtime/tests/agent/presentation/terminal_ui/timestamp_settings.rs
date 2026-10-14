//! Real configuration replacement and immutable timestamp projection snapshots.
//!
//! These tests exercise parser/apply/refresh, not only a private boolean. The
//! temporary test-only parser seam keeps production activation inert while the
//! complete source/copy/client contract is being qualified. Synthetic fixtures
//! have no provider, credentials, external commands or user configuration.

use super::*;

/// A rendered authored paragraph separator owns an explicit empty SourceLine,
/// unlike synthetic padding or a timestamp-only selection. Full source copy
/// must emit that selected interior group once so two authored newlines survive;
/// selecting the same row's margin alone remains empty.
#[test]
fn runtime_log_timestamps_owned_empty_source_line_preserves_authored_separator() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    let source = "**first**\n\nsecond";
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            source,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();
    let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
    copy.set_agent_surface(true);
    let blank = (0..copy.line_count())
        .find(|row| {
            copy.copy_source(*row).is_some_and(|source| {
                source.text.is_empty()
                    && source.id.group() == mez_terminal::TerminalCopySourceGroup::SourceLine(1)
            })
        })
        .unwrap();
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
    assert!(first < blank && blank < last);
    let end = mez_mux::render::char_count(&copy.lines()[last]);
    copy.select_range(
        CopyPosition {
            line: first,
            column: 0,
        },
        CopyPosition {
            line: last,
            column: end,
        },
    )
    .unwrap();
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        source
    );
    copy.select_range(
        CopyPosition {
            line: blank,
            column: 0,
        },
        CopyPosition {
            line: blank,
            column: 9,
        },
    )
    .unwrap();
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        ""
    );
}

/// Raw Markdown peer source groups use one owner followed by skipped rows.
/// Narrow live reflow must not duplicate that owner into unrecognized payloads:
/// every visible first/continuation-only selection returns the exact original
/// source once. Equal-text messages remain distinct owned groups through the
/// same-size ON/OFF/ON transition and a history/live boundary.
#[test]
fn runtime_log_timestamps_raw_peer_reflow_preserves_every_source_group_row() {
    for history_split in [false, true] {
        let mut service = test_runtime_service_with_size(Size::new(100, 80).unwrap());
        replace_timestamp_settings(&mut service, true, 80, "");
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(
                Size::new(80, if history_split { 6 } else { 40 }).unwrap(),
                300,
            )
            .unwrap(),
        );
        let payload = format!("{}\nauthored second line", "longtoken".repeat(24));
        for _ in 0..2 {
            service
                .append_agent_received_peer_message_to_terminal_buffer(
                    "%1",
                    "agent-%3",
                    "text/markdown",
                    &payload,
                )
                .unwrap();
        }
        let before = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let owners = before
            .iter()
            .enumerate()
            .filter_map(|(row, line)| {
                (line.copy_text.as_deref() == Some(payload.as_str())).then_some(row)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            owners.len(),
            2,
            "fixture must exercise two raw source owners"
        );
        let ids = owners
            .iter()
            .map(|row| before[*row].presentation.as_ref().unwrap().id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 2, "equal payloads must be distinct occurrences");
        assert!(owners.iter().all(|row| {
            before
                .get(row + 1)
                .and_then(|line| line.copy_text.as_deref())
                == Some(mez_mux::copy::COPY_SKIP_LINE)
        }));
        service
            .agent_pane_screen_mut("%1")
            .unwrap()
            .resize(Size::new(32, if history_split { 6 } else { 40 }).unwrap())
            .unwrap();
        for enabled in [true, false, true] {
            replace_timestamp_settings(&mut service, enabled, 80, "");
            let mut copy =
                CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 40).unwrap();
            copy.set_agent_surface(true);
            let rows = (0..copy.line_count())
                .filter(|row| {
                    copy.copy_source(*row)
                        .is_some_and(|source| ids.contains(source.id.owner()))
                })
                .collect::<Vec<_>>();
            assert!(rows.len() >= 4);
            let first = rows[0];
            let last = *rows.last().unwrap();
            for row in rows.iter().copied() {
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
                    copy.copy_selection_with_format(
                        crate::host::terminal::CopySelectionFormat::Source
                    )
                    .unwrap(),
                    payload,
                    "history_split={history_split} enabled={enabled} row={row}"
                );
            }
            let end = mez_mux::render::char_count(&copy.lines()[last]);
            copy.select_range(
                CopyPosition {
                    line: first,
                    column: 0,
                },
                CopyPosition {
                    line: last,
                    column: end,
                },
            )
            .unwrap();
            assert_eq!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap(),
                format!("{payload}\n{payload}")
            );
            let screen = service.agent_pane_screen("%1").unwrap();
            let canonical = screen.normal_styled_content_lines();
            let mut shared = std::collections::BTreeMap::new();
            for source in canonical.iter().filter_map(|row| row.copy_source.as_ref()) {
                if let Some(previous) = shared.insert(source.id.clone(), source.text.clone()) {
                    assert!(
                        std::sync::Arc::ptr_eq(&previous, &source.text),
                        "allocation sharing is not semantic identity"
                    );
                }
            }
            assert_eq!(shared.len(), 2);
            let full_payload_bytes = canonical
                .iter()
                .filter_map(|row| row.copy_text.as_deref())
                .filter(|text| *text == payload)
                .map(str::len)
                .sum::<usize>();
            assert!(
                full_payload_bytes <= 2 * payload.len(),
                "reflow must not multiply full payload copies"
            );
            if history_split {
                let history_ids = screen
                    .history()
                    .styled_lines()
                    .filter_map(|row| row.copy_source.map(|source| source.id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                let live_ids = screen
                    .visible_styled_lines()
                    .into_iter()
                    .filter_map(|row| row.copy_source.map(|source| source.id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                assert!(
                    history_ids.intersection(&live_ids).next().is_some(),
                    "fixture must actually straddle history/live"
                );
            }
        }
    }
}

/// Replaces synthetic primary configuration using the actual settings path.
pub(super) fn replace_timestamp_settings(
    service: &mut RuntimeSessionService,
    enabled: bool,
    cap: usize,
    extra: &str,
) {
    service.replace_config_layers(vec![ConfigLayer {
        name: "timestamp-fixture".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("version = {}\n[terminal]\nagent_log_timestamps = {enabled}\nagent_wrap_column_cap = {cap}\n{extra}", crate::config::CURRENT_CONFIG_SCHEMA_VERSION),
    }]).unwrap();
}

/// Same-size OFF->ON->OFF->ON uses real settings replacement, invalidates the
/// visual generation/cache and keeps occurrence identity/time and canonical
/// source bytes. Composer bounds and physical screen dimensions do not move.
/// Invalid types fail before replacing any live setting or row ownership.
#[test]
fn runtime_log_timestamps_real_config_toggle_preserves_source_and_composer() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, false, 80, "");
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 authored")
        .unwrap();
    let original = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let entry = original
        .iter()
        .find_map(|row| row.presentation.clone())
        .unwrap();
    let screen_size = service.agent_pane_screen("%1").unwrap().size();
    let composer = service.agent_prompt_editable_body_width("%1").unwrap();
    let mut last_lineage = service
        .agent_pane_screen_lineage("%1", &conversation)
        .unwrap();
    for enabled in [true, false, true] {
        replace_timestamp_settings(&mut service, enabled, 80, "");
        assert_eq!(
            service.presentation.settings.terminal_agent_log_timestamps,
            enabled
        );
        let lineage = service
            .agent_pane_screen_lineage("%1", &conversation)
            .unwrap();
        assert_ne!(lineage, last_lineage);
        last_lineage = lineage;
        let screen = service.agent_pane_screen("%1").unwrap();
        assert_eq!(screen.size(), screen_size);
        assert_eq!(
            service.agent_prompt_editable_body_width("%1").unwrap(),
            composer
        );
        let canonical = screen.normal_styled_content_lines();
        assert_eq!(canonical, original);
        assert!(
            canonical
                .iter()
                .filter_map(|row| row.presentation.as_ref())
                .all(|retained| retained.id == entry.id && retained.occurrence == entry.occurrence)
        );
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
            usize::from(enabled)
        );
        let mut copy = CopyMode::from_screen(screen, 24).unwrap();
        copy.set_agent_surface(true);
        let row = copy
            .lines()
            .iter()
            .position(|row| row.contains("authored"))
            .unwrap();
        let end = mez_mux::render::char_count(&copy.lines()[row]);
        copy.select_range(
            CopyPosition {
                line: row,
                column: 0,
            },
            CopyPosition {
                line: row,
                column: end,
            },
        )
        .unwrap();
        assert_eq!(copy.copy_selection().unwrap(), "01:02:03 authored");
    }
    let before = service.agent_pane_screen("%1").unwrap().clone();
    assert!(
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "invalid-timestamp".into(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[terminal]\nagent_log_timestamps = \"sometimes\"\n".into(),
            }])
            .is_err()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    assert!(service.presentation.settings.terminal_agent_log_timestamps);
}

/// An active copy view is a frozen row/map/selection snapshot. OFF and theme
/// changes retain that coherent snapshot rather than mixing new offsets with
/// old rows or silently canceling selection. A newly opened OFF snapshot has
/// no decoration, while old full/partial/margin copies preserve content bytes.
#[test]
fn runtime_log_timestamps_retained_copy_snapshot_owns_geometry_and_selection() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 authored")
        .unwrap();
    let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
    copy.set_agent_surface(true);
    let row = copy
        .lines()
        .iter()
        .position(|row| row.contains("authored"))
        .unwrap();
    let end = mez_mux::render::char_count(&copy.lines()[row]);
    copy.select_range(
        CopyPosition {
            line: row,
            column: 0,
        },
        CopyPosition {
            line: row,
            column: end,
        },
    )
    .unwrap();
    let frozen = copy.clone();
    service.insert_active_copy_mode_for_presented_surface("%1", copy);
    for extra in [
        "",
        "[theme.colors]\nagent_transcript_status_fg = \"#abcdef\"\n",
    ] {
        replace_timestamp_settings(&mut service, false, 80, extra);
        let retained = service
            .active_copy_mode_for_presented_surface("%1")
            .unwrap();
        assert_eq!(retained, &frozen);
        assert_eq!(retained.copy_selection().unwrap(), "01:02:03 authored");
        assert_eq!(retained.log_frame(row).unwrap().body_origin, 11);
        let new = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
        assert!(new.log_frame(row).is_none());
        assert!(!new.lines()[row].starts_with("01:02:03 │ "));
    }
    let retained = service
        .active_copy_mode_for_presented_surface_mut("%1")
        .unwrap();
    retained
        .select_range(
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
    assert_eq!(retained.copy_selection().unwrap(), "");
}

/// The threshold is evaluated against the total cap even in a wide pane. At
/// 20 cells the complete margin is suppressed; at 21/22 it is exactly nine
/// cells. Unknown/cropped metadata is not replaced with a current clock.
#[test]
fn runtime_log_timestamps_cap_boundary_uses_whole_frame_budget() {
    for cap in [20, 21, 22] {
        let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
        replace_timestamp_settings(&mut service, true, cap, "");
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
        );
        service
            .append_agent_assistant_text_to_terminal_buffer("%1", "雪 wide boundary")
            .unwrap();
        let copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
        for row in copy
            .visible_styled_lines()
            .iter()
            .filter(|row| !row.text.is_empty())
        {
            assert!(
                mez_terminal::terminal_text_width(&row.text, mez_terminal::terminal_emoji_width())
                    <= cap
            );
        }
        assert_eq!(
            copy.lines()
                .iter()
                .filter(|row| row.starts_with("01:02:03 │ "))
                .count(),
            usize::from(cap >= 21)
        );
    }
}
