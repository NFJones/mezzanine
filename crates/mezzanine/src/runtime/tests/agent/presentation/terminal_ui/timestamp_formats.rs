//! Bounded format replacement across real settings, workers and frozen copies.
//!
//! Synthetic offline fixtures use captured instants and actual config/apply
//! paths. Formats alter only visual projection and its typed geometry: source,
//! occurrence identity, physical PTY dimensions and editable composer stay
//! authoritative. Old selections retain their matching rows and maps.

use super::timestamp_settings::replace_timestamp_settings;
use super::*;
use crate::host::terminal::CopySelectionFormat;

/// Applies the real parser/replacement path, never mutating private settings.
fn replace_format(service: &mut RuntimeSessionService, enabled: bool, cap: usize, format: &str) {
    replace_timestamp_settings(
        service,
        enabled,
        cap,
        &format!(
            "agent_log_timestamp_format = {}\n",
            serde_json::to_string(format).unwrap()
        ),
    );
}

/// Equal-width formatting changes reject an old streaming candidate even when
/// columns and body budget are unchanged. Fresh ON/OFF/custom candidates keep
/// original occurrence, entry boundary and authored directive-looking bytes.
#[test]
fn runtime_log_timestamp_format_same_width_reload_fences_streaming() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_format(&mut service, true, 80, "%H:%M:%S");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: "text/markdown".into(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**first %H observation**".into(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "format-stream", &event)
            .unwrap();
    }
    let captured = service
        .take_agent_streaming_say_projection_work("%1", "format-stream")
        .unwrap()
        .unwrap();
    let stale = RuntimeSessionService::build_agent_streaming_say_projection(captured).unwrap();
    replace_format(&mut service, true, 80, "%S:%M:%H");
    let before = service.agent_pane_screen("%1").unwrap().clone();
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale)
            .unwrap()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    let mut expected_entry = None;
    for (enabled, format) in [(true, "%S:%M:%H"), (false, "%H:%M"), (true, "%H:%M")] {
        replace_format(&mut service, enabled, 80, format);
        service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
        let work = service
            .take_agent_streaming_say_projection_work("%1", "format-stream")
            .unwrap()
            .unwrap();
        let fresh = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(fresh)
                .unwrap()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let entries = rows
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .collect::<Vec<_>>();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|entry| entry.occurrence.unix_millis == Some(3_723_000))
        );
        assert_eq!(entries.iter().filter(|entry| entry.first_row).count(), 1);
        if let Some(expected) = &expected_entry {
            assert_eq!(entries[0], expected);
        } else {
            expected_entry = Some(entries[0].clone());
        }
        assert!(
            rows.iter()
                .any(|row| row.text.contains("first %H observation"))
        );
    }
}

/// Primaries, an observer and captured render workers share server formatting
/// and styles. Production full/delta encoders replay into a modeled terminal
/// exactly like an independent full redraw after format changes. Content APIs
/// retain original bytes; no client clock, format parser or new wire field.
#[test]
fn runtime_log_timestamp_format_client_snapshots_deltas_and_content_capture_agree() {
    use mez_mux::attached_client::{
        AttachedTerminalModeTransitions, AttachedTerminalOutputFrameState,
        encode_attached_terminal_output_frame_with_styles,
        encode_attached_terminal_output_update_frame_with_verified_size,
    };
    use mez_mux::presentation::{
        AttachedTerminalOutputModes, compose_client_presentation_with_styles,
    };
    let size = Size::new(80, 32).unwrap();
    let mut service = test_runtime_service_with_size(size);
    let first = service
        .attach_primary("format-first", true, size, 120)
        .unwrap();
    let second = service
        .attach_primary("format-second", true, size, 121)
        .unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("format-observer", None, 1)
        .unwrap();
    replace_format(&mut service, false, 80, "%H:%M:%S");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 authored %H frame")
        .unwrap();
    let request = r#"{"jsonrpc":"2.0","id":"format-capture","method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#;
    let original: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(request, &first)).unwrap();
    assert!(original.to_string().contains("authored %H frame"));
    for (enabled, format, stamp) in [
        (true, "%H:%M", "01:02"),
        (true, "%M:%H", "02:01"),
        (true, "[%S-%M-%H]", "[03-02-01]"),
        (false, "%%", ""),
    ] {
        replace_format(&mut service, enabled, 80, format);
        let capture: serde_json::Value =
            serde_json::from_str(&service.dispatch_runtime_control_body(request, &first)).unwrap();
        assert_eq!(capture, original);
        let mut expected = None;
        for (client, role, viewport) in [
            (&first, ClientViewRole::Primary, size),
            (&second, ClientViewRole::Primary, size),
            (
                &observer,
                ClientViewRole::Observer,
                Size::new(40, 16).unwrap(),
            ),
        ] {
            service.prepare_client_render(client, role).unwrap();
            let config = service
                .terminal_client_loop_config(TerminalClientLoopConfig::default())
                .unwrap();
            let inline = service
                .render_client_view_for_client_with_resolved_config(client, role, viewport, &config)
                .unwrap()
                .unwrap();
            let snapshot = service
                .capture_client_render_snapshot(client, role, viewport, &config)
                .unwrap();
            let base = crate::runtime::compose_client_render_snapshot(snapshot).unwrap();
            let (worker, _) = service
                .complete_client_render_snapshot(role, &config, base)
                .unwrap();
            let worker = worker.unwrap();
            let row = inline
                .lines
                .iter()
                .position(|row| row.contains("authored %H frame"))
                .unwrap();
            assert_eq!(worker.lines[row], inline.lines[row]);
            assert_eq!(worker.line_style_spans[row], inline.line_style_spans[row]);
            if enabled {
                assert!(inline.lines[row].contains(&format!("{stamp} │ ")));
            }
            let projection = (
                inline.lines[row].clone(),
                inline.line_style_spans[row].clone(),
            );
            if let Some(expected) = &expected {
                assert_eq!(&projection, expected);
            } else {
                expected = Some(projection);
            }
            let modes = AttachedTerminalOutputModes::default();
            let (lines, spans) = compose_client_presentation_with_styles(&inline, None);
            let mut actual = TerminalScreen::new(viewport, 100).unwrap();
            actual.feed(&encode_attached_terminal_output_frame_with_styles(
                &lines, &spans, None, modes,
            ));
            let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &spans, modes);
            replace_timestamp_settings(
                &mut service,
                true,
                80,
                "agent_log_timestamp_format = '%S:%M:%H'\n[theme.colors]\nagent_transcript_status_fg = '#abcdef'\n",
            );
            let next = service
                .render_client_view(role, viewport, &TerminalClientLoopConfig::default())
                .unwrap()
                .unwrap();
            let (next_lines, next_spans) = compose_client_presentation_with_styles(&next, None);
            actual.feed(
                &encode_attached_terminal_output_update_frame_with_verified_size(
                    &next_lines,
                    &next_spans,
                    None,
                    modes,
                    Some(&previous),
                    AttachedTerminalModeTransitions::default(),
                    Some(viewport),
                ),
            );
            let mut oracle = TerminalScreen::new(viewport, 100).unwrap();
            oracle.feed(&encode_attached_terminal_output_frame_with_styles(
                &next_lines,
                &next_spans,
                None,
                modes,
            ));
            assert_eq!(actual.visible_styled_lines(), oracle.visible_styled_lines());
            replace_format(&mut service, enabled, 80, format);
            service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
        }
    }
}

/// Same-width replacements must change visual generations, not canonical rows.
/// Wider/narrower replacements and OFF retain the existing boxed copy snapshot;
/// new copies have the new stamp/map. Authored clocks and percent directives are
/// never interpreted, and margin-only/source/search behavior uses captured maps.
#[test]
fn runtime_log_timestamp_format_reload_preserves_frozen_copy_and_source() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_format(&mut service, true, 80, "%H:%M:%S");
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
    let source = "01:02:03 authored %H:%M:%S";
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", source)
        .unwrap();
    let original = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let size = service.agent_pane_screen("%1").unwrap().size();
    let composer = service.agent_prompt_editable_body_width("%1").unwrap();
    let mut frozen = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
    frozen.set_agent_surface(true);
    let row = frozen
        .lines()
        .iter()
        .position(|line| line.contains("authored"))
        .unwrap();
    frozen
        .select_range(
            CopyPosition {
                line: row,
                column: 0,
            },
            CopyPosition {
                line: row,
                column: mez_mux::render::char_count(&frozen.lines()[row]),
            },
        )
        .unwrap();
    assert_eq!(frozen.copy_selection().unwrap(), source);
    service.insert_active_copy_mode_for_presented_surface("%1", frozen.clone());
    let mut previous_lineage = service
        .agent_pane_screen_lineage("%1", &conversation)
        .unwrap();
    for (enabled, format, prefix, width) in [
        (true, "%S:%M:%H", "03:02:01", 8),
        (true, "[%H:%M]", "[01:02]", 7),
        (true, "%H:%M", "01:02", 5),
        (false, "%S:%M", "03:02", 5),
        (true, "UTC %H%M%S %%", "UTC 010203 %", 11),
    ] {
        replace_format(&mut service, enabled, 80, format);
        let lineage = service
            .agent_pane_screen_lineage("%1", &conversation)
            .unwrap();
        assert_ne!(lineage, previous_lineage);
        previous_lineage = lineage;
        let screen = service.agent_pane_screen("%1").unwrap();
        assert_eq!(screen.size(), size);
        assert_eq!(screen.normal_styled_content_lines(), original);
        assert_eq!(
            service.agent_prompt_editable_body_width("%1").unwrap(),
            composer
        );
        assert_eq!(
            service
                .active_copy_mode_for_presented_surface("%1")
                .unwrap(),
            &frozen
        );
        let mut fresh = CopyMode::from_screen(screen, 24).unwrap();
        fresh.set_agent_surface(true);
        let row = fresh
            .lines()
            .iter()
            .position(|line| line.contains("authored"))
            .unwrap();
        if enabled {
            assert!(fresh.lines()[row].starts_with(&format!("{prefix} │ ")));
            assert_eq!(fresh.log_frame(row).unwrap().body_origin, width + 3);
            fresh
                .select_range(
                    CopyPosition {
                        line: row,
                        column: 0,
                    },
                    CopyPosition {
                        line: row,
                        column: width + 1,
                    },
                )
                .unwrap();
            assert_eq!(fresh.copy_selection().unwrap(), "");
            assert_eq!(
                fresh
                    .copy_selection_with_format(CopySelectionFormat::Source)
                    .unwrap(),
                ""
            );
        } else {
            assert!(fresh.log_frame(row).is_none());
            assert!(fresh.lines()[row].starts_with("│ "));
        }
        let position = fresh
            .search("01:02:03", mez_mux::copy::SearchDirection::Forward)
            .unwrap()
            .unwrap();
        assert!(position.column >= if enabled { width + 3 } else { 2 });
        assert_eq!(fresh.copy_selection().unwrap(), "01:02:03");
        fresh
            .select_range(
                CopyPosition {
                    line: row,
                    column: 0,
                },
                CopyPosition {
                    line: row,
                    column: mez_mux::render::char_count(&fresh.lines()[row]),
                },
            )
            .unwrap();
        assert_eq!(fresh.copy_selection().unwrap(), source);
        assert_eq!(
            fresh
                .copy_selection_with_format(CopySelectionFormat::Source)
                .unwrap(),
            source
        );
    }
}

/// Format-only reload while OFF cannot change canonical bytes/styles/wrapping
/// or activate timestamps. Invalid type/grammar replacement is atomic across
/// live settings, screen/source ownership and the compositor's enabled policy.
#[test]
fn runtime_log_timestamp_format_off_and_invalid_reload_are_atomic() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 literal %H")
        .unwrap();
    let original = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    for format in ["%H:%M", "%M:%H", "%%"] {
        replace_format(&mut service, false, 80, format);
        assert!(!service.presentation.settings.terminal_agent_log_timestamps);
        assert_eq!(
            service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_styled_content_lines(),
            original
        );
        let copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
        assert!(
            copy.lines()
                .iter()
                .filter(|line| line.contains("literal"))
                .all(|line| line.starts_with("│ "))
        );
    }
    let before = service.agent_pane_screen("%1").unwrap().clone();
    let settings = service.presentation.settings.clone();
    for value in [
        "true",
        "42",
        "[]",
        "{}",
        "'%Y'",
        "''",
        "'%_H'",
        "'%H%H%H%H%H%H%H%H%H%H%H%H%H%H%H%H%H'",
    ] {
        assert!(service.replace_config_layers(vec![ConfigLayer {
            name: "invalid-format".into(), path: None, format: ConfigFormat::Toml,
            scope: ConfigScope::Primary, trusted: true,
            text: format!("[terminal]\nagent_log_timestamps = true\nagent_log_timestamp_format = {value}\n"),
        }]).is_err());
        assert_eq!(service.presentation.settings, settings);
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    }
}

/// The full timestamp margin is suppressed below width+13 and admitted exactly
/// at it even in a wide physical pane. Unicode cell expansion and continuation
/// rows remain bounded; literal-only stamps still require occurrence proof.
#[test]
fn runtime_log_timestamp_format_cap_threshold_bounds_unicode_and_continuations() {
    for (format, prefix, width) in [
        ("%H:%M", "01:02", 5),
        ("%H:%M:%S", "01:02:03", 8),
        ("[%H:%M:%S]", "[01:02:03]", 10),
        ("%%", "%", 1),
    ] {
        for cap in [width + 12, width + 13, width + 14] {
            let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
            replace_format(&mut service, true, cap, format);
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
            );
            service
                .append_agent_assistant_text_to_terminal_buffer(
                    "%1",
                    "雪 1️⃣ 👩‍💻 ★️ wide\nmore wrapped content continues",
                )
                .unwrap();
            let copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
            let rows = copy.visible_styled_lines();
            let rows = rows
                .iter()
                .filter(|row| !row.text.is_empty())
                .collect::<Vec<_>>();
            assert!(rows.len() > 1);
            assert!(rows.iter().all(|row| mez_terminal::terminal_text_width(
                &row.text,
                mez_terminal::terminal_emoji_width()
            ) <= cap));
            assert_eq!(
                rows.iter()
                    .filter(|row| row.text.starts_with(&format!("{prefix} │ ")))
                    .count(),
                usize::from(cap >= width + 13)
            );
            if cap >= width + 13 {
                assert!(rows.iter().skip(1).all(|row| {
                    row.text
                        .starts_with(&format!("{}│ ", " ".repeat(width + 1)))
                }));
            }
        }
    }
}

/// A captured resize worker with equal geometry but a different compiled
/// directive order is stale. Fresh work keeps the original identity/instant
/// despite a changed injected clock and never rewrites persisted source.
#[test]
fn runtime_log_timestamp_format_same_width_reload_rejects_stale_resize() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_format(&mut service, true, 80, "%H:%M:%S");
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "timestamp-format-resize",
    )));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "**original %H source**",
            "text/markdown",
        )
        .unwrap();
    service
        .apply_pane_resize_completion_event("%1", Size::new(40, 20).unwrap())
        .unwrap();
    let captured = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .unwrap();
    replace_format(&mut service, true, 80, "%S:%M:%H");
    let before = service.agent_pane_screen("%1").unwrap().clone();
    let stale = RuntimeSessionService::build_agent_presentation_resize(captured)
        .unwrap()
        .unwrap();
    assert!(
        !service
            .apply_agent_presentation_resize_result(stale)
            .unwrap()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
    let current = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .unwrap();
    let current = RuntimeSessionService::build_agent_presentation_resize(current)
        .unwrap()
        .unwrap();
    assert!(
        service
            .apply_agent_presentation_resize_result(current)
            .unwrap()
    );
    let screen = service.agent_pane_screen("%1").unwrap();
    assert!(
        screen
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .all(|entry| entry.occurrence.unix_millis == Some(3_723_000))
    );
    assert!(
        CopyMode::from_screen(screen, 20)
            .unwrap()
            .lines()
            .iter()
            .any(|row| row.starts_with("03:02:01 │ "))
    );
}
