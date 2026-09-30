//! Accepted command projections with independently owned progress and headers.
//!
//! These regressions preserve action-order publication and prevent accepted
//! siblings from being erased or duplicated when shell work is admitted.

use super::*;

/// Verifies accepting two exact command sources does not blank either already
/// visible preview while later dispatch remains separately authorized.
#[tokio::test]
async fn runtime_streaming_multiple_commands_keep_matching_previews() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-multiple-commands"));
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    mark_test_pane_ready(&mut service, "%1");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "inspect two commands")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let actions = ["printf first", "printf second"]
        .into_iter()
        .enumerate()
        .map(|(index, command)| mez_agent::AgentAction {
            id: format!("command-{index}"),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: format!("summary {index}"),
                command: command.to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        })
        .collect::<Vec<_>>();
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Inspect both commands".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    for (index, command) in ["printf first", "printf second"].into_iter().enumerate() {
        for event in [
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted {
                action_index: index,
            },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                action_index: index,
                text: format!("summary {index}"),
            },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete {
                action_index: index,
            },
            mez_agent::StreamingSayEvent::ShellCommandStarted {
                action_index: index,
            },
            mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                action_index: index,
                text: command.to_string(),
            },
            mez_agent::StreamingSayEvent::ShellCommandTextComplete {
                action_index: index,
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(visible.iter().any(|line| line.contains("printf first")));
    assert!(visible.iter().any(|line| line.contains("printf second")));
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Inspect both commands".to_string(),
                actions: actions.clone(),
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: actions
            .iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
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
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    for command in ["printf first", "printf second"] {
        assert_eq!(
            rows.iter().filter(|row| row.contains(command)).count(),
            1,
            "{rows:?}"
        );
        assert_eq!(
            store
                .inspect_presentation(&conversation_id)
                .unwrap()
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(command))
                .count(),
            1,
        );
    }
    assert!(visible.iter().any(|row| row.contains("printf first")));
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    let sources = entries
        .iter()
        .filter_map(|entry| entry.source_text.as_deref())
        .filter(|source| {
            matches!(
                *source,
                "summary 0" | "printf first" | "summary 1" | "printf second"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        ["summary 0", "printf first", "summary 1", "printf second"]
    );
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let locations = ["summary 0", "printf first", "summary 1", "printf second"].map(|fragment| {
        replayed
            .iter()
            .position(|row| row.contains(fragment))
            .unwrap()
    });
    assert!(
        locations.windows(2).all(|pair| pair[0] < pair[1]),
        "{replayed:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a matching progress row does not disappear when its sibling shell
/// command is admitted; neither row may become a second final presentation.
#[tokio::test]
async fn runtime_streaming_progress_and_shell_keep_matching_rows() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-progress-shell"));
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    mark_test_pane_ready(&mut service, "%1");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the shell")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let say = mez_agent::AgentAction {
        id: "progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "checking source".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let shell = mez_agent::AgentAction {
        id: "shell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: String::new(),
            command: "printf shell".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let search = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "mixed search".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Inspect the shell".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "checking source".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 1 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 1,
            text: "printf shell".to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::ActionHeader {
            action_index: 2,
            header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                query: "mixed search".to_string(),
            }),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(visible.iter().any(|row| row.contains("checking source")));
    assert!(visible.iter().any(|row| row.contains("printf shell")));
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Inspect the shell".to_string(),
                actions: vec![say.clone(), shell.clone(), search.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult::succeeded(
                &turn,
                &say,
                vec!["checking source".to_string()],
                None,
            ),
            mez_agent::ActionResult::running(&turn, &shell, Vec::new(), None),
            mez_agent::ActionResult::running(&turn, &search, Vec::new(), None),
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
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
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("checking source"))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("printf shell"))
            .count(),
        1
    );
    assert!(visible.iter().any(|row| row.contains("checking source")));
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("web search: mixed search"))
            .count(),
        1
    );
    assert_eq!(
        service
            .runtime_metrics()
            .agent_streaming_settlement_restorations,
        0
    );
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    for source in [
        "checking source",
        "printf shell",
        "web search: mixed search",
    ] {
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(source))
                .count(),
            1,
            "{entries:?}"
        );
    }
    service.terminate_all_pane_processes().unwrap();
}
