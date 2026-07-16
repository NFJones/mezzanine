//! Runtime tests for actions messaging behavior.

use super::*;

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
    assert!(child_context.blocks.iter().any(|block| {
        block
            .content
            .contains("inspect release notes for the requested version.")
    }));
    assert!(child_context.blocks.iter().any(|block| {
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

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
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

/// Verifies that MAAP `send_message` canonicalizes the common model-emitted
/// `text/plain` shorthand before MMP delivery. The transport endpoint remains
/// strict, but model-produced coordination messages should not fail a subagent
/// turn when the payload is otherwise valid UTF-8 text.
#[test]
fn runtime_canonicalizes_send_message_text_plain_alias() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
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
            "text/markdown",
            "hello worker",
            "MMP text payloads require content_type text/plain; charset=utf-8",
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
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("[action_result msg-1 send_message failed]")
                && block.content.contains("invalid_message_payload")
        }));
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.content.contains("Message recovery")
                && block.content.contains("Next step:")
                && block.content.contains("content_type and payload shape")
        }));
    }
}

/// Verifies that MAAP `send_message` accepts valid JSON payloads through the
/// same shared validator. This catches accidental text-only validation when the
/// action path is kept in sync with MMP transport dispatch.
#[test]
fn runtime_accepts_send_message_action_with_valid_json_payload() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("application/json", r#"{"status":"ok"}"#);

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "application/json");
    assert_eq!(messages[0].payload, r#"{"status":"ok"}"#);
}
