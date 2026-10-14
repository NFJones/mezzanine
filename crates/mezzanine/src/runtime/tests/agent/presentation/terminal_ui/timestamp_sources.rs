//! Finite source-kind qualification at actual structured producer boundaries.
//! Literal source bytes are never ownership markers; immutable group IDs are
//! retained independently of the visual clock policy and physical wrapping.

use super::timestamp_settings::replace_timestamp_settings;
use super::*;
use mez_core::presentation::PresentationEntry;
use mez_terminal::GraphicRendition;

/// A raw-cleared wrapped continuation must invalidate the first row's typed AND
/// legacy source through actual history reflow and CopyMode construction. Valid
/// typed and ordinary untyped legacy histories remain source-copy compatible;
/// rendered bytes are retained regardless of source validity.
#[test]
fn runtime_log_timestamps_history_copy_keeps_merge_source_invalidation() {
    for mode in [
        "valid-typed",
        "legacy",
        "raw-cleared",
        "foreign-owner",
        "mixed-generation",
    ] {
        let mut screen = TerminalScreen::new(Size::new(24, 6).unwrap(), 32).unwrap();
        screen.set_log_frame_settings(
            mez_core::presentation::LogFramePolicy {
                timestamps: true,
                wrap_cap: 24,
            },
            GraphicRendition::default(),
        );
        let entry = PresentationEntry {
            id: "history-owner".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        let source = mez_terminal::TerminalCopySource::new(
            mez_terminal::TerminalCopySourceId::new(
                &entry,
                mez_terminal::TerminalCopySourceGroup::WholeEntry,
            )
            .unwrap(),
            "original source",
        );
        let body = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        if mode == "legacy" {
            screen.feed_presentation(body.as_bytes(), entry.clone());
        } else {
            assert!(screen.feed_presentation_with_copy_source(
                body.as_bytes(),
                entry.clone(),
                Some(source.clone())
            ));
        }
        let encoded = format!(
            "{}0:original source",
            mez_mux::copy::COPY_SOURCE_LINE_PREFIX
        );
        screen.set_recent_normal_copy_texts(&[encoded], mez_mux::copy::COPY_SKIP_LINE);
        if mode == "raw-cleared" {
            screen.feed(b"\x1b[2;1H!");
        }
        if matches!(mode, "foreign-owner" | "mixed-generation") {
            screen.feed(b"\x1b[2;1H");
            let mut other = entry.clone();
            other.first_row = false;
            if mode == "foreign-owner" {
                other.id = "different-owner".into();
            }
            let changed = mez_terminal::TerminalCopySource::projected(
                mez_terminal::TerminalCopySourceId::new(
                    &other,
                    mez_terminal::TerminalCopySourceGroup::WholeEntry,
                )
                .unwrap(),
                "changed source",
                2,
            );
            assert!(screen.feed_presentation_with_copy_source(b"!", other, Some(changed)));
            let rows = screen.visible_styled_lines();
            assert_eq!(
                rows[0].copy_source.as_ref().unwrap().text.as_ref(),
                "original source"
            );
            assert_eq!(
                rows[1].copy_source.as_ref().unwrap().text.as_ref(),
                "changed source"
            );
        }
        screen.feed(b"\x1b[6;1H\r\n\r\n\r\n\r\n\r\n\r\n");
        assert!(
            !screen
                .visible_lines()
                .iter()
                .any(|line| line.contains("ABCDEFGHIJKLMNO") || line.contains("0123456789"))
        );
        let canonical = screen.normal_content_lines().join("\n");
        assert!(canonical.contains("ABCDEFGHIJKLMNO"));
        if !matches!(mode, "valid-typed" | "legacy") {
            assert!(canonical.contains('!'));
        }
        let mut copy = CopyMode::from_screen(&screen, 6).unwrap();
        copy.set_agent_surface(true);
        let first = copy
            .lines()
            .iter()
            .position(|line| line.contains("ABCDEFGHIJKLMNO"))
            .unwrap();
        let last = copy
            .lines()
            .iter()
            .rposition(|line| line.contains("0123456789") || line.contains("89"))
            .unwrap();
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
        let recovered = copy
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap();
        assert_eq!(
            recovered,
            if !matches!(mode, "valid-typed" | "legacy") {
                ""
            } else {
                "original source"
            },
            "{mode}"
        );
        assert!(
            copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Rendered)
                .unwrap()
                .contains("ABCDEFGHIJKLMNO")
        );
    }
}

/// Actual server JSON retains visual rows/styles but never serializes process-
/// local source Arcs. Markdown link URLs are intentionally visual dim text;
/// raw markup is distinct source evidence, not an extra serialized payload.
#[test]
fn runtime_log_timestamps_wire_json_omits_local_copy_source_payload() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    let source = "[visible label](https://copy-source-only.invalid/private-owned-link)";
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            source,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines()
            .iter()
            .filter_map(|row| row.copy_source.as_ref())
            .any(|owner| owner.text.as_ref() == source)
    );
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 32).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let json = crate::runtime::json::rendered_client_view_json(&view, None);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let (visual, spans) =
        mez_mux::presentation::compose_client_presentation_with_styles(&view, None);
    assert!(
        visual
            .iter()
            .any(|line| line.contains("copy-source-only.invalid"))
    );
    assert!(!visual.iter().any(|line| line.contains(source)));
    assert_eq!(value["lines"], serde_json::json!(visual));
    let color = |color| match color {
        Some(mez_terminal::TerminalColor::Indexed(index)) => {
            serde_json::json!({"kind":"indexed","index":index})
        }
        Some(mez_terminal::TerminalColor::Rgb(red, green, blue)) => {
            serde_json::json!({"kind":"rgb","red":red,"green":green,"blue":blue})
        }
        None => serde_json::Value::Null,
    };
    let expected_spans = serde_json::json!(spans.iter().map(|line| line.iter().map(|span| {
        let r = span.rendition;
        serde_json::json!({"start":span.start,"length":span.length,"rendition":{
            "bold":r.bold,"dim":r.dim,"italic":r.italic,"underline":r.underline,"double_underline":r.double_underline,
            "strikethrough":r.strikethrough,"inverse":r.inverse,"hidden":r.hidden,"foreground":color(r.foreground),"background":color(r.background)
        }})
    }).collect::<Vec<_>>()).collect::<Vec<_>>());
    assert_eq!(value["line_style_spans"], expected_spans);
    let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
    copy.set_agent_surface(true);
    let row = (0..copy.line_count())
        .find(|row| copy.copy_source(*row).is_some())
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
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        source
    );
    assert!(
        value
            .get("lines")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("visible label"))
    );
    assert!(!json.contains(source));
    assert!(!json.contains("copy_source"));
    assert!(!json.contains("copySource"));
}

/// Co-resident immutable versions under one semantic ID fail Source copy
/// closed rather than treating revisions as separate occurrences or recovering
/// legacy payloads. Independently frozen old snapshots remain valid and exact.
#[test]
fn runtime_log_timestamps_copy_rejects_mixed_source_generations() {
    let entry = PresentationEntry {
        id: "source-generation".into(),
        occurrence: Default::default(),
        first_row: true,
    };
    let id = mez_terminal::TerminalCopySourceId::new(
        &entry,
        mez_terminal::TerminalCopySourceGroup::WholeEntry,
    )
    .unwrap();
    let original = mez_terminal::TerminalCopySource::projected(id.clone(), "original", 1);
    for changed in [
        mez_terminal::TerminalCopySource::projected(id.clone(), "grown", 2),
        mez_terminal::TerminalCopySource::new(id.clone(), "original"),
        mez_terminal::TerminalCopySource::projected(id.clone(), "conflicting", 1),
    ] {
        let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 16).unwrap();
        screen.set_log_frame_settings(
            mez_core::presentation::LogFramePolicy {
                timestamps: true,
                wrap_cap: 80,
            },
            GraphicRendition::default(),
        );
        assert!(screen.feed_presentation_with_copy_source(
            b"first\r\n",
            entry.clone(),
            Some(original.clone())
        ));
        let mut old = CopyMode::from_screen(&screen, 8).unwrap();
        old.set_agent_surface(true);
        let end = mez_mux::render::char_count(&old.lines()[0]);
        old.select_range(
            CopyPosition { line: 0, column: 0 },
            CopyPosition {
                line: 0,
                column: end,
            },
        )
        .unwrap();
        assert_eq!(
            old.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                .unwrap(),
            "original"
        );
        let mut continuation = entry.clone();
        continuation.first_row = false;
        assert!(screen.feed_presentation_with_copy_source(b"second", continuation, Some(changed)));
        screen.set_recent_normal_copy_texts(&["legacy fallback".into()], "legacy continuation");
        let mut mixed = CopyMode::from_screen(&screen, 8).unwrap();
        mixed.set_agent_surface(true);
        let end = mez_mux::render::char_count(&mixed.lines()[1]);
        mixed
            .select_range(
                CopyPosition { line: 0, column: 0 },
                CopyPosition {
                    line: 1,
                    column: end,
                },
            )
            .unwrap();
        assert!(
            mixed
                .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                .is_err()
        );
        assert!(
            mixed
                .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Rendered)
                .is_ok()
        );
        assert_eq!(
            old.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                .unwrap(),
            "original"
        );
    }
}

/// Actual cumulative stream updates install a newer generation atomically under
/// existing work admission. A retained old copy view never changes its payload,
/// while a newly opened view sees the exact grown source with the same entry.
#[test]
fn runtime_log_timestamps_stream_growth_keeps_frozen_source_snapshot() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "growth",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.into(),
            },
        )
        .unwrap();
    let mut frozen = None;
    for (index, delta) in ["**original**", " and grown"].into_iter().enumerate() {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "growth",
                &mez_agent::StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: delta.into(),
                },
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "growth")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
        copy.set_agent_surface(true);
        let row = (0..copy.line_count())
            .find(|row| copy.copy_source(*row).is_some())
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
        assert_eq!(
            copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                .unwrap(),
            if index == 0 {
                "**original**"
            } else {
                "**original** and grown"
            }
        );
        if index == 0 {
            frozen = Some(copy);
        }
    }
    assert_eq!(
        frozen
            .unwrap()
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        "**original**"
    );
}

/// OFF-produced raw peers retain exact sentinel-looking payloads before a
/// same-size ON/OFF replacement. Equal text is two distinct occurrences; Source
/// selections emit each complete group once while margin-only selection emits
/// nothing. This uses the real Markdown peer producer, not carrier rewriting.
#[test]
fn runtime_log_timestamps_off_raw_literal_source_survives_toggle() {
    let mut service = test_runtime_service_with_size(Size::new(100, 80).unwrap());
    replace_timestamp_settings(&mut service, false, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 40).unwrap(), 300).unwrap(),
    );
    let source = format!(
        "{}literal\n{}\n{}\n01:02:03 authored",
        mez_mux::copy::COPY_SOURCE_LINE_PREFIX,
        mez_mux::copy::COPY_SOURCE_REFERENCE_PREFIX,
        mez_mux::copy::COPY_SKIP_LINE
    );
    for _ in 0..2 {
        service
            .append_agent_received_peer_message_to_terminal_buffer(
                "%1",
                "agent-%3",
                "text/markdown",
                &source,
            )
            .unwrap();
    }
    let off = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let mut ids = std::collections::BTreeSet::new();
    for row in &off {
        if let Some(owner) = &row.copy_source {
            assert_eq!(owner.text.as_ref(), source);
            ids.insert(owner.id.clone());
        }
    }
    assert_eq!(ids.len(), 2);
    for enabled in [true, false, true] {
        replace_timestamp_settings(&mut service, enabled, 80, "");
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 40).unwrap();
        copy.set_agent_surface(true);
        let rows = (0..copy.line_count())
            .filter(|row| copy.copy_source(*row).is_some())
            .collect::<Vec<_>>();
        assert!(!rows.is_empty());
        let first = rows[0];
        let last = *rows.last().unwrap();
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
            format!("{source}\n{source}")
        );
        if enabled {
            copy.select_range(
                CopyPosition {
                    line: first,
                    column: 0,
                },
                CopyPosition {
                    line: first,
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
    }
}

/// Real code and diagram declarations are captured before width wrapping.
/// Changing width may split physical rows but cannot change RawBlock ordinals,
/// source bytes, owner identity or payload sharing within each semantic block.
#[test]
fn runtime_log_timestamps_code_diagram_groups_are_width_independent() {
    let mut service = test_runtime_service_with_size(Size::new(100, 80).unwrap());
    replace_timestamp_settings(&mut service, false, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 60).unwrap(), 300).unwrap(),
    );
    let source = "```rust\nlet value = 123;\n```\n\n```mermaid\ngraph LR\nA --> B\n```";
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            source,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();
    let groups = |screen: &TerminalScreen| {
        screen
            .normal_styled_content_lines()
            .into_iter()
            .filter_map(|row| row.copy_source)
            .filter(|owner| {
                matches!(
                    owner.id.group(),
                    mez_terminal::TerminalCopySourceGroup::RawBlock(_)
                )
            })
            .map(|owner| (owner.id.clone(), owner.text.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let original = groups(service.agent_pane_screen("%1").unwrap());
    assert_eq!(original.len(), 2);
    for width in [32, 48, 80] {
        replace_timestamp_settings(&mut service, true, width, "");
        service
            .agent_pane_screen_mut("%1")
            .unwrap()
            .resize(Size::new(u16::try_from(width).unwrap(), 60).unwrap())
            .unwrap();
        let screen = service.agent_pane_screen("%1").unwrap();
        assert_eq!(groups(screen), original);
        let mut copy = CopyMode::from_screen(screen, 60).unwrap();
        copy.set_agent_surface(true);
        for row in 0..copy.line_count() {
            if let Some(owner) = copy.copy_source(row).filter(|owner| {
                matches!(
                    owner.id.group(),
                    mez_terminal::TerminalCopySourceGroup::RawBlock(_)
                )
            }) {
                let expected = owner.text.to_string();
                let start = copy.log_frame(row).map_or(2, |frame| frame.body_origin);
                let end = mez_mux::render::char_count(&copy.lines()[row]);
                if end <= start {
                    continue;
                }
                copy.select_range(
                    CopyPosition {
                        line: row,
                        column: start,
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
                    expected
                );
            }
        }
    }
}
