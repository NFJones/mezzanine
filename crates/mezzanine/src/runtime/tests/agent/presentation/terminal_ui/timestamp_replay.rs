//! Feature-ON promotion, durable replay and fork occurrence/copy invariants.
//!
//! Exact validated source transfers the first observation without another
//! render clock. Persistence/catalog chronology remains independently owned;
//! no provider request or process transaction is executed by these fixtures.

use super::timestamp_settings::replace_timestamp_settings;
use super::*;

/// A completed streamed Markdown source promotes once, persists original
/// evidence and rerenders through resize/fork without another timestamp or
/// source mutation. Changed actor/catalog clocks cannot replace the captured
/// observation. Every width exposes the original source through copy mode.
#[test]
fn runtime_log_timestamps_stream_promotion_replay_and_fork_keep_original_entry() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    let store = AgentTranscriptStore::new(temp_root("timestamp-promotion-fork"));
    service.set_agent_transcript_store(store.clone());
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    let source = "**original source**\n\n雪 and 01:02:03 authored";
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.into(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: source.into(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projected = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projected)
            .unwrap()
    );
    let original = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .iter()
        .find_map(|row| row.presentation.clone())
        .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: source.into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![mez_agent::AgentAction {
                    id: "timestamp-say".into(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: source.into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.into(),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
    assert_eq!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap(),
        std::collections::BTreeSet::from([0])
    );
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    assert_eq!(execution.response.raw_text, source);
    let entries = store.inspect_presentation(&conversation).unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .any(|entry| presentation_semantic_source(entry).as_deref() == Some(source))
    );
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation.clone(),
            sequence: 1,
            created_at_unix_seconds: 3723,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: source.into(),
        })
        .unwrap();
    store.fork(&conversation, "timestamp-fork", 99_999).unwrap();
    let fork = "timestamp-fork".to_string();
    for (owner, records) in [
        (&conversation, entries),
        (&fork, store.inspect_presentation(&fork).unwrap()),
    ] {
        for width in [80, 32, 48] {
            let mut replay = test_runtime_service_with_size(Size::new(80, 32).unwrap());
            replace_timestamp_settings(&mut replay, true, 80, "");
            replay.presentation.occurrence_clock_millis = Some(Some(199_999_000));
            replay
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            // This is the resume/fork binding primitive, not an execution
            // lineage or full /resume command. The stored owner/count remain
            // authoritative; replay guards and record IDs are unchanged.
            let transcript_entries = store.inspect(owner).unwrap().len();
            assert_eq!(transcript_entries, 1);
            replay
                .agent_shell_store_mut()
                .bind_conversation_with_lineage(
                    "%1",
                    owner,
                    u64::try_from(transcript_entries).unwrap(),
                    None,
                )
                .unwrap();
            replay.set_agent_transcript_store(store.clone());
            set_agent_pane_screen_for_test(
                &mut replay,
                "%1",
                TerminalScreen::new(Size::new(width, 24).unwrap(), 200).unwrap(),
            );
            replay
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &records)
                .unwrap();
            let canonical = replay
                .agent_pane_screen("%1")
                .unwrap()
                .normal_styled_content_lines();
            let retained = canonical
                .iter()
                .filter_map(|row| row.presentation.as_ref())
                .collect::<Vec<_>>();
            assert!(retained.iter().all(|entry| entry.id == original.id && entry.occurrence == original.occurrence), "owner={owner} width={width}");
            assert_eq!(retained.iter().filter(|entry| entry.first_row).count(), 1);
            let mut copy =
                CopyMode::from_screen(replay.agent_pane_screen("%1").unwrap(), 24).unwrap();
            copy.set_agent_surface(true);
            assert_eq!(
                copy.lines()
                    .iter()
                    .filter(|row| row.starts_with("01:02:03 │ "))
                    .count(),
                1
            );
            let first = copy
                .lines()
                .iter()
                .position(|row| row.contains("original source"))
                .unwrap();
            let end = mez_mux::render::char_count(&copy.lines()[first]);
            copy.select_range(
                CopyPosition {
                    line: first,
                    column: 11,
                },
                CopyPosition {
                    line: first,
                    column: end,
                },
            )
            .unwrap();
            assert!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap()
                    .contains("**original source**")
            );
            let last = copy
                .lines()
                .iter()
                .rposition(|row| row.contains("authored"))
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
            assert_eq!(
                copy.copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
                    .unwrap(),
                source
            );
        }
    }
}
