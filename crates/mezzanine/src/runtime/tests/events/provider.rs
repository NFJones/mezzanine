//! Runtime tests for events provider behavior.

use super::*;

/// Verifies terminal transcript persistence accepts one complete execution
/// group exactly once even when two lifecycle paths attempt finalization.
///
/// Repeated cleanup must not append a second assistant/tool group or advance
/// the shell session's raw transcript high-water mark twice.
#[test]
fn runtime_terminal_execution_transcript_persistence_is_idempotent() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-terminal-transcript-idempotent");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "persist this result once")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "say-once".to_string(),
        rationale: "present the result once".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "done".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "present the result once".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["done".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let first = service
        .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
        .unwrap();
    let second = service
        .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
        .unwrap();
    let entries = transcript_store.inspect(&conversation_id).unwrap();

    assert!(first > 0);
    assert_eq!(second, 0);
    assert_eq!(entries.len(), first);
    assert!(entries.iter().all(|entry| entry.turn_id == turn.turn_id));
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        u64::try_from(first).unwrap()
    );
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies provider-completion validation accepts terminal controller failure
/// summaries.
///
/// Failure-summary completions are synthetic runtime-owned failures: the model
/// supplies a user-facing `say`, but the turn remains failed because the
/// provider/controller boundary had already failed before ordinary action
/// execution could continue.
#[test]
fn runtime_provider_completion_accepts_controller_failure_summary_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "determine the next implementation target")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let action = mez_agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "summarize the provider failure".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "The provider request failed before any action could run.".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let result = mez_agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["The provider request failed before any action could run.".to_string()],
        None,
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text:
                "provider_error: InvalidState: upstream failure\ncontroller_failure_summary:\nsummary"
                    .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    mez_agent::outcome::runtime_validate_provider_completion_execution(&turn, &mut execution)
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-completion validation accepts controller-owned MAAP
/// validation failures while retaining the rejected model batch.
///
/// The retained batch is diagnostic evidence only: validation has already
/// rejected it before action execution, so no action results exist even though
/// the response still carries the parsed batch for audit and transcript output.
#[test]
fn runtime_provider_completion_accepts_terminal_maap_validation_failure_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "call unavailable tool")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let action = mez_agent::AgentAction {
        id: "mcp-1".to_string(),
        rationale: "call missing tool".to_string(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "missing".to_string(),
            tool: "read".to_string(),
            arguments_json: "{}".to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "bad maap action\nmaap_validation_error: unavailable server".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    mez_agent::outcome::runtime_validate_provider_completion_execution(&turn, &mut execution)
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-completion validation rejects missing-batch executions
/// that are not terminal failures.
///
/// A missing action batch means the provider/controller path failed before
/// MAAP execution. That state is valid only as a terminal failed turn; accepting
/// it as running or completed would let malformed provider output enter the
/// scheduler as ordinary progress.
#[test]
fn runtime_provider_completion_rejects_nonterminal_missing_batch_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "hello").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "plain text without MAAP".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    mez_agent::outcome::runtime_validate_provider_completion_execution(&turn, &mut execution)
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.final_turn);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-completion validation rejects non-final empty action
/// batches.
///
/// Empty `all(...)` checks previously made an empty non-final batch look like a
/// display-only completion. The runtime boundary should reject that malformed
/// batch explicitly instead.
#[test]
fn runtime_provider_completion_rejects_empty_nonfinal_batch_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "hello").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "empty batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: Vec::new(),
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let error =
        mez_agent::outcome::runtime_validate_provider_completion_execution(&turn, &mut execution)
            .unwrap_err();

    assert!(
        error
            .message()
            .contains("action batch has no actions but is not final"),
        "{error}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime provider failure persists and finishes turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_failure_persists_and_finishes_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-fail","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            ModelProfile {
                provider: "runtime-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == mez_agent::transcript::TranscriptRole::Assistant
            && entry.content.contains("provider_error")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-fail""#), "{audit}");
    assert!(audit.contains(r#""model":"failing-model""#), "{audit}");
    assert!(audit.contains(r#""error_kind":"invalid_state""#), "{audit}");
    assert!(
        audit.contains(r#""error_message":"provider API request failed""#),
        "{audit}"
    );
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let provider_failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let provider_failure: serde_json::Value = serde_json::from_str(provider_failure_json).unwrap();
    assert_eq!(provider_failure["status_code"], 400);
    assert_eq!(
        provider_failure["error"]["message"],
        "stream must be set to true"
    );
    assert_eq!(provider_failure["error"]["type"], "invalid_request_error");
    assert_eq!(
        provider_failure["error"]["code"],
        "missing_required_parameter"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_bytes"]
            .as_str()
            .is_some_and(|value| value.parse::<usize>().unwrap() > 0),
        "{failed_audit_record}"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_sha256"]
            .as_str()
            .is_some_and(|value| value.len() == 64),
        "{failed_audit_record}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that provider errors carrying malformed raw output preserve that
/// output in the failed assistant transcript entry. This covers provider-native
/// MAAP parse failures that happen before the provider can build a
/// `ModelResponse`.
#[test]
fn runtime_provider_parse_failure_persists_raw_provider_text() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-parse-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-parse-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-parse-fail","input":"produce malformed maap"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeProviderRawTextFailingProvider,
            ModelProfile {
                provider: "runtime-raw-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == mez_agent::transcript::TranscriptRole::Assistant
            && entry
                .content
                .contains("{\"protocol\":\"maap/1\",\"actions\":[]}")
            && entry.content.contains("provider_error")
            && entry.content.contains("missing turn_id")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_sha256":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    assert!(audit.contains(r#"malformed_model_output"#), "{audit}");
    assert!(
        !audit.contains(r#""protocol":"maap/1","actions":[]"#),
        "{audit}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that batch-level MAAP validation failures are recorded as failed
/// agent turns with the provider's raw text and the validation diagnostic in
/// the persisted assistant transcript entry. This guards against treating
/// malformed provider output as an opaque provider failure after a response has
/// already been parsed into a `ModelResponse`.
#[test]
fn runtime_maap_validation_failure_persists_provider_response_detail() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-maap-validation-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-maap-validation-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "state",
            "state",
            "mcp-state",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "state",
            vec![mez_agent::mcp::McpToolState {
                server_id: String::new(),
                name: "list".to_string(),
                available: true,
                blacklisted: false,
                permission_required: false,
                effects: mez_agent::mcp::McpToolEffects::none(),
                approval: mez_agent::mcp::McpApprovalSetting::Allow,
                description: "list state".to_string(),
                input_schema_json: "{}".to_string(),
            }],
            1,
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-maap-validation-fail","input":"call @state unavailable tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "bad maap action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "call missing tool".to_string(),
                    payload: mez_agent::AgentActionPayload::McpCall {
                        server: "missing".to_string(),
                        tool: "read".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == mez_agent::transcript::TranscriptRole::Assistant
            && entry.content.contains("bad maap action")
            && entry.content.contains("maap_validation_error")
            && entry.content.contains("unavailable server")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let failure: serde_json::Value = serde_json::from_str(failure_json).unwrap();
    assert_eq!(failure["type"], "agent_turn_execution_failure");
    assert_eq!(failure["stage"], "maap_validation");
    assert_eq!(failure["response"]["action_batch_present"], true);
    assert_eq!(failure["response"]["action_count"], 1);
    assert!(
        failure["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable server")),
        "{failure}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies provider failure after a nonzero shell command does not reuse stale
/// running execution state for final diagnostics.
///
/// Nonzero shell commands are ordinary model-visible observations. If the
/// follow-up provider request then fails, the final failure must describe the
/// provider boundary cleanly instead of reporting the impossible state
/// `turn state is running, not failed`.
#[test]
fn runtime_provider_failure_after_nonzero_shell_result_does_not_report_running_recovery_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-shell-provider-fail","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-fail".to_string(),
                    rationale: "exercise failure feedback".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command that will need correction".to_string(),
                        command: "false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 127)
        .unwrap();

    let error = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchFailingProvider, 1)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("turn state is running, not failed"),
        "{pane_text}"
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-worker network results are applied without actor-side HTTP.
///
/// A large research turn can return many already-settled `fetch_url` results
/// from the async provider worker. The runtime actor must present and audit
/// those results, then queue model recovery for failed fetches, without trying
/// to run the network requests again while applying the provider completion.
#[tokio::test]
async fn runtime_provider_completion_records_preexecuted_network_results_before_recovery() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-preexecuted-network-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 160)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-preexecuted-network-results","input":"research provider docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let success_action = mez_agent::AgentAction {
        id: "fetch-ok".to_string(),
        rationale: "fetch an available provider document".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/ok".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let failed_action = mez_agent::AgentAction {
        id: "fetch-404".to_string(),
        rationale: "fetch a provider document that moved".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let success_result = mez_agent::ActionResult::succeeded(
        &turn,
        &success_action,
        vec!["provider document body".to_string()],
        Some(
            mez_agent::network_action_structured_content_json(
                &success_action,
                serde_json::Value::Null,
                serde_json::json!({
                    "url": "https://example.test/ok",
                    "status_code": 200,
                    "body_bytes": 22,
                    "returned_bytes": 22,
                    "requested_max_bytes": null,
                    "max_bytes": 16384,
                    "hard_max_bytes": 262144,
                    "truncated": false
                }),
            )
            .unwrap(),
        ),
    );
    let mut failed_result = mez_agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404",
    )
    .unwrap();
    failed_result.structured_content_json = Some(
        mez_agent::network_action_structured_content_json(
            &failed_action,
            serde_json::Value::Null,
            serde_json::json!({
                "url": "https://example.test/missing",
                "status_code": 404,
                "body_bytes": 0
            }),
        )
        .unwrap(),
    );
    let mut request = runtime_model_request_fixture("turn-1");
    request.provider = "runtime-batch".to_string();
    request.model = "test".to_string();
    request.agent_id = turn.agent_id.clone();
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::NetworkFetch);
    request.messages = vec![mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::User,
        source: ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::EphemeralTail,
        content: "research provider docs".to_string(),
    }];
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "provider docs fetch batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "fetch provider documentation sources".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![success_action, failed_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![success_result, failed_result],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let applied = service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();

    assert!(applied);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-ok fetch_url succeeded]")
            && block.content.contains("provider document body")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-404 fetch_url failed]")
            && block.content.contains("network request returned HTTP 404")
    }));
    let history = service
        .agent_network_action_history_for_tests()
        .get("turn-1")
        .unwrap();
    assert_eq!(history.requests.len(), 2);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: fetch url: https://example.test/ok"),
        "{pane_text}"
    );
    assert!(pane_text.contains("provider document body"), "{pane_text}");
    assert!(
        pane_text.contains("agent warning: URL fetch failed (HTTP 404)"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("model received the response detail")
            && pane_text.contains("for recovery"),
        "{pane_text}"
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action_id":"fetch-ok""#), "{audit}");
    assert!(audit.contains(r#""action_id":"fetch-404""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(audit_root);
}
