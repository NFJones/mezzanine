//! Deterministic occurrence evidence independent of persistence and geometry.

use super::*;

/// A typed fallback envelope followed by an ordinary accepted source must
/// retain the outer replay suppression guard. Repeated reconstruction at new
/// geometry cannot grow the durable log or assign chrome an entry stamp.
#[test]
fn runtime_occurrence_nested_fallback_replay_keeps_log_and_chrome_unchanged() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("occurrence-nested-fallback"));
    service.set_agent_transcript_store(store.clone());
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let transcript = mez_agent::transcript::TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: 1,
        created_at_unix_seconds: 2,
        role: mez_agent::transcript::TranscriptRole::User,
        turn_id: "turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "error: original record".into(),
    };
    store.append(&transcript).unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    service
        .replay_agent_transcript_fallback_to_terminal_buffer(
            "%1",
            RuntimeSessionService::runtime_resume_transcript_presentation(
                &conversation,
                1,
                &[transcript],
            ),
            true,
        )
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "later accepted source")
        .unwrap();
    let entries = store.inspect_presentation(&conversation).unwrap();
    assert_eq!(entries.len(), 2);
    let next = store.next_presentation_sequence(&conversation).unwrap();
    for width in [30, 48, 24] {
        service.presentation.occurrence_clock_millis = Some(Some(90_000));
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(width, 12).unwrap(), 120).unwrap(),
        );
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap();
        assert_eq!(store.inspect_presentation(&conversation).unwrap(), entries);
        assert_eq!(
            store.next_presentation_sequence(&conversation).unwrap(),
            next
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        assert!(
            rows.iter()
                .any(|row| row.text.contains("Resumed Agent Session"))
        );
        assert!(
            rows.iter()
                .filter(|row| row.text.contains("Resumed Agent Session"))
                .all(|row| row.presentation.is_none())
        );
        assert!(rows.iter().filter_map(|row| row.presentation.as_ref()).any(
            |entry| entry.first_row
                && entry.id == format!("transcript:{conversation}:1")
                && entry.occurrence
                    == mez_core::presentation::PresentationOccurrence::legacy_record(2)
        ));
    }
}

/// A transcript-only fork made before any resume must preserve original coarse
/// record provenance, not its new catalog creation time. A second fork and two
/// replay widths must retain source identity/time, chrome must stay untagged,
/// and authored transcript bytes must remain exact in the new conversation.
#[test]
fn runtime_occurrence_transcript_only_fork_keeps_original_record_evidence() {
    let store = AgentTranscriptStore::new(temp_root("occurrence-transcript-fork"));
    let entry = mez_agent::transcript::TranscriptEntry {
        conversation_id: "original".into(),
        sequence: 1,
        created_at_unix_seconds: 2,
        role: mez_agent::transcript::TranscriptRole::User,
        turn_id: "turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "exact authored\r\n雪 original record".into(),
    };
    store.append(&entry).unwrap();
    assert!(store.inspect_presentation("original").unwrap().is_empty());
    store.fork("original", "forked", 99).unwrap();
    store.fork("forked", "second", 199).unwrap();
    for (conversation, catalog_time) in [("forked", 99), ("second", 199)] {
        let transcript = store.inspect(conversation).unwrap();
        assert_eq!(transcript[0].content, entry.content);
        assert_eq!(transcript[0].created_at_unix_seconds, catalog_time);
        assert_eq!(
            store
                .saved_session(conversation)
                .unwrap()
                .unwrap()
                .summary
                .first_created_at_unix_seconds,
            catalog_time
        );
        for width in [30, 48] {
            let mut service = test_runtime_service();
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            service
                .agent_shell_store_mut()
                .bind_conversation_with_lineage("%1", conversation, 1, None)
                .unwrap();
            service.set_agent_transcript_store(store.clone());
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(width, 12).unwrap(), 120).unwrap(),
            );
            service.presentation.occurrence_clock_millis = Some(Some(90_000));
            let presentation = store.inspect_presentation(conversation).unwrap();
            if !service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &presentation)
                .unwrap()
            {
                service
                    .replay_agent_transcript_fallback_to_terminal_buffer(
                        "%1",
                        RuntimeSessionService::runtime_resume_transcript_presentation(
                            conversation,
                            1,
                            &transcript,
                        ),
                        false,
                    )
                    .unwrap();
            }
            let rows = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_styled_content_lines();
            let boundaries = rows
                .iter()
                .filter_map(|row| row.presentation.as_ref())
                .filter(|entry| entry.first_row)
                .collect::<Vec<_>>();
            assert_eq!(boundaries.len(), 1);
            assert_eq!(boundaries[0].id, "transcript:original:1");
            assert_eq!(
                boundaries[0].occurrence,
                mez_core::presentation::PresentationOccurrence::legacy_record(2)
            );
            assert!(
                rows.iter()
                    .filter(|row| row.text.contains("Resumed Agent Session"))
                    .all(|row| row.presentation.is_none())
            );
            let mut legacy = test_runtime_service();
            legacy
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            set_agent_pane_screen_for_test(
                &mut legacy,
                "%1",
                TerminalScreen::new(Size::new(width, 12).unwrap(), 120).unwrap(),
            );
            legacy
                .set_agent_prompt_display_lines(
                    "%1",
                    RuntimeSessionService::runtime_resume_transcript_presentation(
                        conversation,
                        1,
                        std::slice::from_ref(&entry),
                    )
                    .into_iter()
                    .map(|(line, _)| line)
                    .collect(),
                )
                .unwrap();
            assert_eq!(
                agent_visual_rows(rows),
                agent_visual_rows(
                    legacy
                        .agent_pane_screen("%1")
                        .unwrap()
                        .normal_styled_content_lines()
                )
            );
        }
    }
}

/// Re-rendering for unstreamed rationale or a missing worker projection changes
/// only layout ownership, not a validated component's first observation. Exact
/// closed source keeps its ID/basis/time; different accepted source must not
/// borrow that evidence merely because it reused the response-local ordinal.
fn occurrence_static_fallback_cases(cases: &[(bool, bool, bool, bool, bool, bool)]) {
    for &(projected, rationale, matches, complete, intervening, resize) in cases {
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("occurrence-static-fallback"));
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
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        service.presentation.occurrence_clock_millis = Some(Some(1234));
        let mut events = vec![
            mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: if complete {
                    "original streamed answer"
                } else {
                    "original streamed"
                }
                .into(),
            },
        ];
        if complete {
            events.extend([
                mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
                mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
            ]);
        }
        for event in events {
            service
                .ingest_provider_log(
                    "%1",
                    "turn-1",
                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                )
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection =
            RuntimeSessionService::build_agent_streaming_say_projection(work.clone()).unwrap();
        let observed = projection
            .screen
            .normal_styled_content_lines()
            .into_iter()
            .find(|row| row.text.contains("original streamed"))
            .unwrap()
            .presentation
            .unwrap();
        if projected {
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap();
        }
        if intervening {
            service.presentation.occurrence_clock_millis = Some(Some(2000));
            service
                .append_agent_status_text_to_terminal_buffer("%1", "intervening status")
                .unwrap();
            let delayed =
                RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
            assert!(
                !service
                    .apply_agent_streaming_say_projection_result(delayed)
                    .unwrap()
            );
        }
        if resize {
            assert!(
                service
                    .rebuild_agent_presentation_after_resize("%1", Size::new(30, 12).unwrap())
                    .unwrap()
            );
            assert!(
                service
                    .take_agent_streaming_say_projection_work("%1", "turn-1")
                    .unwrap()
                    .is_none()
            );
        }
        service.presentation.occurrence_clock_millis = Some(Some(9000));
        let accepted = if matches {
            "original streamed answer"
        } else {
            "different accepted answer"
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture("turn-1"),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: accepted.into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: if rationale {
                        "Unstreamed ordering rationale".into()
                    } else {
                        String::new()
                    },
                    actions: vec![mez_agent::AgentAction {
                        id: "answer".into(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: accepted.into(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
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
        if intervening {
            assert!(
                service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .iter()
                    .any(|row| row.contains("intervening status"))
            );
        }
        let live_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let durable = store
            .inspect_presentation(&conversation)
            .unwrap()
            .into_iter()
            .find(|row| presentation_semantic_source(row).as_deref() == Some(accepted))
            .unwrap()
            .presentation
            .unwrap();
        // Physical wrapping cannot locate a whole authored string on one row.
        // The exact durable source above identifies the accepted entry, whose
        // live boundary must exist exactly once with identical provenance.
        let boundaries = live_rows
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.id == durable.id && entry.first_row)
            .collect::<Vec<_>>();
        assert_eq!(boundaries.len(), 1, "{live_rows:?}");
        let live = boundaries[0].clone();
        assert_eq!(live, durable);
        if matches {
            assert_eq!(live, observed);
        } else {
            assert_ne!(live.id, observed.id);
            assert_eq!(live.occurrence.unix_millis, Some(9000));
            assert_eq!(
                live.occurrence.basis,
                mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance
            );
        }
    }
}

/// A structural response owner may be retired before its first component. First
/// source receipt must still capture metadata but cannot install the owner's old
/// screen or erase the exact text/style/copy/status rows of an intervening write.
fn first_component_retirement_case(command: bool) {
    use mez_agent::StreamingSayEvent as Event;
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    service
        .ingest_provider_log(
            "%1",
            "turn-1",
            crate::runtime::RuntimeProviderLogInput::Progress(&Event::ResponseStarted {
                response_index: 0,
            }),
        )
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(2000));
    service
        .append_agent_status_text_to_terminal_buffer("%1", "intervening exact status")
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    service.presentation.occurrence_clock_millis = Some(Some(1234));
    let source = if command { "printf later" } else { "later say" };
    let events = if command {
        vec![
            Event::ShellCommandStarted { action_index: 0 },
            Event::ShellCommandTextDelta {
                action_index: 0,
                text: source.into(),
            },
            Event::ShellCommandTextComplete { action_index: 0 },
            Event::ActionComplete { action_index: 0 },
        ]
    } else {
        vec![
            Event::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
            Event::TextDelta {
                action_index: 0,
                text: source.into(),
            },
            Event::TextComplete { action_index: 0 },
            Event::ActionComplete { action_index: 0 },
        ]
    };
    for event in events {
        service
            .ingest_provider_log(
                "%1",
                "turn-1",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
        assert_eq!(
            service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_styled_content_lines(),
            before,
            "event={event:?}"
        );
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-1")
                .unwrap()
                .is_none()
        );
    }
    service.presentation.occurrence_clock_millis = Some(Some(9000));
    let payload = if command {
        mez_agent::AgentActionPayload::ShellCommand {
            command: source.into(),
            summary: "intent".into(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        }
    } else {
        mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: source.into(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
        }
    };
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
                    id: "later".into(),
                    payload,
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
    service
        .ingest_provider_log(
            "%1",
            "turn-1",
            crate::runtime::RuntimeProviderLogInput::Validated(&execution),
        )
        .unwrap();
    let entry = service
        .retained_provider_component_entry(
            "%1",
            &execution,
            Some(0),
            if command { "command" } else { "say" },
        )
        .unwrap()
        .unwrap();
    assert_eq!(entry.occurrence.unix_millis, Some(1234));
    assert_eq!(
        entry.occurrence.basis,
        mez_core::presentation::PresentationOccurrenceBasis::LocalObservation
    );
    service
        .ingest_provider_log(
            "%1",
            "turn-1",
            crate::runtime::RuntimeProviderLogInput::Settled(&execution),
        )
        .unwrap();
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let original_status = before
        .iter()
        .find(|row| row.text.contains("intervening exact status"))
        .unwrap();
    assert_eq!(
        after
            .iter()
            .find(|row| row.text.contains("intervening exact status"))
            .unwrap(),
        original_status
    );
}

/// First say receipt after structural-owner retirement cannot restore its old
/// screen. Exact accepted source retains its original first-observation stamp.
#[test]
fn runtime_occurrence_retired_owner_first_say_preserves_intervening_rows() {
    first_component_retirement_case(false);
}

/// First command-intent receipt after structural-owner retirement remains
/// inert until accepted source; it is never proof that the command executed.
#[test]
fn runtime_occurrence_retired_owner_first_command_preserves_intervening_rows() {
    first_component_retirement_case(true);
}

/// Every semantic source family transfers only its original observation under
/// the exact typed component contract. Closed unequal text and incomplete
/// non-prefixes cannot acquire that evidence; partial matching text does not
/// authorize partial-row promotion or any executor/message effect.
#[test]
fn runtime_occurrence_component_fallback_retains_only_proven_original_sources() {
    use mez_agent::StreamingSayEvent as Event;
    for component in [
        "rationale",
        "say",
        "command",
        "summary",
        "message",
        "header",
    ] {
        for complete in [false, true] {
            if component == "header" && !complete {
                continue;
            }
            for matches in [false, true] {
                let mut service = test_runtime_service();
                service
                    .agent_shell_store_mut()
                    .enter_or_resume("%1")
                    .unwrap();
                set_agent_pane_screen_for_test(
                    &mut service,
                    "%1",
                    TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
                );
                service.presentation.occurrence_clock_millis = Some(Some(1234));
                let observed = if complete {
                    "authored source extended"
                } else {
                    "authored source"
                };
                let accepted = if matches {
                    "authored source extended"
                } else {
                    "different validated source"
                };
                let mut events = match component {
                    "rationale" => vec![
                        Event::RationaleStarted,
                        Event::RationaleTextDelta {
                            text: observed.into(),
                        },
                    ],
                    "say" => vec![
                        Event::Started {
                            action_index: 0,
                            status: mez_agent::SayStatus::Final,
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                        },
                        Event::TextDelta {
                            action_index: 0,
                            text: observed.into(),
                        },
                    ],
                    "command" => vec![
                        Event::ShellCommandStarted { action_index: 0 },
                        Event::ShellCommandTextDelta {
                            action_index: 0,
                            text: observed.into(),
                        },
                    ],
                    "summary" => vec![
                        Event::ShellCommandSummaryStarted { action_index: 0 },
                        Event::ShellCommandSummaryTextDelta {
                            action_index: 0,
                            text: observed.into(),
                        },
                    ],
                    "message" => vec![
                        Event::MessageStarted {
                            action_index: 0,
                            recipient: "agent-%2".into(),
                            content_type: "text/plain; charset=utf-8".into(),
                        },
                        Event::MessagePayloadDelta {
                            action_index: 0,
                            text: observed.into(),
                        },
                    ],
                    "header" => vec![Event::ActionHeader {
                        action_index: 0,
                        header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                            query: observed.into(),
                        }),
                    }],
                    _ => unreachable!(),
                };
                if complete {
                    match component {
                        "rationale" => events.push(Event::RationaleTextComplete),
                        "say" => events.push(Event::TextComplete { action_index: 0 }),
                        "command" => {
                            events.push(Event::ShellCommandTextComplete { action_index: 0 })
                        }
                        "summary" => {
                            events.push(Event::ShellCommandSummaryTextComplete { action_index: 0 })
                        }
                        "message" => events.push(Event::MessagePayloadComplete { action_index: 0 }),
                        _ => {}
                    }
                }
                for event in events {
                    service
                        .ingest_provider_log(
                            "%1",
                            "turn-1",
                            crate::runtime::RuntimeProviderLogInput::Progress(&event),
                        )
                        .unwrap();
                }
                let work = service
                    .take_agent_streaming_say_projection_work("%1", "turn-1")
                    .unwrap()
                    .unwrap();
                let projection =
                    RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
                let original = projection
                    .screen
                    .normal_styled_content_lines()
                    .into_iter()
                    .filter_map(|row| row.presentation)
                    .find(|entry| entry.first_row)
                    .unwrap();
                service.presentation.occurrence_clock_millis = Some(Some(9000));
                let payload = match component {
                    "rationale" | "say" => mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: accepted.into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                    },
                    "command" | "summary" => mez_agent::AgentActionPayload::ShellCommand {
                        command: accepted.into(),
                        summary: accepted.into(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                    "message" => mez_agent::AgentActionPayload::SendMessage {
                        expect_reply: false,
                        recipient: "agent-%2".into(),
                        scope: None,
                        content_type: "text/plain; charset=utf-8".into(),
                        payload: accepted.into(),
                        correlation_id: None,
                    },
                    "header" => mez_agent::AgentActionPayload::WebSearch {
                        query: accepted.into(),
                        domains: Vec::new(),
                        recency_days: None,
                        max_results: None,
                    },
                    _ => unreachable!(),
                };
                let execution = mez_agent::AgentTurnExecution {
                    request: runtime_model_request_fixture("turn-1"),
                    response: mez_agent::ModelResponse {
                        provider: "runtime-batch".into(),
                        model: "test".into(),
                        raw_text: accepted.into(),
                        usage: Default::default(),
                        latest_request_usage: None,
                        quota_usage: Default::default(),
                        action_batch: Some(mez_agent::MaapBatch {
                            rationale: if component == "rationale" {
                                accepted.into()
                            } else {
                                String::new()
                            },
                            actions: vec![mez_agent::AgentAction {
                                id: "component".into(),
                                payload,
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
                service
                    .ingest_provider_log(
                        "%1",
                        "turn-1",
                        crate::runtime::RuntimeProviderLogInput::Validated(&execution),
                    )
                    .unwrap();
                let retained = service
                    .retained_provider_component_entry(
                        "%1",
                        &execution,
                        if component == "rationale" {
                            None
                        } else {
                            Some(0)
                        },
                        component,
                    )
                    .unwrap();
                assert_eq!(
                    retained,
                    matches.then_some(original),
                    "component={component} complete={complete} matches={matches}"
                );
            }
        }
    }
}

/// Exact closed source retains observation metadata through layout fallback,
/// while closed unequal source cannot borrow that provisional evidence.
#[test]
fn runtime_occurrence_static_fallback_keeps_only_matching_stream_evidence() {
    occurrence_static_fallback_cases(&[
        (true, true, true, true, false, false),
        (false, false, true, true, false, false),
        (true, true, false, true, false, false),
    ]);
}

/// Optional source fragments may end before field closure. An exact accepted
/// continuation must retain first observation without promoting partial rows.
#[test]
fn runtime_occurrence_partial_fallback_keeps_original_observation() {
    occurrence_static_fallback_cases(&[(false, false, true, false, false, false)]);
}

/// Ordinary status output retires only layout authority. Exact later acceptance
/// retains original source evidence and cannot erase status or admit stale work.
#[test]
fn runtime_occurrence_intervening_write_fallback_keeps_original_observation() {
    occurrence_static_fallback_cases(&[(true, true, true, true, true, false)]);
}

/// Retired source survives source-backed resize without re-projecting rows.
/// Acceptance keeps the old observation while durable status/copy remains.
#[test]
fn runtime_occurrence_retired_layout_resize_keeps_only_inert_source() {
    occurrence_static_fallback_cases(&[(true, true, true, true, true, true)]);
}

/// A fork of already projected fallback rows preserves its frozen display/copy
/// source, including historical chrome, while keeping original record stamps.
/// Only transcript-only record-form source synthesizes new resume chrome.
#[test]
fn runtime_occurrence_projected_fallback_fork_preserves_frozen_source() {
    let mut source = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("occurrence-projected-fork"));
    source.set_agent_transcript_store(store.clone());
    let conversation = source
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let entry = mez_agent::transcript::TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: 1,
        created_at_unix_seconds: 2,
        role: mez_agent::transcript::TranscriptRole::User,
        turn_id: "turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "frozen original record".into(),
    };
    store.append(&entry).unwrap();
    set_agent_pane_screen_for_test(
        &mut source,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    source
        .replay_agent_transcript_fallback_to_terminal_buffer(
            "%1",
            RuntimeSessionService::runtime_resume_transcript_presentation(
                &conversation,
                1,
                &[entry],
            ),
            true,
        )
        .unwrap();
    let original = source
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    store.fork(&conversation, "forked", 99).unwrap();
    let entries = store.inspect_presentation("forked").unwrap();
    let mut target = test_runtime_service();
    target
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    target
        .agent_shell_store_mut()
        .bind_conversation_with_lineage("%1", "forked", 1, None)
        .unwrap();
    target.presentation.occurrence_clock_millis = Some(Some(9000));
    set_agent_pane_screen_for_test(
        &mut target,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    target
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
        .unwrap();
    assert_eq!(
        target
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        original
    );
}

/// Typed transcript fallback must retain the legacy batch's visible rows,
/// styles, wraps and exact copy sources while tagging each proven record
/// separately. A blank chrome row must not acquire a gutter through a separate
/// append, and a later clock must not replace original coarse record evidence.
#[test]
fn runtime_occurrence_transcript_fallback_preserves_legacy_batch_framing() {
    let mut entries = [
        mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "authored first\nsecond 雪 row with wrapping".into(),
        },
        mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".into(),
            sequence: 2,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "second record".into(),
        },
    ];
    for (persist, error) in [(false, false), (true, false), (false, true), (true, true)] {
        entries[1].content = if error {
            "error: second record"
        } else {
            "second record"
        }
        .into();
        let mut legacy = test_runtime_service();
        let mut typed = test_runtime_service();
        for service in [&mut legacy, &mut typed] {
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            set_agent_pane_screen_for_test(
                service,
                "%1",
                TerminalScreen::new(Size::new(30, 12).unwrap(), 120).unwrap(),
            );
            service.presentation.occurrence_clock_millis = Some(Some(90_000));
        }
        let lines =
            RuntimeSessionService::runtime_resume_transcript_presentation("saved", 2, &entries);
        legacy
            .set_agent_prompt_display_lines(
                "%1",
                lines.iter().map(|(line, _)| line.clone()).collect(),
            )
            .unwrap();
        typed
            .replay_agent_transcript_fallback_to_terminal_buffer("%1", lines, persist)
            .unwrap();
        let legacy_rows = legacy
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let typed_rows = typed
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        assert_eq!(
            agent_visual_rows(typed_rows.clone()),
            agent_visual_rows(legacy_rows)
        );
        let first_rows = typed_rows
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .collect::<Vec<_>>();
        assert_eq!(first_rows.len(), 2);
        assert_eq!(first_rows[0].id, "transcript:saved:1");
        assert_eq!(first_rows[1].id, "transcript:saved:2");
        assert_eq!(
            first_rows[0].occurrence,
            mez_core::presentation::PresentationOccurrence::legacy_record(1)
        );
        assert_eq!(
            first_rows[1].occurrence,
            mez_core::presentation::PresentationOccurrence::legacy_record(2)
        );
        assert!(
            typed_rows
                .iter()
                .filter(|row| row.text.contains("Resumed Agent Session"))
                .all(|row| row.presentation.is_none())
        );
    }
}

/// Structured shell preview starts require positive producer proof. Growing a
/// wrapped source into a clipped tail keeps its stamp/id but loses the original
/// first row; resize and settlement cannot promote the retained tail to first.
#[test]
fn runtime_occurrence_shell_preview_growth_keeps_stamp_and_crop_proof() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(30, 12).unwrap(), 120).unwrap(),
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn".into(),
        action_id: "action".into(),
        marker: "marker".into(),
    };
    service.presentation.occurrence_clock_millis = Some(Some(1234));
    service
        .update_agent_shell_output_preview_with_boundary(
            "%1",
            owner.clone(),
            1,
            &["first output".into()],
            true,
        )
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let entry = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .find(|entry| entry.first_row)
        .unwrap()
        .clone();
    service.presentation.occurrence_clock_millis = Some(Some(9000));
    service
        .update_agent_shell_output_preview_with_boundary(
            "%1",
            owner.clone(),
            2,
            &["x".repeat(20_000)],
            true,
        )
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let matched = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .filter(|row| row.id == entry.id)
        .collect::<Vec<_>>();
    assert!(!matched.is_empty());
    assert!(
        matched
            .iter()
            .all(|row| !row.first_row && row.occurrence == entry.occurrence)
    );
    service
        .resize_agent_screen_with_pending_steering("%1", Size::new(24, 12).unwrap())
        .unwrap();
    service.settle_agent_shell_output_preview("%1", &owner);
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let matched = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .filter(|row| row.id == entry.id)
        .collect::<Vec<_>>();
    assert!(!matched.is_empty());
    assert!(
        matched
            .iter()
            .all(|row| !row.first_row && row.occurrence == entry.occurrence)
    );
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(30, 12).unwrap(), 120).unwrap(),
    );
    service
        .update_agent_shell_output_preview("%1", owner, 1, &["unknown tail beginning".into()])
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let annotations = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .collect::<Vec<_>>();
    assert!(!annotations.is_empty());
    assert!(annotations.iter().all(|entry| !entry.first_row));
}

/// Reused response-local indices and equal payloads are independent occurrences.
/// Duplicate starts and stale projection callbacks cannot replace an established
/// stamp, including equal/backward clocks across successive exact responses.
#[test]
fn runtime_occurrence_reused_response_indices_keep_original_observation() {
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
    let mut captured = Vec::new();
    let mut stale_projection = None;
    for (response_index, time) in [(1, 1000), (2, 1000), (3, -500)] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn",
                &mez_agent::StreamingSayEvent::ResponseStarted { response_index },
            )
            .unwrap();
        if let Some(stale) = stale_projection.take() {
            assert!(
                !service
                    .apply_agent_streaming_say_projection_result(stale)
                    .unwrap()
            );
        }
        service.presentation.occurrence_clock_millis = Some(Some(time));
        let started = mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
        };
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn", &started)
            .unwrap();
        service.presentation.occurrence_clock_millis = Some(Some(9000));
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn", &started)
            .unwrap();
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn",
                &mez_agent::StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: "same response".into(),
                },
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn")
            .unwrap()
            .unwrap();
        stale_projection = Some(
            RuntimeSessionService::build_agent_streaming_say_projection(work.clone()).unwrap(),
        );
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        let entry = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines()
            .into_iter()
            .find(|row| row.text.contains("same response"))
            .unwrap()
            .presentation
            .unwrap();
        assert_eq!(entry.occurrence.unix_millis, Some(time));
        captured.push(entry);
    }
    assert_eq!(
        captured
            .iter()
            .map(|entry| &entry.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
}

/// An accepted sender echo reuses its original local stream observation rather
/// than acceptance/persistence time, without changing the exact MMP payload.
#[test]
fn runtime_occurrence_outbound_stream_keeps_original_observation() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("occurrence-outbound"));
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
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    service.presentation.occurrence_clock_millis = Some(Some(1234));
    for event in [
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 0,
            recipient: "agent-%2".into(),
            content_type: "text/plain; charset=utf-8".into(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 0,
            text: "exact payload\r\n雪".into(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn", &event)
            .unwrap();
    }
    service.presentation.occurrence_clock_millis = Some(Some(9000));
    let action = mez_agent::AgentAction {
        id: "message".into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            expect_reply: false,
            recipient: "agent-%2".into(),
            scope: None,
            content_type: "text/plain; charset=utf-8".into(),
            payload: "exact payload\r\n雪".into(),
            correlation_id: None,
        },
    };
    service
        .settle_accepted_outbound_message_preview("%1", "turn", 0, &action)
        .unwrap();
    let entries = store.inspect_presentation(&conversation).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]
            .presentation
            .as_ref()
            .unwrap()
            .occurrence
            .unix_millis,
        Some(1234)
    );
    let source: serde_json::Value =
        serde_json::from_str(entries[0].source_text.as_deref().unwrap()).unwrap();
    assert_eq!(source["payload"], "exact payload\r\n雪");
}

/// Equal input and equal/backward clocks remain independent accepted entries.
/// Ephemeral/no-store panes retain provenance even while display is hidden;
/// row wrapping and redraw cannot mint timestamps or modify copy/source bytes.
#[test]
fn runtime_occurrence_memory_only_equal_and_backward_clocks() {
    for ephemeral in [false, true] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        if ephemeral {
            service
                .agent_shell_store_mut()
                .bind_ephemeral_conversation_with_lineage("%1", "ephemeral-occurrence", 0, None)
                .unwrap();
        }
        service.agent_shell_store_mut().request_exit("%1").unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(24, 10).unwrap(), 120).unwrap(),
        );
        for time in [1000, 1000, -500] {
            service.presentation.occurrence_clock_millis = Some(Some(time));
            service
                .append_agent_user_prompt_to_terminal_buffer("%1", "same\nsecond authored row")
                .unwrap();
        }
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let boundaries = rows
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            boundaries
                .iter()
                .map(|entry| entry.occurrence.unix_millis)
                .collect::<Vec<_>>(),
            vec![Some(1000), Some(1000), Some(-500)]
        );
        assert_eq!(
            boundaries
                .iter()
                .map(|entry| &entry.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        service.presentation.occurrence_clock_millis = Some(Some(90_000));
        service
            .agent_pane_screen_mut("%1")
            .unwrap()
            .resize(Size::new(20, 10).unwrap())
            .unwrap();
        let resized = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let after = resized
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .filter(|entry| entry.first_row)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(after, boundaries);
    }
}

/// A semantic durable entry keeps its original observed timestamp through
/// source reconstruction at a different width. Unknown legacy source stays
/// unknown, opaque ANSI rows stay untagged, and replay never samples now.
#[test]
fn runtime_occurrence_durable_source_replay_keeps_stamp_and_unknown() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("occurrence-replay"));
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
        TerminalScreen::new(Size::new(30, 10).unwrap(), 120).unwrap(),
    );
    service.presentation.occurrence_clock_millis = Some(Some(1234));
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "**authored**\n\n|A|B|\n|-|-|\n|1|2|",
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();
    let entries = store.inspect_presentation(&conversation).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]
            .presentation
            .as_ref()
            .unwrap()
            .occurrence
            .unix_millis,
        Some(1234)
    );
    service.presentation.occurrence_clock_millis = Some(Some(9999));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 10).unwrap(), 120).unwrap(),
    );
    service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let stamped = rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .collect::<Vec<_>>();
    assert!(!stamped.is_empty());
    assert!(
        stamped
            .iter()
            .all(|entry| entry == &entries[0].presentation.as_ref().unwrap()
                || (!entry.first_row
                    && entry.id == entries[0].presentation.as_ref().unwrap().id
                    && entry.occurrence == entries[0].presentation.as_ref().unwrap().occurrence))
    );
    assert_eq!(store.inspect_presentation(&conversation).unwrap(), entries);
    let mut unknown = entries[0].clone();
    unknown.presentation = None;
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 10).unwrap(), 120).unwrap(),
    );
    service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &[unknown.clone()])
        .unwrap();
    let unknown_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let unknown_entries = unknown_rows
        .iter()
        .filter_map(|row| row.presentation.as_ref())
        .collect::<Vec<_>>();
    assert!(!unknown_entries.is_empty());
    assert!(
        unknown_entries
            .iter()
            .all(|entry| entry.occurrence
                == mez_core::presentation::PresentationOccurrence::default())
    );
    unknown.source_text = None;
    unknown.source_content_type = None;
    unknown.ansi_text = Some("opaque ANSI\r\n".into());
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 10).unwrap(), 120).unwrap(),
    );
    service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &[unknown])
        .unwrap();
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .iter()
            .any(|row| row.contains("opaque ANSI"))
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines()
            .iter()
            .all(|row| row.presentation.is_none())
    );
}
