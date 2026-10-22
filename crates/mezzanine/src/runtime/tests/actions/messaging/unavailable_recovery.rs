//! Bounded correction for unavailable MMP recipients in mixed final batches.
//!
//! The reopened report describes a provider-driven final batch whose message
//! action failed because the recipient was not registered or available, with
//! no corrective provider continuation. Executor families run messages before
//! discovery and other local families, so a later local placeholder can remain
//! `Running` without ever starting. These regressions require one truthful
//! terminal abandonment for such a placeholder, the original failed result in
//! chronology, exactly-once successful siblings, and the existing bounded
//! same-turn correction. Active external, subagent, approval, denial and
//! cancellation boundaries stay in force.

use super::*;
use crate::integrations::agent::provider::ModelProvider;
use mez_agent::{AgentAction, AgentActionPayload, SayStatus, turn_state_from_action_results};

/// One registered local recipient plus a running turn at `%1`.
fn mixed_batch_fixture() -> (RuntimeSessionService, AgentId) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let target = AgentId::opaque("agent-%2").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: target.clone(),
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
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service.pane_screen_mut("%1").unwrap().feed(b"ready\n");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"mixed-unavailable-batch","input":"run the mixed final batch"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    (service, target)
}

/// Builds one send_message action.
fn send_action(id: &str, recipient: &str, payload: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        payload: AgentActionPayload::SendMessage {
            recipient: recipient.to_string(),
            scope: Some("session".to_string()),
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    }
}

/// Builds one final `say` action.
fn final_say_action(id: &str, text: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        payload: AgentActionPayload::Say {
            status: SayStatus::Final,
            text: text.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    }
}

/// Builds one read-only local discovery action.
fn list_agents_action(id: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        payload: AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    }
}

/// Builds a provider response carrying one exact provider identity.
fn provider_response(provider: &str, actions: Vec<AgentAction>) -> mez_agent::ModelResponse {
    mez_agent::ModelResponse {
        provider: provider.to_string(),
        model: "test".to_string(),
        raw_text: "mixed unavailable-recipient batch".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(mez_agent::MaapBatch {
            rationale: "mixed unavailable-recipient batch".to_string(),
            actions,
        }),
        provider_transcript_events: Vec::new(),
    }
}

/// Builds an ordinary canonical runtime-batch response.
///
/// The injected fake provider validates response identity against its own id.
/// Only the explicit provider-control cases call `provider_response` directly.
fn batch_response(provider: &str, actions: Vec<AgentAction>) -> mez_agent::ModelResponse {
    let provider = if provider == "alternate-compat" {
        "alternate-compat"
    } else {
        "runtime-batch"
    };
    provider_response(provider, actions)
}

/// Executes one batch through the synchronous production provider path.
fn execute_sync(
    service: &mut RuntimeSessionService,
    response: mez_agent::ModelResponse,
) -> mez_agent::AgentTurnExecution {
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchProvider { response },
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap()
}

/// Wraps the batch fixture under an explicit provider compatibility identity.
struct CompatibilityProvider {
    inner: RuntimeBatchProvider,
    provider_id: &'static str,
    api: mez_agent::ProviderApiCompatibility,
}

impl ModelProvider for CompatibilityProvider {
    fn provider_id(&self) -> &str {
        self.provider_id
    }

    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        self.api
    }

    fn send_request(&self, request: &mez_agent::ModelRequest) -> Result<mez_agent::ModelResponse> {
        let mut response = self.inner.send_request(request)?;
        response.provider = self.provider_id().to_string();
        Ok(response)
    }
}

/// Counts one action-result block in the active turn context.
///
/// This is a pre-terminal fixture assertion only: normal terminal lifecycle
/// removes the active context, so post-completion evidence must be checked
/// through persistence or an owner-level fixture instead.
fn action_result_block_count(service: &RuntimeSessionService, action_id: &str) -> usize {
    service
        .agent_turn_contexts()
        .get("turn-1")
        .map(|context| {
            context
                .blocks()
                .iter()
                .filter(|block| {
                    block.source == ContextSourceKind::ActionResult
                        && block
                            .content
                            .contains(&format!("action_result {action_id}"))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Renders result statuses for failure diagnostics without payload bytes.
fn result_diagnostics(execution: &mez_agent::AgentTurnExecution) -> Vec<String> {
    execution
        .action_results
        .iter()
        .map(|result| {
            format!(
                "{}:{:?}:{}",
                result.action_type,
                result.status,
                result
                    .error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or("-")
            )
        })
        .collect()
}

/// Finds one result by action id with a bounded diagnostic.
fn result_for<'a>(
    execution: &'a mez_agent::AgentTurnExecution,
    action_id: &str,
) -> &'a mez_agent::ActionResult {
    execution
        .action_results
        .iter()
        .find(|result| result.action_id == action_id)
        .unwrap_or_else(|| {
            panic!(
                "missing action result {action_id}: {:?}",
                result_diagnostics(execution)
            )
        })
}

/// The reported shape: unavailable send, unstarted local discovery sibling and
/// final say. Correction must queue once, retain the failure and abandonment
/// evidence, and complete after the model changes course.
#[test]
fn runtime_unavailable_recipient_corrects_past_unstarted_local_sibling() {
    let (mut service, target) = mixed_batch_fixture();
    let actions = vec![
        send_action("msg-missing", "agent:agent-nowhere", "handoff"),
        list_agents_action("discover-1"),
        final_say_action("say-final", "finished"),
    ];
    assert!(
        !mez_agent::maap::batch_requests_terminal_completion(&actions),
        "a mixed final say must not infer terminal intent"
    );
    let execution = execute_sync(&mut service, batch_response("deepseek", actions));
    assert_eq!(
        execution.terminal_state,
        AgentTurnState::Running,
        "results={:?}",
        result_diagnostics(&execution)
    );
    let failed = result_for(&execution, "msg-missing");
    assert_eq!(failed.status, ActionStatus::Failed);
    assert_eq!(
        failed.error.as_ref().unwrap().code,
        "message_recipient_unavailable"
    );
    let discovery = result_for(&execution, "discover-1");
    assert_eq!(discovery.status, ActionStatus::Cancelled);
    assert!(discovery.is_error);
    assert_eq!(
        discovery.error.as_ref().unwrap().code,
        "action_abandoned_not_started"
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
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.content.contains("message_recipient_unavailable")
            && block.content.contains("msg-missing")
    }));
    assert!(context.blocks().iter().any(|block| {
        block.content.contains("action_abandoned_not_started")
            && block.content.contains("discover-1")
    }));
    let corrected = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_say_response("turn-1", "finished after correction", true),
            },
            1,
        )
        .unwrap()
        .remove(0);
    assert_eq!(corrected.terminal_state, AgentTurnState::Completed);
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// The unavailable action fails at each of the three planned positions while a
/// local discovery sibling and final say remain in the batch. Each position
/// must still receive the bounded same-turn correction.
#[test]
fn runtime_unavailable_recipient_correction_covers_all_action_positions() {
    for missing_at in 0..3 {
        let (mut service, _target) = mixed_batch_fixture();
        let mut actions = vec![
            final_say_action("say-final", "finished"),
            list_agents_action("discover-1"),
            send_action("msg-missing", "agent:agent-nowhere", "handoff"),
        ];
        let missing = actions.pop().unwrap();
        actions.insert(missing_at, missing);
        let execution = execute_sync(&mut service, batch_response("deepseek", actions));
        assert_eq!(
            execution.terminal_state,
            AgentTurnState::Running,
            "position {missing_at}: {:?}",
            result_diagnostics(&execution)
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1"),
            "position {missing_at}"
        );
        let corrected = service
            .poll_agent_provider_tasks_with_provider(
                &RuntimeBatchProvider {
                    response: runtime_say_response("turn-1", "changed course", true),
                },
                1,
            )
            .unwrap()
            .remove(0);
        assert_eq!(corrected.terminal_state, AgentTurnState::Completed);
    }
}

/// Successful siblings from the same batch are delivered exactly once and are
/// never replayed when the model changes course after the failed recipient.
#[test]
fn runtime_unavailable_recipient_correction_preserves_successful_sibling() {
    let (mut service, target) = mixed_batch_fixture();
    let success_recipient = format!("agent:{target}");
    let execution = execute_sync(
        &mut service,
        batch_response(
            "deepseek",
            vec![
                send_action("msg-target", &success_recipient, "accepted handoff"),
                send_action("msg-missing", "agent:agent-nowhere", "lost handoff"),
                list_agents_action("discover-1"),
                final_say_action("say-final", "finished"),
            ],
        ),
    );
    assert_eq!(
        execution.terminal_state,
        AgentTurnState::Running,
        "results={:?}",
        result_diagnostics(&execution)
    );
    assert_eq!(
        result_for(&execution, "msg-target").status,
        ActionStatus::Succeeded
    );
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "accepted handoff");
    for action_id in ["msg-target", "msg-missing", "discover-1"] {
        assert_eq!(
            action_result_block_count(&service, action_id),
            1,
            "{action_id} must appear exactly once in canonical chronology"
        );
    }
    let corrected = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_say_response("turn-1", "changed course", true),
            },
            1,
        )
        .unwrap()
        .remove(0);
    assert_eq!(corrected.terminal_state, AgentTurnState::Completed);
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(
        messages.len(),
        1,
        "successful sibling must not be replayed by correction"
    );
    assert_eq!(messages[0].payload, "accepted handoff");
}

/// Inline async apply must queue the same bounded continuation and complete
/// normally after the model changes course; production actor-deferred dispatch
/// remains a separate integration gate.
#[tokio::test]
async fn runtime_unavailable_recipient_correction_is_async_parity() {
    let (mut service, target) = mixed_batch_fixture();
    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &RuntimeBatchProvider {
                response: batch_response(
                    "deepseek",
                    vec![
                        send_action("msg-missing", "agent:agent-nowhere", "handoff"),
                        list_agents_action("discover-1"),
                        final_say_action("say-final", "finished"),
                    ],
                ),
            },
            runtime_model_profile("runtime-batch", "test"),
        )
        .await
        .unwrap();
    assert_eq!(
        execution.terminal_state,
        AgentTurnState::Running,
        "results={:?}",
        result_diagnostics(&execution)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
    let corrected = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_say_response("turn-1", "changed course", true),
            },
            1,
        )
        .unwrap()
        .remove(0);
    assert_eq!(corrected.terminal_state, AgentTurnState::Completed);
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// A different provider compatibility identifier uses the same runtime
/// admission and correction path; the failure is not provider shaping.
#[test]
fn runtime_unavailable_recipient_correction_is_provider_agnostic() {
    for control in [
        None,
        Some((
            "deepseek",
            mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions,
        )),
        Some((
            "alternate-compat",
            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
        )),
    ] {
        let (mut service, _target) = mixed_batch_fixture();
        let actions = vec![
            send_action("msg-missing", "agent:agent-nowhere", "handoff"),
            list_agents_action("discover-1"),
            final_say_action("say-final", "finished"),
        ];
        let execution = if let Some((provider_id, api)) = control {
            service
                .execute_agent_turn_with_provider(
                    "turn-1",
                    &CompatibilityProvider {
                        inner: RuntimeBatchProvider {
                            response: provider_response(provider_id, actions),
                        },
                        provider_id,
                        api,
                    },
                    runtime_model_profile(provider_id, "test"),
                )
                .unwrap()
        } else {
            execute_sync(&mut service, batch_response("runtime-batch", actions))
        };
        assert_eq!(
            execution.terminal_state,
            AgentTurnState::Running,
            "control={control:?}: {:?}",
            result_diagnostics(&execution)
        );
        assert_eq!(
            execution.response.provider,
            control
                .map(|(provider_id, _)| provider_id)
                .unwrap_or("runtime-batch")
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1")
        );
    }
}

/// An active external sibling still owns work and blocks correction until it
/// settles; the bounded admission must not abandon it.
#[test]
fn runtime_active_external_sibling_keeps_correction_unavailable() {
    let (mut service, turn) = open_turn_for_constructed_execution();
    let missing = send_action("msg-missing", "agent:agent-nowhere", "handoff");
    let spawn = runtime_spawn_agent_action("spawn-active", "background child");
    let discovery = list_agents_action("discover-local");
    let say = final_say_action("say-final", "finished");
    let results = vec![
        mez_agent::ActionResult::failed(
            &turn,
            &missing,
            ActionStatus::Failed,
            "message_recipient_unavailable",
            "message recipient is not registered or available",
        )
        .unwrap(),
        mez_agent::ActionResult::running(&turn, &spawn, Vec::new(), None),
        mez_agent::ActionResult::running(&turn, &discovery, Vec::new(), None),
        mez_agent::ActionResult::succeeded(&turn, &say, vec!["finished".to_string()], None),
    ];
    let mut execution =
        constructed_execution(&turn, vec![missing, spawn, discovery, say], results, false);
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .commit_settled_action_results_context(
            &turn.turn_id,
            &[
                execution.action_results[0].clone(),
                execution.action_results[3].clone(),
            ],
        )
        .unwrap();
    let queued = service
        .queue_agent_failure_feedback_for_correction(&turn, &mut execution, "fixture")
        .unwrap();
    assert!(!queued, "active external sibling must block correction");
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        result_for(&execution, "spawn-active").status,
        ActionStatus::Running
    );
    assert_eq!(
        result_for(&execution, "discover-local").status,
        ActionStatus::Cancelled,
        "unstarted read-only discovery is settled even when correction is blocked"
    );
    assert_eq!(action_result_block_count(&service, "discover-local"), 1);
    assert_eq!(action_result_block_count(&service, "msg-missing"), 1);
}

/// Approval-blocked and policy-denied siblings retain their existing gates and
/// are never reclassified into model correction.
#[test]
fn runtime_blocked_and_denied_siblings_keep_correction_unavailable() {
    for denied in [false, true] {
        let (mut service, turn) = open_turn_for_constructed_execution();
        let missing = send_action("msg-missing", "agent:agent-nowhere", "handoff");
        let spawn = runtime_spawn_agent_action("spawn-sibling", "child");
        let say = final_say_action("say-final", "finished");
        let sibling = if denied {
            mez_agent::ActionResult::failed(
                &turn,
                &spawn,
                ActionStatus::Denied,
                "forbidden",
                "policy denied delegation",
            )
            .unwrap()
        } else {
            mez_agent::ActionResult::blocked(&turn, &spawn, Vec::new(), "{}".to_string())
        };
        let results = vec![
            mez_agent::ActionResult::failed(
                &turn,
                &missing,
                ActionStatus::Failed,
                "message_recipient_unavailable",
                "message recipient is not registered or available",
            )
            .unwrap(),
            sibling,
            mez_agent::ActionResult::succeeded(&turn, &say, vec!["finished".to_string()], None),
        ];
        let mut execution = constructed_execution(&turn, vec![missing, spawn, say], results, false);
        append_test_execution_assistant_context(&mut service, &turn, &execution);
        let queued = service
            .queue_agent_failure_feedback_for_correction(&turn, &mut execution, "fixture")
            .unwrap();
        assert!(
            !queued,
            "denied={denied}: sibling gate must block correction"
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(
            result_for(&execution, "spawn-sibling").status,
            if denied {
                ActionStatus::Denied
            } else {
                ActionStatus::Blocked
            }
        );
    }
}

/// A repeated unavailable failure with a renamed action id consumes the stable
/// per-signature budget instead of restarting correction indefinitely.
#[test]
fn runtime_unavailable_recipient_mixed_batch_correction_budget_is_stable() {
    let (mut service, _target) = mixed_batch_fixture();
    service.set_agent_action_failure_retry_limit(1);
    let execution = execute_sync(
        &mut service,
        batch_response(
            "deepseek",
            vec![
                send_action("msg-missing-1", "agent:agent-nowhere", "handoff"),
                list_agents_action("discover-1"),
                final_say_action("say-final", "finished"),
            ],
        ),
    );
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let repeated = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: batch_response(
                    "deepseek",
                    vec![
                        send_action("msg-missing-2", "agent:agent-nowhere", "handoff"),
                        list_agents_action("discover-2"),
                        final_say_action("say-final-2", "finished"),
                    ],
                ),
            },
            1,
        )
        .unwrap()
        .remove(0);
    assert_eq!(
        repeated.terminal_state,
        AgentTurnState::Failed,
        "results={:?}",
        result_diagnostics(&repeated)
    );
    assert_eq!(
        result_for(&repeated, "discover-2").status,
        ActionStatus::Cancelled
    );
    assert_eq!(
        result_for(&repeated, "discover-2")
            .error
            .as_ref()
            .unwrap()
            .code,
        "action_abandoned_not_started"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Settlement commits a truthful abandonment even when bounded correction is
/// exhausted, and repeating settlement/feedback does not duplicate evidence.
#[test]
fn runtime_settlement_commits_abandonment_before_budget_exhaustion() {
    let (mut service, _target) = mixed_batch_fixture();
    service.set_agent_action_failure_retry_limit(1);
    let first = execute_sync(
        &mut service,
        batch_response(
            "runtime-batch",
            vec![
                send_action("msg-attempt-1", "agent:agent-nowhere", "handoff"),
                list_agents_action("discover-attempt-1"),
                final_say_action("say-attempt-1", "finished"),
            ],
        ),
    );
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    // Simulate the runtime claiming the admitted corrective continuation while
    // keeping the same turn and stable failed-action signature.
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let missing = send_action("msg-custody", "agent:agent-nowhere", "handoff");
    let discovery = list_agents_action("discover-custody");
    let say = final_say_action("say-custody", "finished");
    let failed = mez_agent::ActionResult::failed(
        &turn,
        &missing,
        ActionStatus::Failed,
        "message_recipient_unavailable",
        "message recipient is not registered or available",
    )
    .unwrap();
    let succeeded =
        mez_agent::ActionResult::succeeded(&turn, &say, vec!["finished".to_string()], None);
    let running = mez_agent::ActionResult::running(&turn, &discovery, Vec::new(), None);
    let mut execution = constructed_execution(
        &turn,
        vec![missing, discovery, say],
        vec![failed.clone(), running, succeeded.clone()],
        false,
    );
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .commit_settled_action_results_context(&turn.turn_id, &[failed, succeeded])
        .unwrap();
    let queued = service
        .queue_agent_failure_feedback_for_correction(&turn, &mut execution, "fixture")
        .unwrap();
    assert!(!queued, "consumed limit-one budget exhausts correction");
    assert_eq!(
        result_for(&execution, "discover-custody").status,
        ActionStatus::Cancelled
    );
    assert_eq!(action_result_block_count(&service, "msg-custody"), 1);
    assert_eq!(action_result_block_count(&service, "discover-custody"), 1);
    let queued_again = service
        .queue_agent_failure_feedback_for_correction(&turn, &mut execution, "fixture")
        .unwrap();
    assert!(!queued_again);
    assert_eq!(action_result_block_count(&service, "discover-custody"), 1);
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Counts native provider tool-result events for one call identity.
fn native_tool_result_count(service: &RuntimeSessionService, call_id: &str) -> usize {
    service
        .agent_turn_contexts()
        .get("turn-1")
        .map(|context| {
            context
                .chronology()
                .iter()
                .filter(|event| {
                    event.block().source == ContextSourceKind::TranscriptTool
                        && event.block().label == format!("provider tool result {call_id}")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Returns the native provider tool-result content for one call identity.
fn native_tool_result_content(service: &RuntimeSessionService, call_id: &str) -> Option<String> {
    service
        .agent_turn_contexts()
        .get("turn-1")
        .and_then(|context| {
            context
                .chronology()
                .iter()
                .find(|event| {
                    event.block().source == ContextSourceKind::TranscriptTool
                        && event.block().label == format!("provider tool result {call_id}")
                })
                .map(|event| event.block().content.clone())
        })
}

/// DeepSeek native tool-call composition: committing only original terminal
/// results must not emit a premature tool result; settling the abandoned
/// discovery through real feedback admission completes exactly one native
/// result containing the failed, abandoned and successful evidence, and the
/// corrective request sees the same content without duplication.
#[test]
fn runtime_native_tool_result_completes_only_after_abandonment_settlement() {
    let (mut service, _target) = mixed_batch_fixture();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let missing = send_action("msg-native", "agent:agent-nowhere", "handoff");
    let discovery = list_agents_action("discover-native");
    let say = final_say_action("say-native", "finished");
    let failed = mez_agent::ActionResult::failed(
        &turn,
        &missing,
        ActionStatus::Failed,
        "message_recipient_unavailable",
        "message recipient is not registered or available",
    )
    .unwrap();
    let succeeded =
        mez_agent::ActionResult::succeeded(&turn, &say, vec!["finished".to_string()], None);
    let running = mez_agent::ActionResult::running(&turn, &discovery, Vec::new(), None);
    let mut execution = constructed_execution(
        &turn,
        vec![missing, discovery, say],
        vec![failed.clone(), running, succeeded.clone()],
        false,
    );
    execution.request.provider = "deepseek".to_string();
    execution.response.provider = "deepseek".to_string();
    execution.response.provider_transcript_events = vec![
        mez_agent::ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![serde_json::json!({
                "id": "call-native-group",
                "type": "function",
                "function": {"name": "submit_maap_action_batch", "arguments": "{}"}
            })],
        },
    ];
    let owner = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions,
        "deepseek",
    )
    .unwrap();
    service
        .append_agent_execution_chronology_for_provider(&turn, &execution, Some(&owner))
        .unwrap();
    service
        .commit_settled_action_results_context(&turn.turn_id, &[failed, succeeded])
        .unwrap();
    assert_eq!(
        native_tool_result_count(&service, "call-native-group"),
        0,
        "a native group result requires every owned action to settle"
    );
    let queued = service
        .queue_agent_failure_feedback_for_correction(&turn, &mut execution, "fixture")
        .unwrap();
    assert!(queued, "settled abandonment must admit bounded correction");
    assert_eq!(native_tool_result_count(&service, "call-native-group"), 1);
    let native = native_tool_result_content(&service, "call-native-group").unwrap();
    let native_event =
        mez_agent::ProviderTranscriptEvent::from_transcript_content(&native).unwrap();
    let mez_agent::ProviderTranscriptEvent::DeepSeekToolResult {
        tool_call_id,
        content: native_content,
    } = &native_event
    else {
        panic!("native result must project as a DeepSeek tool result");
    };
    assert_eq!(tool_call_id.as_str(), "call-native-group");
    assert!(native_content.contains("message_recipient_unavailable"));
    assert!(native_content.contains("action_abandoned_not_started"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let result_event = context
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::TranscriptTool
                && event.block().label == "provider tool result call-native-group"
        })
        .unwrap();
    let owner = result_event.provider_owner().unwrap();
    assert_eq!(owner.provider_id(), "deepseek");
    assert_eq!(
        owner.api(),
        mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions
    );
    let group = result_event.execution_group_id().cloned().unwrap();
    let assistant_event = context
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::TranscriptTool
                && event.execution_group_id() == Some(&group)
                && event.block().content.contains("call-native-group")
                && !event.block().label.starts_with("provider tool result")
        })
        .expect("original native assistant call must share the result group");
    assert_eq!(
        assistant_event.provider_owner().unwrap().provider_id(),
        "deepseek"
    );
    assert!(native.contains("message_recipient_unavailable"));
    assert!(native.contains("action_abandoned_not_started"));
    assert!(native.contains("finished"));
    service
        .commit_settled_action_results_context(&turn.turn_id, &execution.action_results.clone())
        .unwrap();
    assert_eq!(
        native_tool_result_count(&service, "call-native-group"),
        1,
        "repeated full commit must not duplicate the native tool result"
    );
    let prepared = crate::runtime::tests::runtime_prepared_context_for_turn(&service, "turn-1");
    let corrective = crate::integrations::agent::context::assemble_model_request(
        &runtime_model_profile("deepseek", "test"),
        mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions,
        &turn,
        &prepared,
    )
    .unwrap();
    let events = corrective
        .messages
        .iter()
        .filter_map(|message| {
            mez_agent::ProviderTranscriptEvent::from_transcript_content(&message.content)
        })
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        mez_agent::ProviderTranscriptEvent::DeepSeekAssistantToolCall { tool_calls, .. }
            if tool_calls.iter().any(|call| call["id"] == "call-native-group")
    )));
    let corrective_result = events
        .iter()
        .find_map(|event| match event {
            mez_agent::ProviderTranscriptEvent::DeepSeekToolResult {
                tool_call_id,
                content,
            } if tool_call_id.as_str() == "call-native-group" => Some(content.clone()),
            _ => None,
        })
        .expect("corrective request must carry the matching DeepSeek tool result");
    assert!(corrective_result.contains("message_recipient_unavailable"));
    assert!(corrective_result.contains("action_abandoned_not_started"));
}

/// Produces a running turn without a provider execution.
fn open_turn_for_constructed_execution() -> (RuntimeSessionService, AgentTurnRecord) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service.pane_screen_mut("%1").unwrap().feed(b"ready\n");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"constructed-unavailable-batch","input":"run the constructed batch"}}"#,
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
    (service, turn)
}

/// Builds a failed execution record around the supplied actions and results.
fn constructed_execution(
    turn: &AgentTurnRecord,
    actions: Vec<AgentAction>,
    action_results: Vec<mez_agent::ActionResult>,
    final_turn: bool,
) -> mez_agent::AgentTurnExecution {
    let terminal_state = turn_state_from_action_results(&action_results, final_turn);
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "deepseek".to_string(),
            model: "test".to_string(),
            raw_text: "constructed mixed batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "constructed mixed batch".to_string(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results,
        final_turn,
        terminal_state,
    }
}
