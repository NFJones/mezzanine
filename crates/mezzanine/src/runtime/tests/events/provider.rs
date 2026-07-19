//! Runtime tests for events provider behavior.

use super::*;

/// Verifies provider execution identity is idempotent for exact replay while
/// preserving textually identical responses reached from different requests.
///
/// Response text is not a valid event identity. The consumed request
/// chronology distinguishes later executions, while replay of the same
/// request/response pair must not append a duplicate assistant event.
#[test]
fn runtime_provider_execution_identity_is_request_scoped_and_idempotent() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "preserve identical response events")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let response = mez_agent::ModelResponse {
        provider: "openai".to_string(),
        model: "test".to_string(),
        raw_text: "same assistant response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Vec::new(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };
    let first = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: response.clone(),
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    service
        .append_agent_execution_chronology(&turn, &first)
        .unwrap();
    service
        .append_agent_execution_chronology(&turn, &first)
        .unwrap();

    let mut second = first.clone();
    second.request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Context,
        source: ContextSourceKind::CommittedEvidence,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "[new settled evidence]\nrequest chronology advanced".to_string(),
    });
    service
        .append_agent_execution_chronology(&turn, &second)
        .unwrap();

    let assistant_events = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .collect::<Vec<_>>();
    assert_eq!(assistant_events.len(), 2);
    assert_eq!(
        assistant_events[0].block().content,
        "same assistant response"
    );
    assert_eq!(
        assistant_events[1].block().content,
        "same assistant response"
    );
    assert_ne!(
        assistant_events[0].execution_group_id(),
        assistant_events[1].execution_group_id()
    );
}

/// Verifies response-local MAAP ordinals become distinct causal action ids
/// across consecutive provider completions in one logical turn.
///
/// Every real provider response is parsed independently and therefore starts
/// its local numbering at `action-1`. The first continuation below owns one
/// action, the second owns a mixed `action-1`/`action-2` batch, and the third
/// owns one `action-1`. This covers both the partial-registration failure and
/// its single-id stable-owner mismatch sibling while proving late results still
/// settle under the assistant execution that produced them.
#[test]
fn runtime_provider_action_ids_are_scoped_to_each_execution() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "continue through repeated local action ids")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();

    let build_execution = |sequence: usize, action_count: usize| {
        let actions = (1..=action_count)
            .map(|ordinal| mez_agent::AgentAction {
                id: format!("action-{ordinal}"),
                rationale: format!("execution {sequence} action {ordinal}"),
                payload: mez_agent::AgentActionPayload::IssueQuery {
                    kind: None,
                    state: Some("open".to_string()),
                    text: Some(format!("execution-{sequence}-action-{ordinal}")),
                    limit: Some(1),
                    refresh: false,
                },
            })
            .collect::<Vec<_>>();
        let action_results = actions
            .iter()
            .map(|action| {
                mez_agent::ActionResult::succeeded(
                    &turn,
                    action,
                    vec![format!("execution {sequence} result")],
                    Some(
                        serde_json::json!({
                            "action_id": action.id,
                            "nested": {"original_action_id": action.id},
                        })
                        .to_string(),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let mut request = runtime_model_request_fixture(&turn.turn_id);
        request.messages.push(mez_agent::ModelMessage {
            role: mez_agent::ModelMessageRole::Context,
            source: ContextSourceKind::CommittedEvidence,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            content: format!("[settled execution {sequence}]\ncontinue"),
        });
        mez_agent::AgentTurnExecution {
            request,
            response: mez_agent::ModelResponse {
                provider: "openai".to_string(),
                model: "test".to_string(),
                raw_text: format!("provider execution {sequence}"),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: format!("provider execution {sequence}"),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions,
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results,
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        }
    };

    let mut first = build_execution(1, 1);
    service
        .scope_provider_execution_action_ids(&turn, &mut first)
        .unwrap();
    let scoped_first_replay = first.clone();
    service
        .scope_provider_execution_action_ids(&turn, &mut first)
        .unwrap();
    assert_eq!(first, scoped_first_replay);
    let first_id = first.response.action_batch.as_ref().unwrap().actions[0]
        .id
        .clone();
    assert!(first_id.starts_with("action-1~mez~"));
    assert_eq!(first.action_results[0].action_id, first_id);
    let first_structured: serde_json::Value = serde_json::from_str(
        first.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first_structured["action_id"], first_id);
    assert_eq!(first_structured["nested"]["original_action_id"], first_id);
    service
        .append_agent_execution_chronology(&turn, &first)
        .unwrap();

    let mut second = build_execution(2, 2);
    service
        .scope_provider_execution_action_ids(&turn, &mut second)
        .unwrap();
    let second_ids = second
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect::<Vec<_>>();
    assert_ne!(second_ids[0], first_id);
    service
        .append_agent_execution_chronology(&turn, &second)
        .unwrap();

    let mut third = build_execution(3, 1);
    service
        .scope_provider_execution_action_ids(&turn, &mut third)
        .unwrap();
    let third_id = third.response.action_batch.as_ref().unwrap().actions[0]
        .id
        .clone();
    assert_ne!(third_id, first_id);
    assert_ne!(third_id, second_ids[0]);
    service
        .append_agent_execution_chronology(&turn, &third)
        .unwrap();

    let mut settled_results = first.action_results.clone();
    settled_results.extend(second.action_results.clone());
    settled_results.extend(third.action_results.clone());
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn.turn_id, &settled_results)
            .unwrap(),
        4
    );

    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    for (execution, scoped_ids) in [
        (&first, vec![first_id]),
        (&second, second_ids),
        (&third, vec![third_id]),
    ] {
        let rationale = execution
            .response
            .action_batch
            .as_ref()
            .unwrap()
            .rationale
            .as_str();
        let assistant = context
            .chronology()
            .iter()
            .find(|event| {
                event.block().source == ContextSourceKind::TranscriptAssistant
                    && event.block().content.contains(rationale)
            })
            .unwrap();
        for scoped_id in scoped_ids {
            let result = context
                .chronology()
                .iter()
                .find(|event| event.block().label == format!("action result {scoped_id}"))
                .unwrap();
            assert_eq!(result.execution_group_id(), assistant.execution_group_id());
        }
    }
}

/// Verifies asynchronous provider-completion ingress scopes repeated
/// response-local action ids before it registers causal ownership.
///
/// The first progress response settles and queues continuation in the same
/// logical turn. Applying another response whose parser-local id is again
/// `action-1` must retain two distinct assistant/result groups without exposing
/// either ownership validator error.
#[tokio::test]
async fn runtime_provider_application_accepts_reused_local_action_id_on_continuation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "apply two continued provider responses")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let build_execution = |sequence: usize| {
        let action = mez_agent::AgentAction {
            id: "action-1".to_string(),
            rationale: format!("report provider progress {sequence}"),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: format!("provider progress {sequence}"),
                content_type: "text/plain; charset=utf-8".to_string(),
            },
        };
        let mut request = runtime_model_request_fixture(&turn.turn_id);
        request.messages.push(mez_agent::ModelMessage {
            role: mez_agent::ModelMessageRole::Context,
            source: ContextSourceKind::CommittedEvidence,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            content: format!("[continued provider request {sequence}]\ncontinue"),
        });
        mez_agent::AgentTurnExecution {
            request,
            response: mez_agent::ModelResponse {
                provider: "openai".to_string(),
                model: "test".to_string(),
                raw_text: format!("provider progress response {sequence}"),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: format!("provider progress execution {sequence}"),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                &turn,
                &action,
                vec![format!("provider progress {sequence}")],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        }
    };
    let agent_id = AgentId::opaque(turn.agent_id.clone()).unwrap();
    assert!(
        service
            .apply_agent_provider_completed_event(&agent_id, &turn.turn_id, build_execution(1))
            .await
            .unwrap()
    );
    let first = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    assert!(
        service
            .apply_agent_provider_completed_event(&agent_id, &turn.turn_id, build_execution(2))
            .await
            .unwrap()
    );
    let second = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    let first_id = &first.response.action_batch.as_ref().unwrap().actions[0].id;
    let second_id = &second.response.action_batch.as_ref().unwrap().actions[0].id;
    assert!(first_id.starts_with("action-1~mez~"));
    assert!(second_id.starts_with("action-1~mez~"));
    assert_ne!(first_id, second_id);

    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    for (sequence, action_id) in [(1, first_id), (2, second_id)] {
        let assistant = context
            .chronology()
            .iter()
            .find(|event| {
                event.block().source == ContextSourceKind::TranscriptAssistant
                    && event
                        .block()
                        .content
                        .contains(&format!("provider progress execution {sequence}"))
            })
            .unwrap();
        let result = context
            .chronology()
            .iter()
            .find(|event| event.block().label == format!("action result {action_id}"))
            .unwrap();
        assert_eq!(result.execution_group_id(), assistant.execution_group_id());
    }
}

/// Verifies a late result keeps its original execution owner even after
/// steering and another assistant response advance chronology.
///
/// The same execution group also owns the provider-neutral assistant record,
/// DeepSeek-native tool-call pair, and generic action result. This lets the
/// provider adapter select one representation without losing causal identity.
#[test]
fn runtime_late_result_retains_original_provider_execution_group() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect one issue")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let first_action = mez_agent::AgentAction {
        id: "query-original".to_string(),
        rationale: "Load the selected issue evidence".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: Some("open".to_string()),
            text: Some("selected".to_string()),
            limit: Some(10),
            refresh: false,
        },
    };
    let first = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "deepseek".to_string(),
            model: "test".to_string(),
            raw_text: "executing".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Continue active issue iss-42".to_string(),
                thought: Some("Active issue: iss-42".to_string()),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![first_action.clone()],
                final_turn: false,
            }),
            provider_transcript_events: vec![
                mez_agent::ProviderTranscriptEvent::DeepSeekAssistantToolCall {
                    content: String::new(),
                    reasoning_content: Some("Continue active issue iss-42".to_string()),
                    tool_calls: vec![serde_json::json!({
                        "id": "call-original",
                        "type": "function",
                        "function": {
                            "name": "submit_maap_action_batch",
                            "arguments": "{}"
                        }
                    })],
                },
            ],
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .append_agent_execution_chronology(&turn, &first)
        .unwrap();

    service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_user_event("steering", "also retain causal ownership")
        .unwrap();

    let second_action = mez_agent::AgentAction {
        id: "query-later".to_string(),
        rationale: "Record a later assistant execution".to_string(),
        payload: mez_agent::AgentActionPayload::IssueQuery {
            kind: None,
            state: Some("open".to_string()),
            text: Some("later".to_string()),
            limit: Some(10),
            refresh: false,
        },
    };
    let mut second_request = runtime_model_request_fixture(&turn.turn_id);
    second_request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::User,
        source: ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "also retain causal ownership".to_string(),
    });
    let second = mez_agent::AgentTurnExecution {
        request: second_request,
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "later response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Handle later evidence".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![second_action],
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
    service
        .append_agent_execution_chronology(&turn, &second)
        .unwrap();
    service
        .commit_settled_action_results_context(
            &turn.turn_id,
            &[mez_agent::ActionResult::succeeded(
                &turn,
                &first_action,
                vec!["original result".to_string()],
                None,
            )],
        )
        .unwrap();

    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let original_assistant = context
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::TranscriptAssistant
                && event.block().content.contains("Active issue: iss-42")
        })
        .unwrap();
    let later_assistant = context
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::TranscriptAssistant
                && event.block().content.contains("Handle later evidence")
        })
        .unwrap();
    let original_result = context
        .chronology()
        .iter()
        .find(|event| event.block().label == "action result query-original")
        .unwrap();
    assert_eq!(
        original_result.execution_group_id(),
        original_assistant.execution_group_id()
    );
    assert_ne!(
        original_result.execution_group_id(),
        later_assistant.execution_group_id()
    );
    let provider_events = context
        .chronology()
        .iter()
        .filter(|event| {
            mez_agent::ProviderTranscriptEvent::from_transcript_content(&event.block().content)
                .is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(provider_events.len(), 2);
    assert!(
        provider_events
            .iter()
            .all(|event| { event.execution_group_id() == original_assistant.execution_group_id() })
    );
    assert!(
        context
            .chronology()
            .iter()
            .position(|event| event.block().label == "action result query-original")
            .unwrap()
            > context
                .chronology()
                .iter()
                .position(|event| event.block().content.contains("Handle later evidence"))
                .unwrap()
    );
}

/// Verifies actor-accepted steering invalidates every older in-flight provider
/// decision instead of reordering it around the newer user event.
///
/// The stale response must never enter canonical chronology or reach action
/// dispatch. Completion ingress queues a fresh provider request whose snapshot
/// includes the steering event.
#[tokio::test]
async fn runtime_provider_completion_discards_generation_older_than_steering() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the parser")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let consumed_high_water_mark = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests(&turn.turn_id, consumed_high_water_mark)
        .unwrap();

    assert_eq!(
        service
            .inject_agent_steering_for_running_turn("%1", "stop and review ordering")
            .unwrap(),
        Some(turn.turn_id.clone())
    );

    let response = runtime_say_response(&turn.turn_id, "stale provider answer", true);
    let action = response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .first()
        .unwrap()
        .clone();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response,
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["stale provider answer".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    assert!(
        service
            .apply_agent_provider_completed_event(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
                execution,
            )
            .await
            .unwrap()
    );

    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::UserInstruction
            && block.content == "stop and review ordering"
    }));
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("stale provider answer"))
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert!(!service.agent_turn_executions().contains_key(&turn.turn_id));

    let macro_consumed_high_water_mark = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests(
            &turn.turn_id,
            macro_consumed_high_water_mark,
        )
        .unwrap();
    service
        .inject_agent_steering_for_running_turn("%1", "also stop the macro judge")
        .unwrap();
    let macro_response = runtime_say_response(&turn.turn_id, "stale macro decision", true);
    let macro_action = macro_response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .first()
        .unwrap()
        .clone();
    let mut macro_request = runtime_model_request_fixture(&turn.turn_id);
    macro_request.interaction_kind = mez_agent::ModelInteractionKind::MacroJudge;
    let macro_execution = mez_agent::AgentTurnExecution {
        request: macro_request,
        response: macro_response,
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &macro_action,
            vec!["stale macro decision".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    assert!(
        service
            .apply_agent_provider_completed_event(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
                macro_execution,
            )
            .await
            .unwrap()
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .all(|block| !block.content.contains("stale macro decision"))
    );
}

/// Verifies steering accepted after an action request does not suppress or
/// backdate the later result of work that already executed.
///
/// The action and result keep one causal owner, while actor chronology remains
/// `assistant action -> steering -> result`. This is the straddling-group case
/// the compactor must preserve raw rather than gather across the user barrier.
#[test]
fn runtime_steering_during_executed_action_preserves_later_evidence_in_place() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "run the long action")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let group = mez_agent::ContextExecutionGroupId::new("executed-before-steering").unwrap();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_assistant_event(
            "assistant response for executed action",
            "dispatch shell action already in progress",
            group.clone(),
        )
        .unwrap();
    assert_eq!(
        service
            .inject_agent_steering_for_running_turn(
                "%1",
                "do not start anything else; retain this result",
            )
            .unwrap(),
        Some(turn.turn_id.clone())
    );
    let action = mez_agent::AgentAction {
        id: "executed-action".to_string(),
        rationale: "finish work already dispatched".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "finish dispatched action".to_string(),
            command: "printf completed".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let result = mez_agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["completed after steering".to_string()],
        None,
    );
    service
        .commit_settled_action_results_context(&turn.turn_id, &[result])
        .unwrap();

    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let events = canonical_event_oracle(context);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    let action_index = events
        .iter()
        .position(|event| event.label == "assistant response for executed action")
        .unwrap();
    let steering_index = events
        .iter()
        .position(|event| event.label.starts_with("user steering"))
        .unwrap();
    let result_index = events
        .iter()
        .position(|event| event.label == "action result executed-action")
        .unwrap();
    assert!(action_index < steering_index && steering_index < result_index);
    assert_eq!(
        events[action_index].execution_group_id.as_deref(),
        Some(group.as_str())
    );
    assert_eq!(
        events[result_index].execution_group_id.as_deref(),
        Some(group.as_str())
    );
    assert_eq!(events[steering_index].execution_group_id, None);
    assert_eq!(
        events[steering_index].retention,
        mez_agent::ContextRetention::Exact
    );
}

/// Verifies repeated steering cannot gather, suppress, or reassign terminal
/// evidence from multiple actions already owned by one assistant execution.
///
/// A failure observed after the first steering event and a cancellation
/// observed after the second remain at their actor acceptance positions. Both
/// results retain the original execution group, while each steering event is
/// an exact unowned barrier between the surrounding causal records.
#[test]
fn runtime_multiple_steering_events_preserve_failure_and_cancellation_order() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "run both long actions")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let group = mez_agent::ContextExecutionGroupId::new("multi-steering-actions").unwrap();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_assistant_event(
            "assistant response for two actions",
            "dispatch two actions before steering",
            group.clone(),
        )
        .unwrap();

    let failed_action = mez_agent::AgentAction {
        id: "failed-after-first-steering".to_string(),
        rationale: "observe an already-started failure".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "fail after first steering".to_string(),
            command: "exit 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let cancelled_action = mez_agent::AgentAction {
        id: "cancelled-after-second-steering".to_string(),
        rationale: "observe an explicitly cancelled sibling".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "cancel after second steering".to_string(),
            command: "sleep 10".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };

    service
        .inject_agent_steering_for_running_turn("%1", "retain the first settled result")
        .unwrap();
    let failed_result = mez_agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "test_action_failed",
        "first action failed after steering",
    )
    .unwrap();
    service
        .commit_settled_action_results_context(&turn.turn_id, &[failed_result])
        .unwrap();

    service
        .inject_agent_steering_for_running_turn("%1", "cancel the remaining action")
        .unwrap();
    let cancelled_result = mez_agent::ActionResult::failed(
        &turn,
        &cancelled_action,
        ActionStatus::Cancelled,
        "test_action_cancelled",
        "second action was cancelled after steering",
    )
    .unwrap();
    service
        .commit_settled_action_results_context(&turn.turn_id, &[cancelled_result])
        .unwrap();

    let events = canonical_event_oracle(service.agent_turn_contexts().get(&turn.turn_id).unwrap());
    let labels = events
        .iter()
        .map(|event| event.label.as_str())
        .collect::<Vec<_>>();
    let assistant_index = labels
        .iter()
        .position(|label| *label == "assistant response for two actions")
        .unwrap();
    let steering_indices = labels
        .iter()
        .enumerate()
        .filter_map(|(index, label)| label.starts_with("user steering").then_some(index))
        .collect::<Vec<_>>();
    let failed_index = labels
        .iter()
        .position(|label| *label == "action result failed-after-first-steering")
        .unwrap();
    let cancelled_index = labels
        .iter()
        .position(|label| *label == "action result cancelled-after-second-steering")
        .unwrap();

    assert_eq!(steering_indices.len(), 2);
    assert!(
        assistant_index < steering_indices[0]
            && steering_indices[0] < failed_index
            && failed_index < steering_indices[1]
            && steering_indices[1] < cancelled_index
    );
    assert_eq!(
        events[failed_index].execution_group_id.as_deref(),
        Some(group.as_str())
    );
    assert_eq!(
        events[cancelled_index].execution_group_id.as_deref(),
        Some(group.as_str())
    );
    for steering_index in steering_indices {
        assert_eq!(events[steering_index].execution_group_id, None);
        assert_eq!(
            events[steering_index].retention,
            mez_agent::ContextRetention::Exact
        );
    }
}

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

/// Verifies a routed presentation persists exactly one typed handoff summary
/// before the visible parent answer and rehydrates both on the next turn.
///
/// The exact worker result and presentation instructions are turn-local. Only
/// the summarized handoff belongs in durable context, and repeated lifecycle
/// finalization must not duplicate its reserved transcript event.
#[test]
fn runtime_routed_handoff_summary_persists_once_and_rehydrates_with_parent_answer() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-routed-handoff-durable-context");
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
        .start_agent_prompt_turn("%1", "route this implementation")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.mark_routed_presentation_turn_for_tests(&turn.turn_id);
    let handoff = r#"{"version":1,"result_summary":"implemented durable context","decisions":[],"evidence":["focused test passed"],"changes":[],"validation":[],"assumptions":[],"unresolved_risks":[],"follow_up_context":[]}"#;
    let context = service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap();
    for block in [
        ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "routed worker exact final result".to_string(),
            content: "EXACT WORKER RESULT MUST NOT PERSIST".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "routed worker handoff context".to_string(),
            content: handoff.to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "routed result presentation".to_string(),
            content: "PRESENTATION INSTRUCTION MUST NOT PERSIST".to_string(),
        },
    ] {
        insert_test_context_block(context, block);
    }
    let action = mez_agent::AgentAction {
        id: "present-routed-result".to_string(),
        rationale: "present the routed result".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "The routed implementation is complete.".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "The routed implementation is complete.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "present the routed result".to_string(),
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
            vec!["presented".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let first = service
        .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
        .unwrap();
    let replay = service
        .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
        .unwrap();
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let event_positions = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content)
                .map(|event| (index, event))
        })
        .collect::<Vec<_>>();
    let user_index = entries
        .iter()
        .position(|entry| entry.role == TranscriptRole::User)
        .unwrap();
    let assistant_index = entries
        .iter()
        .position(|entry| entry.role == TranscriptRole::Assistant)
        .unwrap();

    assert!(first > 0);
    assert_eq!(replay, 0);
    assert_eq!(event_positions.len(), 1);
    assert!(user_index < event_positions[0].0);
    assert!(event_positions[0].0 < assistant_index);
    assert_eq!(
        event_positions[0].1,
        mez_agent::TranscriptContextEvent::RoutedHandoff {
            content: handoff.to_string()
        }
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("EXACT WORKER RESULT"))
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("PRESENTATION INSTRUCTION"))
    );

    service
        .complete_running_agent_turn_and_start_ready(
            &turn,
            AgentTurnState::Completed,
            "routed presentation persisted",
        )
        .unwrap();
    let next = service
        .start_agent_prompt_turn("%1", "continue from the routed result")
        .unwrap();
    let next_context = service.agent_turn_contexts().get(&next.turn_id).unwrap();
    assert!(next_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker handoff context"
            && block.content == handoff
    }));
    assert!(next_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block
                .content
                .contains("rationale: present the routed result")
            && block
                .content
                .contains("action rationale present-routed-result (say): present the routed result")
            && block
                .content
                .contains("The routed implementation is complete.")
    }));
    let ordinary_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &next.turn_id,
            &next.agent_id,
            "Ordinary follow-up answer.",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &next.turn_id,
            &ordinary_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let event_count = transcript_store
        .inspect(&conversation_id)
        .unwrap()
        .iter()
        .filter(|entry| {
            mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content).is_some()
        })
        .count();
    assert_eq!(event_count, 1);
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
        placement: mez_agent::ContextPlacement::ConversationAppend,
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
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-ok fetch_url succeeded]")
            && block.content.contains("provider document body")
    }));
    assert!(context.blocks().iter().any(|block| {
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
