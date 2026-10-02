//! Static/streamed parity, exact promotion, and interrupted-output retention.
//!
//! Accepted sources settle once in their original order and styles. Optional
//! progress cannot change final display, while interruption retains visible text.

use super::*;

/// A complete later say remains buffered behind an open command preview and
/// becomes visible only after the command field closes.
#[test]
fn runtime_streaming_command_closure_releases_later_say() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: "printf first".to_string(),
        },
        mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "later answer".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-command-order",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-command-order")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(before.contains("printf first"), "{before}");
    assert!(!before.contains("later answer"), "{before}");
    service
        .ingest_provider_log(
            "%1",
            "turn-command-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
            ),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-command-order")
            .unwrap()
            .is_none(),
        "command field closure is not whole-action receipt"
    );
    service
        .ingest_provider_log(
            "%1",
            "turn-command-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
            ),
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-command-order")
        .unwrap()
        .expect("command receipt must release buffered answer");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(after.contains("printf first"), "{after}");
    assert!(
        !after.contains("later answer"),
        "command receipt alone cannot finalize its preview: {after}"
    );
}

/// Verifies every published cumulative Markdown and diff prefix is identical
/// to a fresh static render of the same source snapshot.
///
/// Later Markdown fragments may reinterpret prior rows as Setext headings or
/// tables, while a unified diff becomes progressively more structured. Each
/// generation must replace the whole provisional component through the
/// ordinary renderer so no literal tail or stale styling survives.
#[test]
fn runtime_streaming_say_prefixes_match_static_rich_renderers() {
    let cases = [
        (
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
            vec![
                "Heading",
                "\n---",
                "\n\n| Name | Value |",
                "\n| --- | --- |",
                "\n| alpha | beta |",
            ],
        ),
        (
            mez_agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE,
            vec![
                "diff --git a/demo.rs b/demo.rs\n",
                "--- a/demo.rs\n",
                "+++ b/demo.rs\n",
                "@@ -1 +1 @@\n",
                "-old\n",
                "+new\n",
            ],
        ),
    ];

    for (case_index, (content_type, fragments)) in cases.into_iter().enumerate() {
        let mut streaming = test_runtime_service();
        streaming
            .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
            .unwrap();
        streaming
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut streaming,
            "%1",
            TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
        );
        streaming
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-prefix",
                &mez_agent::StreamingSayEvent::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Progress,
                    content_type: content_type.to_string(),
                },
            )
            .unwrap();

        let mut source = String::new();
        for fragment in fragments {
            source.push_str(fragment);
            streaming
                .apply_agent_streaming_say_event_to_terminal_buffer(
                    "%1",
                    "turn-prefix",
                    &mez_agent::StreamingSayEvent::TextDelta {
                        action_index: 0,
                        text: fragment.to_string(),
                    },
                )
                .unwrap();
            let work = streaming
                .take_agent_streaming_say_projection_work("%1", "turn-prefix")
                .unwrap()
                .expect("each non-empty source prefix should be dirty");
            let projection = RuntimeSessionService::build_agent_streaming_say_projection(work)
                .expect("each cumulative source prefix should render");
            assert!(
                streaming
                    .apply_agent_streaming_say_projection_result(projection)
                    .unwrap(),
                "case {case_index} prefix {source:?} should install"
            );

            let mut static_render = test_runtime_service();
            static_render
                .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
                .unwrap();
            set_agent_pane_screen_for_test(
                &mut static_render,
                "%1",
                TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
            );
            static_render
                .append_agent_assistant_content_to_terminal_buffer("%1", &source, content_type)
                .unwrap();

            assert_eq!(
                streaming
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines(),
                static_render
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines(),
                "case {case_index} prefix {source:?} display must match static rendering"
            );
            assert_eq!(
                streaming
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines(),
                static_render
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines(),
                "case {case_index} prefix {source:?} styles must match static rendering"
            );
        }
    }
}

/// Verifies streamed Markdown is the canonical assistant presentation rather
/// than a bounded preview that is replayed after validated completion.
///
/// The prefix must exist before source text arrives, cumulative Markdown must
/// render richly before its source string closes, exact reconciliation must
/// persist the raw source once, and ordinary completion presentation must not
/// append a duplicate assistant block.
#[test]
fn runtime_streaming_say_promotes_rich_output_without_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-say-promotion"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    let started = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(started.contains("mez>"), "{started}");

    let source = "**streamed** output";
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: source.to_string(),
            },
        )
        .unwrap();
    let projection_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("incomplete streamed source should produce projection work");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(projection_work)
        .expect("incomplete streamed source should render off actor");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap(),
        "current incomplete projection should install atomically"
    );
    let rendered_before_completion = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let rendered_text = rendered_before_completion.join("\n");
    assert!(rendered_text.contains("streamed output"), "{rendered_text}");
    assert!(!rendered_text.contains("**streamed**"), "{rendered_text}");
    let streamed_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed output"))
        .expect("streamed Markdown line should be visible");
    assert!(!streamed_line.style_spans.is_empty(), "{streamed_line:?}");
    let projection_before_completion = service.agent_pane_screen("%1").unwrap().clone();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .is_none(),
        "completion without new source must not request another projection"
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap(),
        &projection_before_completion,
        "completion without new source must not alter the visible generation"
    );

    let action = mez_agent::AgentAction {
        id: "say-streamed".to_string(),

        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: source.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: source.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),

                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    assert_eq!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap(),
        std::collections::BTreeSet::from([0])
    );
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        rendered_before_completion
    );
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    let matching = entries
        .iter()
        .filter(|entry| entry.source_text.as_deref() == Some(source))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1, "{entries:?}");
    assert_eq!(
        matching[0].source_content_type.as_deref(),
        Some(mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE)
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "later durable row")
        .unwrap();
    let after_append = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert_eq!(
        after_append
            .iter()
            .filter(|line| line.text.contains("streamed output"))
            .count(),
        1,
        "{after_append:?}"
    );
    assert!(
        after_append
            .iter()
            .any(|line| line.text.contains("later durable row")),
        "{after_append:?}"
    );
}

/// Verifies a newer cumulative source generation that renders identically does
/// not replace the pane screen or request an attached-client redraw.
///
/// A shell summary can repeat the batch rationale exactly. The source revision
/// still advances for reconciliation, but filtering the duplicate thinking row
/// leaves the rendered generation unchanged and must preserve screen lineage.
#[test]
fn runtime_streaming_identical_projection_is_a_screen_noop() {
    let mut service = test_runtime_service();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let thinking = "Inspect the streaming compositor";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: thinking.to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );
    let screen = service.agent_pane_screen("%1").unwrap().clone();
    let lineage = service
        .agent_pane_screen_lineage("%1", &conversation_id)
        .unwrap();

    for event in [
        mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
            action_index: 0,
            text: thinking.to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let duplicate_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();

    assert!(
        !service
            .apply_agent_streaming_say_projection_result(duplicate_projection)
            .unwrap(),
        "an identical screen generation must not request physical output"
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &screen);
    assert_eq!(
        service.agent_pane_screen_lineage("%1", &conversation_id),
        Some(lineage),
        "a no-op projection must preserve screen lineage"
    );
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_streaming_projection_results, 2);
    assert_eq!(metrics.agent_streaming_projection_installs, 1);
    assert_eq!(metrics.agent_streaming_projection_rejections, 0);
}

/// Verifies validated provider completion finalizes streamed say rows in place.
///
/// Production MAAP batches carry a non-empty batch rationale and one result per
/// action. Completion must preserve the streamed assistant block, persist it
/// once, and apply the same final styling as the static renderer without
/// appending a second copy below the provisional rows.
#[tokio::test]
async fn runtime_streaming_say_completion_does_not_append_final_duplicate() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-say-finalization"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "stream the final response")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let rationale = "Report the completed result";
    let source = "**streamed final** output";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: source.to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("complete streamed source should project"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let streamed_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed final"))
        .expect("streamed assistant row should be visible");

    let action = mez_agent::AgentAction {
        id: "say-streamed".to_string(),

        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: source.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: source.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: rationale.to_string(),

                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec![source.to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();

    assert!(transition.applied);
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::PaneOutput,
            ..
        }
    )));
    assert!(transition.side_effects.iter().all(|effect| !matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    let final_screen = service.agent_pane_screen("%1").unwrap();
    let final_lines = final_screen.normal_content_lines();
    assert_eq!(
        final_lines
            .iter()
            .filter(|line| line.contains("streamed final"))
            .count(),
        1,
        "{final_lines:?}"
    );
    let finalized_line = final_screen
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed final"))
        .expect("finalized assistant row should remain visible");
    assert_eq!(finalized_line, streamed_line);
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some(source))
            .count(),
        1,
        "{entries:?}"
    );
}

/// The provider-neutral ingestion boundary must settle a validated batch to
/// the same styled and persisted answer with or without optional fragments.
#[test]
fn runtime_validated_say_settlement_matches_with_and_without_progress() {
    let rationale = "Distinct validated rationale without fragments";
    let later = "Later validated action without fragments";
    for (case, content_type, source) in [
        (
            "plain",
            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
            "same validated answer",
        ),
        (
            "markdown",
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
            "**same** validated answer",
        ),
        (
            "diff",
            mez_agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE,
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
        ),
    ] {
        let mut settled = Vec::new();
        for streamed_rationale in [false, true] {
            for streamed in [false, true] {
                let mut service = test_runtime_service();
                let store = AgentTranscriptStore::new(temp_root(&format!(
                    "mode-parity-{case}-{}-{}",
                    if streamed { "streamed" } else { "complete" },
                    if streamed_rationale {
                        "rationale"
                    } else {
                        "no-rationale"
                    }
                )));
                service.set_agent_transcript_store(store.clone());
                service
                    .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
                    .unwrap();
                let conversation_id = service
                    .agent_shell_store_mut()
                    .enter_or_resume("%1")
                    .unwrap()
                    .session_id
                    .clone();
                set_agent_pane_screen_for_test(
                    &mut service,
                    "%1",
                    TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
                );
                if streamed_rationale {
                    for event in [
                        mez_agent::StreamingSayEvent::RationaleStarted,
                        mez_agent::StreamingSayEvent::RationaleTextDelta {
                            text: rationale.to_string(),
                        },
                        mez_agent::StreamingSayEvent::RationaleTextComplete,
                    ] {
                        service
                            .ingest_provider_log(
                                "%1",
                                "turn-1",
                                crate::runtime::RuntimeProviderLogInput::Progress(&event),
                            )
                            .unwrap();
                    }
                }
                if streamed {
                    for event in [
                        mez_agent::StreamingSayEvent::Started {
                            action_index: 0,
                            status: mez_agent::SayStatus::Final,
                            content_type: content_type.to_string(),
                        },
                        mez_agent::StreamingSayEvent::TextDelta {
                            action_index: 0,
                            text: source.to_string(),
                        },
                        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
                        mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
                    ] {
                        service
                            .ingest_provider_log(
                                "%1",
                                "turn-1",
                                crate::runtime::RuntimeProviderLogInput::Progress(&event),
                            )
                            .unwrap();
                    }
                    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
                        service
                            .take_agent_streaming_say_projection_work("%1", "turn-1")
                            .unwrap()
                            .unwrap(),
                    )
                    .unwrap();
                    service
                        .apply_agent_streaming_say_projection_result(projection)
                        .unwrap();
                }
                let execution = mez_agent::AgentTurnExecution {
                    request: runtime_model_request_fixture("turn-1"),
                    response: mez_agent::ModelResponse {
                        provider: "runtime-batch".to_string(),
                        model: "test".to_string(),
                        raw_text: source.to_string(),
                        usage: Default::default(),
                        latest_request_usage: None,
                        quota_usage: Default::default(),
                        action_batch: Some(mez_agent::MaapBatch {
                            rationale: rationale.to_string(),
                            actions: vec![
                                mez_agent::AgentAction {
                                    id: "answer".to_string(),
                                    payload: mez_agent::AgentActionPayload::Say {
                                        status: mez_agent::SayStatus::Final,
                                        text: source.to_string(),
                                        content_type: content_type.to_string(),
                                    },
                                },
                                mez_agent::AgentAction {
                                    id: "later".to_string(),
                                    payload: mez_agent::AgentActionPayload::Say {
                                        status: mez_agent::SayStatus::Final,
                                        text: later.to_string(),
                                        content_type:
                                            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                                .to_string(),
                                    },
                                },
                            ],
                        }),
                        provider_transcript_events: Vec::new(),
                    },
                    latest_response_usage: Default::default(),
                    routing_token_usage_by_model: Default::default(),
                    action_results: Vec::new(),
                    final_turn: true,
                    terminal_state: AgentTurnState::Completed,
                };
                service
                    .ingest_provider_log(
                        "%1",
                        "turn-1",
                        crate::runtime::RuntimeProviderLogInput::Validated(&execution),
                    )
                    .unwrap();
                service
                    .ingest_provider_log(
                        "%1",
                        "turn-1",
                        crate::runtime::RuntimeProviderLogInput::Settled(&execution),
                    )
                    .unwrap();
                service
                    .ingest_provider_log(
                        "%1",
                        "turn-1",
                        crate::runtime::RuntimeProviderLogInput::Settled(&execution),
                    )
                    .unwrap();
                let rows = service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines();
                let entries = store.inspect_presentation(&conversation_id).unwrap();
                let semantic_source =
                    |entry: &crate::storage::transcript::AgentPresentationEntry| {
                        if entry.source_content_type.as_deref()
                            == Some(crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE)
                        {
                            entry.source_text.as_deref().map(|text| {
                                crate::storage::transcript::activity::ActivitySource::decode(text)
                                    .unwrap()
                                    .source
                            })
                        } else {
                            entry.source_text.clone()
                        }
                    };
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| semantic_source(entry).as_deref() == Some(rationale))
                        .count(),
                    1,
                    "{case}: {entries:?}"
                );
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| entry.source_text.as_deref() == Some(source))
                        .count(),
                    1
                );
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| entry.source_text.as_deref() == Some(later))
                        .count(),
                    1,
                    "{case}: {entries:?}"
                );
                assert_eq!(
                    entries
                        .iter()
                        .filter_map(semantic_source)
                        .filter(|text| [rationale, source, later].contains(&text.as_str()))
                        .collect::<Vec<_>>(),
                    vec![rationale, source, later],
                    "{case}: {entries:?}"
                );
                settled.push((
                    rows,
                    entries
                        .into_iter()
                        .filter(|entry| {
                            [source, rationale, later]
                                .contains(&semantic_source(entry).as_deref().unwrap_or(""))
                        })
                        .map(|entry| (entry.display_lines, entry.copy_lines))
                        .collect::<Vec<_>>(),
                ));
                // A later request in the same turn is a distinct response even
                // if it repeats the same provider-authored batch verbatim.
                let mut continuation = execution.clone();
                continuation.request.messages.push(mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "new provider request chronology".to_string(),
                });
                service
                    .ingest_provider_log(
                        "%1",
                        "turn-1",
                        crate::runtime::RuntimeProviderLogInput::Settled(&continuation),
                    )
                    .unwrap();
                let continued = store.inspect_presentation(&conversation_id).unwrap();
                assert_eq!(
                    continued
                        .iter()
                        .filter(|entry| entry.source_text.as_deref() == Some(source))
                        .count(),
                    2,
                    "{case}: {continued:?}"
                );
            }
        }
        for actual in &settled[1..] {
            assert_eq!(&settled[0], actual, "{case}");
        }
    }
}

/// Verifies interrupting a turn freezes already streamed output in the pane
/// buffer rather than restoring the screen that existed before streaming.
///
/// Provider cancellation can occur after a user-visible partial response has
/// been rendered but before an authoritative response batch is available.
/// The interruption path must retire streaming ownership so later projection
/// work cannot mutate the pane, while retaining that partial response as a
/// terminal log record followed by the stopped-turn status output.
#[test]
fn runtime_interrupted_turn_retains_partial_streamed_output_in_pane_buffer() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "stream a partial response")
        .unwrap();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: "text/plain; charset=utf-8".to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "partial streamed log".to_string(),
            },
        )
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("partial streamed output should produce projection work"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Interrupted)
        .unwrap();

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("partial streamed log"), "{pane_text}");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .is_none(),
        "interrupted output must no longer have live streaming ownership"
    );
}
