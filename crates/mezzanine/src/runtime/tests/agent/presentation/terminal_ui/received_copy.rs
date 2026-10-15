//! Receiver copy qualification after actual offline MMP acceptance.
//!
//! Delivery commits transport/context once before a deferred presenter runs.
//! Copy and replay consume that retained source, never resend transport or
//! manufacture replacement identities to work around presentation failures.

use super::*;
use mez_agent::messaging::{Envelope, MessageScope, Recipient};
use mez_core::ids::PaneId;

/// A one-cell selection on the last retained producer-owned row must recover
/// that exact semantic source, not a visible fragment or control marker. OFF
/// row location uses canonical proof without changing copy consumption policy.
fn assert_tail_source_copy(screen: &TerminalScreen, identity: &str, payload: &str) {
    let canonical = screen.normal_styled_content_lines();
    let mut copy = CopyMode::from_screen(screen, 12).unwrap();
    copy.set_agent_surface(true);
    let row = (0..copy.line_count())
        .rfind(|row| {
            copy.copy_source(*row)
                .is_some_and(|source| source.id.owner() == identity)
                || (!screen.log_frame_settings().0.timestamps
                    && canonical
                        .get(*row)
                        .and_then(|line| line.copy_source.as_ref())
                        .is_some_and(|source| source.id.owner() == identity))
        })
        .expect("retained producer-owned tail");
    let origin = copy.log_frame(row).map_or(2, |frame| frame.body_origin);
    assert!(mez_mux::render::char_count(&copy.lines()[row]) > origin);
    copy.select_range(
        CopyPosition {
            line: row,
            column: origin,
        },
        CopyPosition {
            line: row,
            column: origin + 1,
        },
    )
    .unwrap();
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        payload,
        "tail copy: size={:?}, timestamps={}, row={}, copy={:?}",
        screen.size(),
        screen.log_frame_settings().0.timestamps,
        row,
        copy.copy_line(row)
    );
}

/// A peer's wrapped continuation and pruned tail retain exact source ownership
/// with timestamps initially OFF or ON. Literal carrier/control bytes remain
/// authored input for plaintext SourceLine and Markdown WholeEntry producers.
#[test]
fn runtime_received_source_copy_recovers_continuation_and_pruned_tail() {
    for content_type in ["text/plain; charset=utf-8", "text/markdown; charset=utf-8"] {
        for timestamps in [false, true] {
            let mut service = test_runtime_service();
            super::timestamp_settings::replace_timestamp_settings(&mut service, timestamps, 28, "");
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(28, 12).unwrap(), 300).unwrap(),
            );
            let identity = "peer-message recipient=agent-%1 sequence=1 id=turn:tail:copy";
            let payload = format!(
                "{} tail:雪%3A\u{1e}mez-copy-source-line:literal/0:raw",
                "long wrapped source ".repeat(12)
            );
            service
                .append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
                    "%1",
                    Some(identity),
                    "agent-%3",
                    content_type,
                    &payload,
                )
                .unwrap();
            let screen = service.agent_pane_screen("%1").unwrap();
            let owner = screen
                .normal_styled_content_lines()
                .iter()
                .find_map(|row| {
                    row.copy_source
                        .as_ref()
                        .map(|source| source.id.owner().to_string())
                })
                .expect("actual captured producer owner");
            assert_tail_source_copy(screen, &owner, &payload);
            let mut pruned = screen.clone();
            pruned.resize(Size::new(12, 3).unwrap()).unwrap();
            pruned.set_history_limit(1).unwrap();
            let rows = pruned.normal_styled_content_lines();
            let owned = rows
                .iter()
                .filter(|row| {
                    row.copy_source
                        .as_ref()
                        .is_some_and(|source| source.id.owner() == owner)
                })
                .collect::<Vec<_>>();
            assert!(!owned.is_empty());
            assert!(
                owned
                    .iter()
                    .all(|row| !row.presentation.as_ref().unwrap().first_row)
            );
            assert_tail_source_copy(&pruned, &owner, &payload);
        }
    }
}

/// Supported source-free numeric/grouped carriers persist and replay as opaque
/// explicit copy overrides. Literal percent-like legacy identities are never
/// decoded into a new namespace; new escaped anchors use the same lookup.
#[test]
fn runtime_received_copy_retained_legacy_carriers_roundtrip() {
    for identity in ["7", "legacy/7", "legacy%3Agroup/7", "turn%3Aaction%253A/7"] {
        let mut service = test_runtime_service();
        let conversation = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let store = AgentTranscriptStore::new(temp_root("retained-copy-carriers"));
        let raw = "authored:colon/0: 雪%3A";
        let entry = crate::storage::transcript::AgentPresentationEntry {
            presentation: None,
            conversation_id: conversation.clone(),
            sequence: 1,
            created_at_unix_seconds: 10,
            pane_id: "%1".into(),
            turn_id: None,
            terminal_width: 256,
            style_names: vec!["status".into(); 2],
            display_lines: vec!["first".into(), "last".into()],
            copy_lines: vec![
                format!("{}{identity}:{raw}", mez_mux::copy::COPY_SOURCE_LINE_PREFIX),
                format!("{}{identity}", mez_mux::copy::COPY_SOURCE_REFERENCE_PREFIX),
            ],
            ansi_text: None,
            source_text: None,
            source_content_type: None,
        };
        store.append_presentation(&entry).unwrap();
        let persisted = store.inspect_presentation(&conversation).unwrap();
        assert_eq!(persisted, vec![entry]);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(256, 12).unwrap(), 100).unwrap(),
        );
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &persisted)
            .unwrap();
        let mut copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 12).unwrap();
        copy.set_agent_surface(true);
        let row = copy
            .lines()
            .iter()
            .rposition(|line| line.contains("last"))
            .unwrap();
        copy.select_range(
            CopyPosition {
                line: row,
                column: 2,
            },
            CopyPosition {
                line: row,
                column: 3,
            },
        )
        .unwrap();
        assert_eq!(
            copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                .unwrap(),
            raw
        );
        assert_eq!(
            store.inspect_presentation(&conversation).unwrap(),
            persisted
        );
    }
}

/// Selects all rows owned by the committed receiver occurrence, and then one
/// body cell. The full selection emits original source ordinals exactly once;
/// partial selection recovers only its complete semantic source line.
fn assert_received_copy(
    service: &RuntimeSessionService,
    identity: &str,
    payload: &str,
    stage: &str,
) {
    let screen = service.agent_pane_screen("%1").unwrap();
    let mut copy = CopyMode::from_screen(screen, 12).unwrap();
    copy.set_agent_surface(true);
    let canonical = screen.normal_styled_content_lines();
    let owned = (0..copy.line_count())
        .filter(|row| {
            copy.copy_source(*row)
                .is_some_and(|source| source.id.owner() == identity)
                || (!screen.log_frame_settings().0.timestamps
                    && canonical
                        .get(*row)
                        .and_then(|line| line.copy_source.as_ref())
                        .is_some_and(|source| source.id.owner() == identity))
        })
        .collect::<Vec<_>>();
    assert!(
        !owned.is_empty(),
        "missing receiver source: {:?}",
        copy.lines()
    );
    let first = owned[0];
    let last = *owned.last().unwrap();
    let end_column = mez_mux::render::char_count(&copy.lines()[last]);
    copy.select_range(
        CopyPosition {
            line: first,
            column: 0,
        },
        CopyPosition {
            line: last,
            column: end_column,
        },
    )
    .unwrap();
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        payload,
        "stage={stage}, size={:?}, timestamps={}, rows={:#?}",
        screen.size(),
        screen.log_frame_settings().0.timestamps,
        (0..copy.line_count())
            .map(|row| (
                row,
                &copy.lines()[row],
                copy.copy_line(row),
                canonical
                    .get(row)
                    .and_then(|line| line.copy_text.as_deref()),
                canonical
                    .get(row)
                    .and_then(|line| line.copy_source.as_ref())
                    .map(|source| (&source.id, source.text.as_ref()))
            ))
            .collect::<Vec<_>>()
    );
    let origin = copy.log_frame(first).map_or(2, |frame| frame.body_origin);
    copy.select_range(
        CopyPosition {
            line: first,
            column: origin,
        },
        CopyPosition {
            line: first,
            column: origin + 1,
        },
    )
    .unwrap();
    assert_eq!(
        copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        payload.lines().next().unwrap()
    );
}

/// A real accepted colon-bearing envelope commits one context/cursor/receipt
/// before an injected display failure. Flushing, empty redelivery sweeps and
/// replay at narrower geometry must preserve exact payload, occurrence and
/// eligibility without another acceptance, context block or durable record.
#[test]
fn runtime_received_colon_carrier_flush_copy_replay_is_once_only() {
    for timestamps in [false, true] {
        let mut service = test_runtime_service();
        super::timestamp_settings::replace_timestamp_settings(&mut service, timestamps, 28, "");
        service
            .attach_primary("primary", true, Size::new(28, 12).unwrap(), 300)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let store = AgentTranscriptStore::new(temp_root("received-colon-copy"));
        service.set_agent_transcript_store(store.clone());
        let conversation = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let started = service
            .start_agent_prompt_turn("%1", "receive synthetic copy evidence")
            .unwrap();
        service.remove_pending_agent_provider_task(&started.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(28, 12).unwrap(), 300).unwrap(),
        );
        let now = crate::runtime::current_unix_millis();
        let recipient = service
            .ensure_runtime_message_identity("agent-%1", PaneId::opaque("%1"), "agent", &[], now)
            .unwrap();
        service
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
        let sender = service
            .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now)
            .unwrap();
        let message_id = "turn:action:copy/雪 space:%3A";
        let payload = "body:colon/雪 long wrapped source\n\u{1e}mez-copy-source-line:literal/7:raw\n\u{1e}mez-copy-source-reference:literal/0\u{1e}mez-copy-skip-line";
        let delivery = service
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: message_id.into(),
                    message_type: "send".into(),
                    time: format!("runtime:{now}"),
                    sender: sender.clone(),
                    recipient: Recipient::Agent(recipient.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".into(),
                    payload: payload.into(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now,
            )
            .unwrap();
        let identity = format!(
            "peer-message recipient={} sequence={} id={message_id}",
            recipient.agent_id, delivery.sequence
        );
        service.fail_next_received_peer_message_presentation_for_tests();
        assert_eq!(
            service.deliver_pending_runtime_agent_messages(now).unwrap(),
            1
        );
        let pending = service.snapshot_unsettled_received_peer_message_presentations();
        assert_eq!(pending.len(), 1);
        let receipt = &pending[0];
        assert_eq!(receipt.identity, identity);
        assert_eq!(receipt.payload, payload);
        assert!(receipt.presentation_eligible);
        assert!(!receipt.live_rendered);
        assert!(receipt.occurrence.is_valid());
        receipt.validate().unwrap();
        assert_eq!(
            service
                .message_service()
                .subscription(&recipient.agent_id)
                .unwrap()
                .last_sequence,
            delivery.sequence
        );
        let context_label = crate::runtime::control::runtime_peer_message_block_label(
            delivery.sequence,
            message_id,
        );
        let count_context = |service: &RuntimeSessionService| {
            service
                .agent_turn_contexts()
                .values()
                .flat_map(|context| context.blocks())
                .filter(|block| {
                    block.source == ContextSourceKind::PeerMessage && block.label == context_label
                })
                .count()
        };
        assert_eq!(count_context(&service), 1);
        let committed_messages = service.message_service().snapshot_state();
        assert_eq!(
            committed_messages
                .accepted_messages
                .iter()
                .filter(|message| message.envelope.id == message_id)
                .count(),
            1
        );
        service.flush_received_peer_message_presentations().unwrap();
        assert_received_copy(&service, &identity, payload, "live flush");
        let installed = service.agent_pane_screen("%1").unwrap().clone();
        service.flush_received_peer_message_presentations().unwrap();
        assert_eq!(
            service.deliver_pending_runtime_agent_messages(now).unwrap(),
            0
        );
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &installed);
        assert!(
            service
                .snapshot_unsettled_received_peer_message_presentations()
                .is_empty()
        );
        let entries = store
            .inspect_presentation(&conversation)
            .unwrap()
            .into_iter()
            .filter(|entry| {
                crate::runtime::render::peer_message_presentation_receive_identity(
                    entry.source_content_type.as_deref().unwrap_or_default(),
                    entry.source_text.as_deref().unwrap_or_default(),
                )
                .as_deref()
                    == Some(identity.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].presentation.as_ref().unwrap().occurrence,
            receipt.occurrence
        );
        let source: serde_json::Value =
            serde_json::from_str(entries[0].source_text.as_deref().unwrap()).unwrap();
        assert_eq!(source["payload"], payload);
        assert_eq!(source["presentation_eligible"], true);
        for size in [Size::new(18, 5).unwrap(), Size::new(36, 8).unwrap()] {
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(size, 300).unwrap(),
            );
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap();
            assert_received_copy(&service, &identity, payload, "semantic replay");
            let rows = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_styled_content_lines();
            let provenance = rows
                .iter()
                .filter_map(|row| row.presentation.as_ref())
                .collect::<Vec<_>>();
            assert!(!provenance.is_empty());
            assert!(
                provenance
                    .iter()
                    .all(|entry| entry.id == identity && entry.occurrence == receipt.occurrence)
            );
            assert_eq!(provenance.iter().filter(|entry| entry.first_row).count(), 1);
            service
                .resize_agent_screen_with_pending_steering("%1", Size::new(24, 6).unwrap())
                .unwrap();
            assert_received_copy(&service, &identity, payload, "physical resize");
        }
        assert_eq!(count_context(&service), 1);
        assert_eq!(
            service.message_service().snapshot_state(),
            committed_messages
        );
        assert_eq!(
            store
                .inspect_presentation(&conversation)
                .unwrap()
                .iter()
                .filter(|entry| {
                    crate::runtime::render::peer_message_presentation_receive_identity(
                        entry.source_content_type.as_deref().unwrap_or_default(),
                        entry.source_text.as_deref().unwrap_or_default(),
                    )
                    .as_deref()
                        == Some(identity.as_str())
                })
                .count(),
            1
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// The receiver's raw source must not inherit display-only CRLF normalization
/// or trailing-line trimming. Exercise both semantic line and whole-entry peer
/// routes before any timestamp toggle; the persisted payload is not a substitute
/// for correct live source copy.
#[test]
fn runtime_received_copy_preserves_authored_line_endings() {
    for content_type in ["text/plain; charset=utf-8", "text/markdown; charset=utf-8"] {
        for timestamps in [false, true] {
            let mut service = test_runtime_service();
            super::timestamp_settings::replace_timestamp_settings(&mut service, timestamps, 80, "");
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
            );
            let payload = "first: 雪\r\nsecond\r\n\n";
            service
                .append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
                    "%1",
                    Some("peer-message recipient=agent-%1 sequence=1 id=turn:line:endings"),
                    "agent-%3",
                    content_type,
                    payload,
                )
                .unwrap();
            let mut copy =
                CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 12).unwrap();
            copy.set_agent_surface(true);
            let last = copy
                .lines()
                .iter()
                .rposition(|line| line.contains("second"))
                .unwrap();
            let end_column = mez_mux::render::char_count(&copy.lines()[last]);
            copy.select_range(
                CopyPosition { line: 0, column: 0 },
                CopyPosition {
                    line: last,
                    column: end_column,
                },
            )
            .unwrap();
            assert_eq!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap(),
                payload,
                "content_type={content_type}, timestamps={timestamps}"
            );
            for (line, raw) in [(0, "first: 雪\r"), (last, "second\r\n\n")] {
                let origin = copy.log_frame(line).map_or(2, |frame| frame.body_origin);
                copy.select_range(
                    CopyPosition {
                        line,
                        column: origin,
                    },
                    CopyPosition {
                        line,
                        column: origin + 1,
                    },
                )
                .unwrap();
                assert_eq!(
                    copy.copy_selection_with_format(
                        crate::host::terminal::CopySelectionFormat::Source
                    )
                    .unwrap(),
                    if content_type.starts_with("text/markdown") {
                        payload
                    } else {
                        raw
                    },
                    "partial source ordinal: content_type={content_type}, timestamps={timestamps}"
                );
            }
        }
    }
}
