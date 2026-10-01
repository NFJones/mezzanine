//! Runtime send dispatch, correction budgets and objective fallback regressions.
//!
//! Dispatch authenticates and validates one envelope before accepting it. Invalid
//! recipients consume bounded correction work without replaying sibling actions.

use super::*;

/// Verifies retained message subscriptions survive a process-style snapshot
/// restore, commit unread messages once in sequence order before the next
/// prompt, and do not replay them after a second restore.
#[test]
fn runtime_local_message_cursor_restores_exactly_once_in_arrival_order() {
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let mut before_restart = test_runtime_service();
    let recipient_identity = before_restart
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    before_restart
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = before_restart
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    for (id, payload) in [
        ("restart-message-1", "first retained message"),
        ("restart-message-2", "second retained message"),
    ] {
        before_restart
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: id.to_string(),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(
                        recipient_identity.agent_id.clone(),
                    ),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: payload.to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now_ms,
            )
            .unwrap();
    }
    let restored_messages = mez_agent::messaging::MessageService::from_snapshot_state(
        &before_restart.control.message_service().snapshot_state(),
    )
    .unwrap();
    let mut after_restart = RuntimeSessionService::from_parts(
        SessionFixture::new().build(),
        PathBuf::from("/tmp/mez-1000/message-restart.sock"),
        100,
        ControlIdempotencyCache::default(),
        restored_messages,
        None,
    )
    .unwrap();
    after_restart
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let started = after_restart
        .start_agent_prompt_turn("%1", "use all retained messages")
        .unwrap();
    let context = after_restart
        .agent_turn_contexts()
        .get(&started.turn_id)
        .unwrap();
    let ordered_labels = context
        .blocks()
        .iter()
        .map(|block| block.label.as_str())
        .collect::<Vec<_>>();
    let first = ordered_labels
        .iter()
        .position(|label| label.contains("restart-message-1"))
        .unwrap();
    let second = ordered_labels
        .iter()
        .position(|label| label.contains("restart-message-2"))
        .unwrap();
    let prompt = ordered_labels
        .iter()
        .position(|label| *label == "user prompt")
        .unwrap();
    assert!(first < second && second < prompt, "{ordered_labels:?}");
    assert_eq!(
        after_restart
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        2
    );

    let restored_again = mez_agent::messaging::MessageService::from_snapshot_state(
        &after_restart.control.message_service().snapshot_state(),
    )
    .unwrap();
    let mut second_restart = RuntimeSessionService::from_parts(
        SessionFixture::new().build(),
        PathBuf::from("/tmp/mez-1000/message-restart-2.sock"),
        100,
        ControlIdempotencyCache::default(),
        restored_again,
        None,
    )
    .unwrap();
    second_restart
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let replay_check = second_restart
        .start_agent_prompt_turn("%1", "do not replay old messages")
        .unwrap();
    assert!(
        second_restart
            .agent_turn_contexts()
            .get(&replay_check.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .all(|block| !block.label.contains("restart-message-"))
    );
}

/// Verifies macro step slash commands are dispatched through the child agent shell.
///
/// This protects slash-command compatibility for macro steps: a step containing
/// `/loop` must not be delivered as a passive MMP message because that would
/// bypass the subagent shell parser and break the feature contract.
#[test]
fn runtime_agent_macro_send_message_queues_child_shell_turn() {
    let config_root = temp_root("runtime-macro-step-message");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
    macro_dir.join("MACRO.md"),
    "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. /loop inspect release notes for the requested version.\n2. Summarize release blockers.\n",
)
.unwrap();
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let child_agent_id = service
        .macro_managed_subagent_ids()
        .into_iter()
        .next()
        .expect("macro child should be registered");
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    assert_eq!(parent_turn.state, AgentTurnState::Blocked);
    let parent_execution = service
        .agent_turn_executions()
        .get(&parent_turn.turn_id)
        .expect(
            "parent macro orchestration execution should be waiting on runtime-owned first step",
        );
    assert_eq!(parent_execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        parent_execution.action_results[0].status,
        ActionStatus::Running
    );
    let structured = parent_execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap_or_default();
    assert!(
        structured.contains(r#""join_policy":"macro_step""#),
        "{structured}"
    );
    assert!(structured.contains(&child_agent_id), "{structured}");
    assert!(
        service
            .message_service()
            .receive_for(&AgentId::opaque(child_agent_id.clone()).unwrap(), u64::MAX)
            .is_empty()
    );
    let child_pane_id = child_agent_id
        .strip_prefix("agent-")
        .expect("macro child agent should identify its pane");
    let child_turn_id = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == child_pane_id)
        .map(|(turn_id, _)| turn_id.clone())
        .expect("runtime-owned /loop macro step should queue loop work");
    let loop_completion = service
        .agent_loop_state(child_pane_id)
        .and_then(|state| state.completion.as_ref())
        .expect("runtime-owned loop should retain the macro parent completion");
    assert_eq!(loop_completion.child_agent_id, child_agent_id);
    assert!(!service.has_joined_subagent_dependency(&child_turn_id));
    let macro_run = service
        .macro_run_for_tests(parent_turn.turn_id.as_str())
        .expect("macro run state should be keyed by parent turn");
    assert_eq!(macro_run.current_step, 0);
    assert_eq!(
        macro_run.steps[0].child_turn_id.as_deref(),
        Some(child_turn_id.as_str())
    );
    assert_eq!(
        service.macro_parent_turn_for_child(child_turn_id.as_str()),
        Some(&parent_turn.turn_id)
    );
    assert!(
        macro_run.steps[0]
            .submitted_prompt
            .as_deref()
            .unwrap_or_default()
            .contains("User additional context for this macro invocation:\nfor v1.2")
    );
    let child_pane_id = child_agent_id.strip_prefix("agent-").unwrap();
    let child_pane_text = service
        .pane_screen(child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        child_pane_text.contains("user> /loop inspect release notes for the requested version."),
        "{child_pane_text}"
    );
    let child_context = service.agent_turn_contexts().get(&child_turn_id).unwrap();
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("inspect release notes for the requested version.")
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("User additional context for this macro invocation:\nfor v1.2")
    }));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == child_turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert_eq!(
        service
            .agent_loop_turns_for_tests()
            .values()
            .filter(|loop_turn| loop_turn.pane_id == child_pane_id)
            .count(),
        1
    );
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that MAAP `send_message` still reaches the shared message queue
/// when its media metadata is valid. This protects the accepted text path while
/// invalid media handling is tightened to match MMP transport validation.
#[test]
fn runtime_executes_send_message_action_through_message_service() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain; charset=utf-8", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let structured: serde_json::Value = serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("delivery structured content"),
    )
    .unwrap();
    assert_eq!(structured["scope"], "session");
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""delivery_status":"accepted""#)
    );
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Malformed recipient arguments must yield durable correction feedback without
/// delivery; a subsequent model-authored correction delivers exactly once.
#[test]
fn runtime_invalid_message_recipient_queues_correction_without_delivery() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(
        result.error.as_ref().unwrap().code,
        "invalid_message_recipient"
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .is_some_and(|structured| structured.contains(r#""scope":"session""#))
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("accepted_recipient_forms")
    );
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    let mut response = execution.response.clone();
    let action = &mut response.action_batch.as_mut().unwrap().actions[0];
    action.id = "msg-corrected".to_string();
    if let mez_agent::AgentActionPayload::SendMessage { recipient, .. } = &mut action.payload {
        *recipient = format!("agent:{target}");
    }
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(corrected.action_results[0].status, ActionStatus::Succeeded);
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "handoff");
}

/// An unavailable recipient is model-correctable: the failed action remains
/// durable context and one corrected recipient choice delivers exactly once.
#[test]
fn runtime_unavailable_message_recipient_queues_correction_without_delivery() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(
        result.error.as_ref().unwrap().code,
        "message_recipient_unavailable"
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(r#""code":"message_recipient_unavailable""#))
    );
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );

    let mut response = execution.response.clone();
    let action = &mut response.action_batch.as_mut().unwrap().actions[0];
    action.id = "msg-corrected".to_string();
    if let mez_agent::AgentActionPayload::SendMessage { recipient, .. } = &mut action.payload {
        *recipient = format!("agent:{target}");
    }
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(corrected.action_results[0].status, ActionStatus::Succeeded);
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "handoff");
}

/// Repeating an unavailable recipient consumes the correction budget instead
/// of retrying delivery to the same absent agent indefinitely.
#[test]
fn runtime_unavailable_message_recipient_exhausts_correction_budget() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    service.set_agent_action_failure_retry_limit(1);
    let mut response = execution.response.clone();
    response.action_batch.as_mut().unwrap().actions[0].id = "msg-repeated".to_string();

    let repeated = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);

    assert_eq!(repeated.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// Repeating an invalid recipient consumes the existing correction budget and
/// terminates without delivering messages or scheduling unbounded continuations.
#[test]
fn runtime_invalid_message_recipient_exhausts_correction_budget() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    service.set_agent_action_failure_retry_limit(1);
    let mut response = execution.response.clone();
    response.action_batch.as_mut().unwrap().actions[0].id = "msg-repeated".to_string();
    let repeated = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(repeated.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// A joined child must retain a partially successful batch while correcting an
/// invalid parent recipient, then deliver and hand off each effect exactly once.
///
/// This composes recipient correction with the spawned-child lifecycle. The
/// successful sibling action must not be replayed during correction, the child
/// must keep both first-attempt results as provider context, and its corrected
/// `agent:<parent-id>` delivery must not displace the normal joined handoff.
#[test]
fn runtime_spawned_child_corrects_parent_recipient_without_replaying_sibling() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(2)
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let sibling_target = AgentId::opaque("agent-message-sibling").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: sibling_target.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: Some("worker".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();

    let parent = service
        .start_agent_prompt_turn("%1", "delegate messaging")
        .unwrap();
    let spawn_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn messaging child".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "delegate the messaging task".to_string(),

                actions: vec![runtime_spawn_agent_action(
                    "spawn-messaging-child",
                    "send both handoff messages",
                )],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let _spawned_execution = service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &spawn_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id)
        .cloned()
        .expect("spawned child turn");
    assert_eq!(
        child.state,
        AgentTurnState::Running,
        "{child:?}; execution={:?}",
        service.agent_turn_executions().get(&child.turn_id)
    );
    // The child uses its own effective approval policy; this scenario exercises
    // sibling-then-parent delivery ordering rather than the approval gate.
    service.set_pane_approval_policy_override(
        &child.pane_id,
        Some(mez_agent::ApprovalPolicy::AutoAllow),
    );

    let send_action = |id: &str, recipient: String, payload: &str| mez_agent::AgentAction {
        id: id.to_string(),

        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient,
            scope: Some("session".to_string()),
            content_type: "text/plain".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "send sibling and parent messages".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "send the completed sibling before the parent handoff".to_string(),

                actions: vec![
                    send_action(
                        "message-sibling-once",
                        format!("agent:{sibling_target}"),
                        "completed sibling delivery",
                    ),
                    send_action(
                        "message-parent-invalid",
                        "parent".to_string(),
                        "child handoff",
                    ),
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first = service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert!(first.action_results.iter().any(|result| {
        result.action_id == "message-sibling-once" && result.status == ActionStatus::Succeeded
    }));
    assert!(first.action_results.iter().any(|result| {
        result.action_id == "message-parent-invalid"
            && result.status == ActionStatus::Failed
            && result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "invalid_message_recipient")
    }));
    assert_eq!(
        service
            .message_service()
            .receive_for(&sibling_target, u64::MAX)
            .len(),
        1
    );
    let child_context = runtime_prepared_context_for_turn(&service, &child.turn_id);
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("[action_result message-sibling-once send_message succeeded]")
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("[action_result message-parent-invalid send_message failed]")
            && block.content.contains("invalid_message_recipient")
    }));
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| { task.turn_id == child.turn_id })
    );

    let corrected_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "correct the parent recipient".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "deliver the corrected parent handoff".to_string(),

                actions: vec![send_action(
                    "message-parent-corrected",
                    format!("agent:{}", parent.agent_id),
                    "child handoff",
                )],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&corrected_provider, 1)
        .unwrap();
    assert_eq!(corrected.len(), 1);
    assert_eq!(corrected[0].terminal_state, AgentTurnState::Running);
    assert_eq!(
        service
            .message_service()
            .receive_for(&sibling_target, u64::MAX)
            .len(),
        1
    );
    let parent_id = AgentId::opaque(parent.agent_id.clone()).unwrap();
    let parent_messages = service.message_service().receive_for(&parent_id, u64::MAX);
    assert_eq!(
        parent_messages
            .iter()
            .filter(|message| message.payload == "child handoff")
            .count(),
        1
    );
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    let terminal_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "Child work is complete.",
            true,
        ),
    };
    let terminal = service
        .poll_agent_provider_tasks_with_provider(&terminal_provider, 1)
        .unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].terminal_state, AgentTurnState::Completed);
    assert!(!service.has_joined_subagent_dependency(&child.turn_id));
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert_eq!(
        parent_context
            .blocks()
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::PeerMessage
                    && block.content.contains("child handoff")
            })
            .count(),
        1
    );
    assert_eq!(
        parent_context
            .blocks()
            .iter()
            .filter(|block| block.label == "action result spawn-messaging-child")
            .count(),
        1
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| { task.turn_id == parent.turn_id })
    );
    service.terminate_all_pane_processes().unwrap();
}

/// The text/plain shorthand is canonicalized before accepted MMP delivery;
/// correcting malformed recipients must not change this valid payload behavior.
#[test]
fn runtime_canonicalizes_send_message_text_plain_alias() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` uses the same text, JSON, and binary
/// payload metadata validation as the MMP transport endpoint. Rejected actions
/// must not enqueue messages because the agent-facing action result is the
/// durable protocol feedback for the failed local delivery.
#[test]
fn runtime_rejects_send_message_action_with_invalid_mmp_payload_metadata() {
    let cases = [
        (
            "text/html",
            "hello worker",
            "MMP text payloads require text/plain; charset=utf-8 or text/markdown",
        ),
        (
            "application/json",
            "not-json",
            "MMP JSON payload must be valid JSON",
        ),
        (
            "application/octet-stream",
            "AQID",
            "MMP binary payloads require payload_encoding base64",
        ),
    ];

    for (content_type, payload, expected_message) in cases {
        let (service, execution, target_agent) =
            execute_runtime_send_message_action(content_type, payload);

        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        let result = &execution.action_results[0];
        assert_eq!(result.status, ActionStatus::Failed);
        assert!(result.is_error);
        assert_eq!(
            result.error.as_ref().map(|error| error.code.as_str()),
            Some("invalid_message_payload")
        );
        assert_eq!(
            result.error.as_ref().map(|error| error.message.as_str()),
            Some(expected_message)
        );
        let structured = result.structured_content_json.as_deref().unwrap();
        assert!(structured.contains(r#""scope":"session""#));
        assert!(structured.contains(r#""delivery_status":"rejected""#));
        assert!(structured.contains(r#""code":"invalid_params""#));
        assert!(structured.contains(expected_message), "{structured}");
        assert!(
            service
                .message_service()
                .receive_for(&target_agent, u64::MAX)
                .is_empty()
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1")
        );
        let context = service.agent_turn_contexts().get("turn-1").unwrap();
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("[action_result msg-1 send_message failed]")
                && block.content.contains("invalid_message_payload")
        }));
        assert!(
            context
                .blocks()
                .iter()
                .all(|block| block.source != ContextSourceKind::RuntimeHint)
        );
    }
}

/// Verifies that MAAP `send_message` accepts valid JSON payloads through the
/// same shared validator. This catches accidental text-only validation when the
/// action path is kept in sync with MMP transport dispatch.
#[test]
fn runtime_accepts_send_message_action_with_valid_json_payload() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("application/json", r#"{"status":"ok"}"#);

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "application/json");
    assert_eq!(messages[0].payload, r#"{"status":"ok"}"#);
}

/// Verifies the per-agent objective refreshes from the turn prompt through the
/// same identity registry discovery reads, that an unchanged republish performs
/// no write, and that a changed value republishes once.
#[test]
fn runtime_agent_objective_refresh_is_bounded_and_throttled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "  inspect   the   discovery contract ")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("inspect the discovery contract")
    );
    let published_at_ms = service
        .message_service()
        .presence()
        .into_iter()
        .find(|record| record.identity.agent_id == agent_id)
        .map(|record| record.updated_at_ms)
        .unwrap();

    assert!(!service.publish_prepared_runtime_agent_objective(
        started.agent_id.as_str(),
        Some("inspect the discovery contract")
    ));
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == agent_id)
            .map(|record| record.updated_at_ms),
        Some(published_at_ms)
    );

    assert!(service.publish_prepared_runtime_agent_objective(
        started.agent_id.as_str(),
        Some("Review the discovery contract")
    ));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Review the discovery contract")
    );
}

/// Verifies the prompt-derived fallback objective reuses the shared bounds and
/// publishes nothing when the prompt text cannot satisfy them.
#[test]
fn runtime_agent_objective_fallback_honors_shared_bounds() {
    assert_eq!(
        RuntimeSessionService::runtime_agent_objective_from_prompt("  Inspect\t the   backlog ")
            .as_deref(),
        Some("Inspect the backlog")
    );
    assert!(RuntimeSessionService::runtime_agent_objective_from_prompt("   ").is_none());
    assert!(
        RuntimeSessionService::runtime_agent_objective_from_prompt("inspect\u{7}the pane")
            .is_none()
    );
}

/// Verifies a turn without a model-authored objective still derives one from
/// its own prompt context, and keeps the previous published objective when a
/// refresh cannot be bounded.
#[test]
fn runtime_agent_turn_objective_uses_turn_prompt_context() {
    let mut service = test_runtime_service();
    let turn = AgentTurnRecord {
        turn_id: "turn-objective".to_string(),
        conversation_id: "conversation-objective".to_string(),
        agent_id: "agent-%9".to_string(),
        pane_id: "%9".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 2,
        policy_profile: "runtime".to_string(),
        model_profile: "test".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Queued,
        initial_capability: None,
    };
    service.agent_turn_contexts_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentContext::new(vec![ContextBlock::user_event(
            "user prompt",
            "  inspect   the turn context ",
        )])
        .unwrap(),
    );
    assert_eq!(
        service.runtime_agent_turn_objective(&turn).as_deref(),
        Some("inspect the turn context")
    );

    service.agent_turn_contexts_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentContext::new(vec![ContextBlock::user_event(
            "user prompt",
            "inspect\u{7}the turn context",
        )])
        .unwrap(),
    );
    assert!(service.runtime_agent_turn_objective(&turn).is_none());
}
