//! Runtime tests for agent presentation logging behavior.

use super::*;

/// A validated progress say after a runtime-owned action waits for its log.
/// The action header is emitted by its executor, not the batch presenter.
#[test]
fn runtime_mixed_action_progress_waits_for_preceding_header() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let action = mez_agent::AgentAction {
        id: "discovery".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-mixed-log"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![
                    action.clone(),
                    mez_agent::AgentAction {
                        id: "progress".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: "later progress".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
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
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!before.contains("later progress"), "{before}");
    service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    let mut settled = execution.clone();
    settled.action_results.push(mez_agent::ActionResult {
        protocol: "maap/1".to_string(),
        turn_id: "turn-mixed-log".to_string(),
        agent_id: "agent-%1".to_string(),
        action_id: action.id.clone(),
        action_type: "list_agents",
        status: ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    });
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
        .unwrap();
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(after.contains("list agents:"), "{after}");
    assert!(after.contains("later progress"), "{after}");
    assert!(
        after.find("list agents:").unwrap() < after.find("later progress").unwrap(),
        "{after}"
    );
    assert_eq!(after.matches("later progress").count(), 1, "{after}");
    settled.terminal_state = AgentTurnState::Completed;
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
        .unwrap();
    let after_completion = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        after_completion.matches("later progress").count(),
        1,
        "{after_completion}"
    );
}

/// A leading validated progress say releases the next action ordinal without
/// requiring a streamed promotion or a preceding runtime action.
#[test]
fn runtime_static_progress_releases_following_discovery_header() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover after progress")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let progress = mez_agent::AgentAction {
        id: "first-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "first progress".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "second-discovery".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![progress.clone(), discovery.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&progress, &discovery]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        text.find("first progress").unwrap() < text.find("list agents:").unwrap(),
        "{text}"
    );
    assert_eq!(text.matches("first progress").count(), 1, "{text}");
}

/// Executor traversal must not move a later discovery header past an
/// interleaved progress say whose predecessor has just settled.
#[test]
fn runtime_discovery_headers_preserve_interleaved_progress_order() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover peers")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let discover = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let first = discover("first-discovery");
    let last = discover("last-discovery");
    let progress = mez_agent::AgentAction {
        id: "middle-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "middle response".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![first.clone(), progress.clone(), last.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&first, &progress, &last]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let first_row = lines.find("list agents:").unwrap();
    let progress_row = lines.find("middle response").unwrap();
    let last_row = lines.rfind("list agents:").unwrap();
    assert!(
        first_row < progress_row && progress_row < last_row,
        "{lines}"
    );
}

/// A later executor header cannot pass a progress say still waiting for its
/// earlier shell result, even when that shell's preview is already visible.
#[test]
fn runtime_running_shell_holds_interleaved_say_and_discovery_header() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "run and discover")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let shell = mez_agent::AgentAction {
        id: "shell-first".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: String::new(),
            command: "printf first-shell".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let say = mez_agent::AgentAction {
        id: "say-middle".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "middle progress".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "discovery-last".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![shell.clone(), say.clone(), discovery.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&shell, &say, &discovery]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .queue_ordered_provider_command("%1", &execution, &shell, "printf first-shell")
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let pending = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pending.contains("$ printf first-shell"), "{pending}");
    assert!(!pending.contains("middle progress"), "{pending}");
    assert!(!pending.contains("list agents:"), "{pending}");
    execution.action_results[0].status = ActionStatus::Succeeded;
    service
        .flush_ordered_provider_headers("%1", &execution)
        .unwrap();
    let settled = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        settled.find("first-shell").unwrap() < settled.find("middle progress").unwrap(),
        "{settled}"
    );
    assert!(
        settled.find("middle progress").unwrap() < settled.find("list agents:").unwrap(),
        "{settled}"
    );
}

/// Issue execution must publish a middle progress row before its next header.
#[test]
fn runtime_issue_headers_preserve_interleaved_progress_order() {
    let mut service = test_runtime_service();
    service.set_config_root(temp_root("interleaved-issue-logs"));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect issues")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let query = |id: &str, text: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: None,
            text: Some(text.to_string()),
            limit: None,
            refresh: false,
        },
    };
    let first = query("first-issue", "FIRST_ISSUE_MARKER");
    let last = query("last-issue", "LAST_ISSUE_MARKER");
    let middle = mez_agent::AgentAction {
        id: "middle-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "middle issue response".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![first.clone(), middle.clone(), last.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&first, &middle, &last]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_issue_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let first_row = lines.find("FIRST_ISSUE_MARKER").unwrap();
    let progress_row = lines.find("middle issue response").unwrap();
    let last_row = lines.find("LAST_ISSUE_MARKER").unwrap();
    assert!(
        first_row < progress_row && progress_row < last_row,
        "{lines}"
    );
}

/// Executor families may settle out of order, but their visible rows must
/// follow the accepted batch's action ordinals.
#[test]
fn runtime_issue_then_progress_then_discovery_preserves_log_order() {
    let mut service = test_runtime_service();
    service.set_config_root(temp_root("cross-family-log-order"));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect issues and peers")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let issue = mez_agent::AgentAction {
        id: "first-issue".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: None,
            text: Some("FIRST_ISSUE_MARKER".to_string()),
            limit: None,
            refresh: false,
        },
    };
    let progress = mez_agent::AgentAction {
        id: "middle-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "middle response".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "last-discovery".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![issue.clone(), progress.clone(), discovery.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&issue, &progress, &discovery]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    service
        .execute_running_issue_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let issue_row = lines.find("FIRST_ISSUE_MARKER").unwrap();
    let progress_row = lines.find("middle response").unwrap();
    let discovery_row = lines.find("list agents:").unwrap();
    assert!(
        issue_row < progress_row && progress_row < discovery_row,
        "{lines}"
    );
}

/// A settled later discovery must publish its queued header before the
/// progress say that follows it, even when the issue executor runs last.
#[test]
fn runtime_issue_then_discovery_then_progress_preserves_log_order() {
    let mut service = test_runtime_service();
    service.set_config_root(temp_root("cross-family-queued-middle"));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect issues and peers")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let issue = mez_agent::AgentAction {
        id: "first-issue".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: None,
            text: Some("FIRST_ISSUE_MARKER".to_string()),
            limit: None,
            refresh: false,
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "middle-discovery".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let progress = mez_agent::AgentAction {
        id: "last-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "last response".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![issue.clone(), discovery.clone(), progress.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&issue, &discovery, &progress]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    service
        .execute_running_issue_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let issue_row = lines.find("FIRST_ISSUE_MARKER").unwrap();
    let discovery_row = lines.find("list agents:").unwrap();
    let progress_row = lines.find("last response").unwrap();
    assert!(
        issue_row < discovery_row && discovery_row < progress_row,
        "{lines}"
    );
}

/// An outcome for a later action waits behind an earlier runtime-owned log.
#[test]
fn runtime_failed_outcome_waits_for_preceding_issue_log() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect then fetch")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let issue = mez_agent::AgentAction {
        id: "issue-first".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: None,
            text: Some("FIRST_ISSUE_MARKER".to_string()),
            limit: None,
            refresh: false,
        },
    };
    let fetch = mez_agent::AgentAction {
        id: "fetch-second".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
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
                rationale: String::new(),
                actions: vec![issue.clone(), fetch.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![
            mez_agent::ActionResult::running(&turn, &issue, Vec::new(), None),
            mez_agent::ActionResult::failed(
                &turn,
                &fetch,
                ActionStatus::Failed,
                "network_http_error",
                "HTTP 404",
            )
            .unwrap(),
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
        .unwrap();
    let pending = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pending.contains("HTTP 404"), "{pending}");
    let mut settled = execution.clone();
    settled.action_results[0].status = ActionStatus::Succeeded;
    service
        .queue_ordered_provider_header("%1", &settled, &issue)
        .unwrap();
    service
        .flush_ordered_provider_headers("%1", &settled)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        rows.find("FIRST_ISSUE_MARKER").unwrap() < rows.find("HTTP 404").unwrap(),
        "{rows}"
    );
    assert_eq!(rows.matches("HTTP 404").count(), 1, "{rows}");
    service
        .present_agent_action_outcomes_to_terminal_buffer("%1", &settled)
        .unwrap();
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(replayed.matches("HTTP 404").count(), 1, "{replayed}");
}

/// A failed response retires an unshown progress say so its later error survives.
#[test]
fn runtime_failed_response_releases_outcome_after_suppressed_say() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect then fetch")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let issue = mez_agent::AgentAction {
        id: "issue-first".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: None,
            text: Some("FIRST_ISSUE_MARKER".to_string()),
            limit: None,
            refresh: false,
        },
    };
    let say = mez_agent::AgentAction {
        id: "middle-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "do not show progress on failure".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let fetch = mez_agent::AgentAction {
        id: "fetch-last".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![issue.clone(), say.clone(), fetch.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![
            mez_agent::ActionResult::running(&turn, &issue, Vec::new(), None),
            mez_agent::ActionResult::running(&turn, &say, Vec::new(), None),
            mez_agent::ActionResult::failed(
                &turn,
                &fetch,
                ActionStatus::Failed,
                "network_http_error",
                "HTTP 404",
            )
            .unwrap(),
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .queue_ordered_provider_header("%1", &execution, &issue)
        .unwrap();
    service
        .flush_ordered_provider_headers("%1", &execution)
        .unwrap();
    execution.action_results[0].status = ActionStatus::Succeeded;
    execution.terminal_state = AgentTurnState::Failed;
    service
        .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!rows.contains("do not show progress on failure"), "{rows}");
    assert!(
        rows.find("FIRST_ISSUE_MARKER").unwrap() < rows.find("HTTP 404").unwrap(),
        "{rows}"
    );
    assert_eq!(rows.matches("HTTP 404").count(), 1, "{rows}");
    execution.terminal_state = AgentTurnState::Running;
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let after_correction = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !after_correction.contains("do not show progress on failure"),
        "{after_correction}"
    );
}

/// An approval hold must leave its deferred final answer eligible after the
/// same accepted execution resumes and succeeds.
#[test]
fn runtime_blocked_action_preserves_final_say_after_approval() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "fetch after approval")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let fetch = mez_agent::AgentAction {
        id: "fetch-pending".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/approved".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let answer = mez_agent::AgentAction {
        id: "say-after-approval".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "approved final answer".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![fetch.clone(), answer],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::blocked(
            &turn,
            &fetch,
            Vec::new(),
            "{\"approval\":{}}".to_string(),
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Blocked,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    assert!(
        !service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("approved final answer")
    );
    execution.action_results[0] =
        mez_agent::ActionResult::succeeded(&turn, &fetch, Vec::new(), None);
    execution.terminal_state = AgentTurnState::Completed;
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(rows.matches("approved final answer").count(), 1, "{rows}");
}

/// Approval and later failure are distinct settled outcomes for one action;
/// replaying either state must not duplicate its visible row.
#[test]
fn runtime_blocked_outcome_then_failure_publishes_each_once() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "fetch approved source")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let action = mez_agent::AgentAction {
        id: "fetch-approved".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/approved".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::blocked(
            &turn,
            &action,
            Vec::new(),
            "{\"approval\":{}}".to_string(),
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Blocked,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    for _ in 0..2 {
        service
            .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
            .unwrap();
    }
    execution.action_results[0] = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "network_http_error",
        "HTTP 503",
    )
    .unwrap();
    execution.terminal_state = AgentTurnState::Failed;
    for _ in 0..2 {
        service
            .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
            .unwrap();
    }
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(rows.matches("awaiting approval").count(), 1, "{rows}");
    assert_eq!(rows.matches("HTTP 503").count(), 1, "{rows}");
}

/// A later discovery header waits for an earlier config action even though
/// discovery executes first; its following progress is published last.
#[test]
fn runtime_config_then_discovery_then_progress_preserves_log_order() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 16).unwrap(), 120)
        .unwrap();
    service.set_config_root(temp_root("cross-family-config-log-order"));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "configure and discover")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let config = mez_agent::AgentAction {
        id: "config-first".to_string(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("123".to_string()),
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "discovery-second".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let progress = mez_agent::AgentAction {
        id: "progress-third".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "configuration settled".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![config.clone(), discovery.clone(), progress.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&config, &discovery, &progress]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    assert!(
        !service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("list agents:")
    );
    service
        .execute_running_config_change_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let config_row = lines.find("config change: set history.lines").unwrap();
    let discovery_row = lines.find("list agents:").unwrap();
    let progress_row = lines.find("configuration settled").unwrap();
    assert!(
        config_row < discovery_row && discovery_row < progress_row,
        "{lines}"
    );
}

/// Issue and close-agent actions must release a later progress say after
/// their runtime-owned results settle, even when no static header exists.
#[test]
fn runtime_issue_and_close_headers_release_deferred_progress() {
    for (kind, payload, header) in [
        (
            "issue_query",
            mez_agent::AgentActionPayload::IssueQuery {
                kind: None,
                state: None,
                text: None,
                limit: None,
                refresh: false,
            },
            "issue query",
        ),
        (
            "close_agent",
            mez_agent::AgentActionPayload::CloseAgent {
                agent_id: "agent-%2".to_string(),
            },
            "close agent",
        ),
    ] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
        );
        let action = mez_agent::AgentAction {
            id: kind.to_string(),
            payload,
        };
        let mut execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture("turn-deferred-kind"),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: String::new(),
                    actions: vec![
                        action.clone(),
                        mez_agent::AgentAction {
                            id: "progress".to_string(),
                            payload: mez_agent::AgentActionPayload::Say {
                                status: mez_agent::SayStatus::Progress,
                                text: "later progress".to_string(),
                                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
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
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        service
            .present_agent_response_actions_to_terminal_buffer("%1", &execution)
            .unwrap();
        assert!(
            !service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n")
                .contains("later progress")
        );
        let has_header = service
            .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
            .unwrap();
        execution.action_results.push(mez_agent::ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-deferred-kind".to_string(),
            agent_id: "agent-%1".to_string(),
            action_id: action.id.clone(),
            action_type: kind,
            status: ActionStatus::Succeeded,
            content: Vec::new(),
            structured_content_json: None,
            permission_evaluation: None,
            is_error: false,
            error: None,
        });
        service
            .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
            .unwrap();
        let lines = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(lines.contains(header), has_header, "{kind}: {lines}");
        assert_eq!(
            lines.matches("later progress").count(),
            1,
            "{kind}: {lines}"
        );
        if has_header {
            assert!(
                lines.find(header).unwrap() < lines.find("later progress").unwrap(),
                "{kind}: {lines}"
            );
        }
    }
}

/// Repeated shell settlement must not append a deferred final answer twice.
#[test]
fn runtime_deferred_final_say_is_published_once() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-final-once"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![
                    mez_agent::AgentAction {
                        id: "discovery".to_string(),
                        payload: mez_agent::AgentActionPayload::ListAgents {
                            agent_type: None,
                            scope: None,
                        },
                    },
                    mez_agent::AgentAction {
                        id: "final".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "one final answer".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
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
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("one final answer").count(), 1, "{text}");
}

/// Verifies progress `say` messages continue through durable assistant
/// chronology without a request-local ledger.
///
/// Progress text is already an assistant event at its occurrence boundary.
/// Replaying a second controller-generated copy would duplicate information and
/// invalidate the reusable prefix.
#[test]
fn runtime_progress_say_chronology_reaches_provider_continuation_without_ledger() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-progress-ledger","input":"fix the repeated progress updates"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "record the first sequence point".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-progress".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: "The redundant updates are coming from repeated progress says."
                            .to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let assistant_block = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::TranscriptAssistant)
        .expect("progress say should be preserved as assistant chronology");
    assert_eq!(
        assistant_block.placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    assert!(
        assistant_block
            .content
            .contains("The redundant updates are coming from repeated progress says."),
        "{}",
        assistant_block.content
    );
    assert!(context.validate_durable().is_ok());
    assert!(!context.blocks().iter().any(|block| {
        block.label.contains("ledger") || block.content.contains("progress_say:")
    }));

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::TranscriptAssistant
            && message
                .content
                .contains("The redundant updates are coming from repeated progress says.")
    }));
    assert!(!request.messages.iter().any(|message| {
        message
            .content
            .contains("[current-turn progress say ledger]")
            || message.content.contains("progress_say:")
    }));
    assert!(!service.agent_turn_contexts().contains_key("turn-1"));
}

/// Verifies runtime keeps repeated progress `say` updates visible during a turn.
///
/// Progress messages are user-visible sequence points, and repeated provider
/// updates should still render as ordinary progress output instead of being
/// silently transformed into a suppression marker.
#[test]
fn runtime_agent_keeps_redundant_progress_say_updates_visible() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-redundant-progress","input":"fix the selector"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_progress = "The selector bug is in the real resume pager path.";
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "record the owner".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-progress-1".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: first_progress.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);

    let duplicate_progress = "The surviving selector bug is still in the real resume pager path.";
    let final_text = "The fix is complete.";
    let second_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: duplicate_progress.to_string(),

                actions: vec![
                    mez_agent::AgentAction {
                        id: "say-progress-2".to_string(),

                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: duplicate_progress.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    mez_agent::AgentAction {
                        id: "say-final".to_string(),

                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: final_text.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Running);

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains(first_progress), "{pane_text}");
    assert!(pane_text.contains(duplicate_progress), "{pane_text}");
    assert!(pane_text.contains(final_text), "{pane_text}");
    assert!(
        executions[0]
            .action_results
            .iter()
            .any(|result| result.action_id == "say-progress-2" && !result.is_error),
        "{:?}",
        executions[0].action_results
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies agent presentation appended while its surface is hidden never
/// changes the process terminal and remains available for later reentry.
#[test]
fn runtime_hidden_agent_presentation_isolated_from_process_screen() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap();
    process_screen.feed(b"process-only sentinel\r\n");
    service.set_process_pane_screen("%1", process_screen);
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "hidden agent sentinel")
        .unwrap();

    let process_text = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_text.contains("process-only sentinel"),
        "{process_text}"
    );
    assert!(
        !process_text.contains("hidden agent sentinel"),
        "{process_text}"
    );
    assert!(agent_text.contains("hidden agent sentinel"), "{agent_text}");
    assert!(
        !agent_text.contains("process-only sentinel"),
        "{agent_text}"
    );
}

/// Verifies sandbox mapping warnings are visible without verbose mode and one
/// stable mapping outcome is retained only once in the affected pane log.
#[test]
fn runtime_sandbox_mapping_warning_is_visible_and_deduplicated() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();

    for _ in 0..2 {
        service
            .append_sandbox_mapping_warning_once(
                "%1",
                "supplementary-group:docker:not-active",
                "supplementary-group `docker` (not active in the pane shell)",
            )
            .unwrap();
    }

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        pane_text.matches("agent warning:").count(),
        1,
        "{pane_text}"
    );
    assert!(pane_text.contains("sandbox remains active"), "{pane_text}");
}

/// Verifies persisted presentation from another pane conversation is rejected
/// before replay can mutate either retained screen.
#[test]
fn runtime_agent_presentation_replay_rejects_mismatched_conversation() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap();
    process_screen.feed(b"process replay sentinel\r\n");
    service.set_process_pane_screen("%1", process_screen);
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "current-conversation", 0)
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "current agent sentinel")
        .unwrap();
    let process_before = service.process_pane_screen("%1").unwrap().clone();
    let agent_before = service.agent_pane_screen("%1").unwrap().clone();
    let stale = crate::storage::transcript::AgentPresentationEntry {
        conversation_id: "stale-conversation".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        pane_id: "%1".to_string(),
        turn_id: None,
        terminal_width: 80,
        style_names: vec!["assistant".to_string()],
        display_lines: vec!["stale presentation sentinel".to_string()],
        copy_lines: vec!["stale presentation sentinel".to_string()],
        ansi_text: None,
        source_text: Some("stale presentation sentinel".to_string()),
        source_content_type: Some(mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string()),
    };

    let error = service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &[stale])
        .unwrap_err();

    assert!(error.message().contains("active conversation"), "{error}");
    assert_eq!(service.process_pane_screen("%1").unwrap(), &process_before);
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &agent_before);
}
