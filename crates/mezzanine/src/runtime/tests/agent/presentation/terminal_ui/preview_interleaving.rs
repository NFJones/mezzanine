//! Provider and shell-preview composition on one exactly owned screen lineage.
//!
//! Settled suffixes retire without moving durable rows; active shell owners
//! survive provider rollback, and rejected source cannot become retained output.

use super::*;

/// Verifies provider projections and shell previews share one composite lineage.
///
/// Provider updates must retain independently owned shell progress, shell
/// updates must not retire provisional provider source, and discarding the
/// provider projection must restore its durable baseline while preserving the
/// still-running shell preview.
#[test]
fn runtime_streaming_say_composes_with_active_shell_preview() {
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
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable baseline")
        .unwrap();
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provider one**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-provider", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell".to_string(),
        action_id: "shell-1".to_string(),
        marker: "marker-1".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &["shell progress one".to_string()],
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-provider",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: " and provider two".to_string(),
            },
        )
        .unwrap();
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );
    service
        .update_agent_shell_output_preview("%1", owner, 2, &["shell progress two".to_string()])
        .unwrap();

    let composite = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        composite.contains("provider one and provider two"),
        "{composite}"
    );
    assert!(composite.contains("shell progress two"), "{composite}");
    assert!(!composite.contains("shell progress one"), "{composite}");
    assert_eq!(service.agent_shell_output_previews_for_tests("%1").len(), 1);

    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some("turn-provider"))
            .unwrap()
    );
    let restored = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(restored.contains("durable baseline"), "{restored}");
    assert!(restored.contains("shell progress two"), "{restored}");
    assert!(!restored.contains("provider one"), "{restored}");
}

/// Verifies a provider update removes a settled command tail in one projection.
///
/// A completed command intentionally remains visible until the next pane
/// content is installed. Provider streaming must be that cleanup boundary, or
/// stale terminal rows survive until later output overwrites them physically.
#[test]
fn runtime_streaming_say_retires_settled_shell_preview() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(40, 4).unwrap(), 120).unwrap();
    screen.feed(b"durable zero\r\ndurable one\r\ndurable two\r\ndurable three");
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "provider one".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-provider", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell".to_string(),
        action_id: "shell-1".to_string(),
        marker: "marker-1".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "settled shell tail one".to_string(),
                "settled shell tail two".to_string(),
            ],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    let retained = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(retained.contains("settled shell tail one"), "{retained}");
    let retained_history_len = service.agent_pane_screen("%1").unwrap().history().len();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-provider",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: " and provider two".to_string(),
            },
        )
        .unwrap();
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );

    let updated_screen = service.agent_pane_screen("%1").unwrap();
    let updated = updated_screen.normal_content_lines().join("\n");
    assert!(
        updated.contains("provider one and provider two"),
        "{updated}"
    );
    assert!(!updated.contains("settled shell tail one"), "{updated}");
    assert!(!updated.contains("settled shell tail two"), "{updated}");
    assert_eq!(updated_screen.history().len(), retained_history_len);
    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
}

/// Verifies a new provider response consumes an already displayed shell window.
///
/// Unlike an update to a provider response that predates the shell preview,
/// this is the ordinary next-response handoff. Both its first projection and
/// subsequent deltas must leave durable rows at their installed coordinates.
#[test]
fn runtime_streaming_say_after_settled_tail_preserves_visible_rows() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(60, 5).unwrap(), 40).unwrap();
    screen.feed(b"durable-zero\r\ndurable-one\r\ndurable-two\r\ndurable-three\r\ndurable-four");
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell".to_string(),
        action_id: "shell".to_string(),
        marker: "marker".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "tail-one".to_string(),
                "tail-two".to_string(),
                "tail-three".to_string(),
            ],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().visible_lines(),
        vec![
            "durable-three",
            "durable-four",
            "│ tail-one",
            "│ tail-two",
            "│ tail-three"
        ]
    );
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-next",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    let mut frames = Vec::new();
    for text in ["replacement", " continued"] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-next",
                &mez_agent::StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: text.to_string(),
                },
            )
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-next")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        frames.push(service.agent_pane_screen("%1").unwrap().visible_lines());
    }
    assert_eq!(
        frames,
        vec![
            vec![
                "durable-three",
                "durable-four",
                "│ mez> replacement",
                "",
                ""
            ],
            vec![
                "durable-three",
                "durable-four",
                "│ mez> replacement continued",
                "",
                ""
            ],
        ]
    );
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-next",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "\nline two\nline three\nline four".to_string(),
            },
        )
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-next")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let before_rollback = service.agent_pane_screen("%1").unwrap().visible_lines();
    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some("turn-next"))
            .unwrap()
    );
    let after_rollback = service.agent_pane_screen("%1").unwrap().visible_lines();
    for marker in ["durable-three", "durable-four"] {
        assert_eq!(
            after_rollback.iter().position(|line| line.contains(marker)),
            before_rollback
                .iter()
                .position(|line| line.contains(marker)),
            "rollback moved {marker} downward: {before_rollback:?} -> {after_rollback:?}",
        );
    }
}

/// Verifies a full-pane provider rebase retires only settled shell ownership.
///
/// When settled and active preview owners share a full pane, consuming the
/// settled suffix must retain its viewport displacement, project the active
/// owner exactly once, and transfer that rebased baseline to later provider
/// cleanup without resurrecting or duplicating either owner.
#[test]
fn runtime_streaming_say_rebases_mixed_shell_preview_owners_in_full_pane() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(40, 4).unwrap(), 120).unwrap();
    screen.feed(b"durable zero\r\ndurable one\r\ndurable two\r\ndurable three");
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "provider one".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-provider", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );
    let settled_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell-settled".to_string(),
        action_id: "shell-settled".to_string(),
        marker: "marker-settled".to_string(),
    };
    let active_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell-active".to_string(),
        action_id: "shell-active".to_string(),
        marker: "marker-active".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            settled_owner.clone(),
            1,
            &[
                "settled shell one".to_string(),
                "settled shell two".to_string(),
            ],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            active_owner.clone(),
            1,
            &["active shell once".to_string()],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &settled_owner));
    let retained_history_len = service.agent_pane_screen("%1").unwrap().history().len();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-provider",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: " and provider two".to_string(),
            },
        )
        .unwrap();
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );

    let rebased_screen = service.agent_pane_screen("%1").unwrap();
    let rebased = rebased_screen.normal_content_lines().join("\n");
    assert_eq!(rebased_screen.history().len(), retained_history_len);
    assert!(
        rebased.contains("provider one and provider two"),
        "{rebased}"
    );
    assert!(!rebased.contains("settled shell one"), "{rebased}");
    assert!(!rebased.contains("settled shell two"), "{rebased}");
    assert_eq!(rebased.matches("active shell once").count(), 1, "{rebased}");
    assert_eq!(service.agent_shell_output_previews_for_tests("%1").len(), 1);

    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some("turn-provider"))
            .unwrap()
    );
    let restored_screen = service.agent_pane_screen("%1").unwrap();
    let restored = restored_screen.normal_content_lines().join("\n");
    assert_eq!(restored_screen.history().len(), retained_history_len);
    assert!(!restored.contains("provider one"), "{restored}");
    assert!(!restored.contains("settled shell one"), "{restored}");
    assert_eq!(
        restored.matches("active shell once").count(),
        1,
        "{restored}"
    );
    assert_eq!(
        service.agent_shell_output_previews_for_tests("%1"),
        vec![(active_owner, 1, 1, vec!["active shell once".to_string()])]
    );
}

/// Verifies streamed source is neither truncated by shell-preview settings nor
/// retained when validated completion supplies different authoritative text.
///
/// Long live output must retain its beginning and end in terminal history. A
/// later mismatch must restore the pre-stream screen so normal presentation can
/// append only the validated replacement.
#[test]
fn runtime_streaming_say_is_untruncated_and_mismatch_restores_baseline() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(32, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(32, 8).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "baseline")
        .unwrap();
    let long_source = (0..24)
        .map(|index| format!("stream-line-{index:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: long_source,
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("long cumulative source should produce projection work");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work)
        .expect("long cumulative source should render completely");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap(),
        "the complete long-source generation should install atomically"
    );
    let streamed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(streamed.contains("stream-line-00"), "{streamed}");
    assert!(streamed.contains("stream-line-23"), "{streamed}");

    let replacement = "validated replacement";
    let action = mez_agent::AgentAction {
        id: "say-replacement".to_string(),

        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: replacement.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: replacement.to_string(),
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
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let final_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(final_text.contains("baseline"), "{final_text}");
    assert!(final_text.contains(replacement), "{final_text}");
    assert!(!final_text.contains("stream-line-00"), "{final_text}");
    assert_eq!(final_text.matches(replacement).count(), 1, "{final_text}");
}
