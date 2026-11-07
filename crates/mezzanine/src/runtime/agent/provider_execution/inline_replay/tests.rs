//! Real prompt and result append boundaries produce each retirement fence.
//! Synthetic source mutations exercise late-arrival/re-protection refusal;
//! they never stand in for completion or asynchronous effect settlement.

use super::*;
use crate::test_support::{replay_epoch::Fixture, runtime::RuntimeServiceFixture};
use mez_agent::{ActionResult, AgentAction, ContextBlockOccurrenceId, ModelProfile};

fn source() -> (
    Fixture,
    RuntimeSessionService,
    AgentTurnRecord,
    AgentTurnExecution,
    usize,
) {
    let fixture = Fixture::new();
    let mut service = RuntimeServiceFixture::new()
        .control_socket(fixture.root.join("inline.sock"))
        .build();
    service
        .attach_primary(
            "inline",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let conversation = String::from(ContextBlockOccurrenceId::allocate());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage("%1", &conversation, 0, None)
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "original inline task")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let profile = ModelProfile {
        provider: "runtime-batch".into(),
        model: "test".into(),
        ..Default::default()
    };
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &service.agent_turn_contexts()[&turn.turn_id],
    )
    .unwrap();
    let action = AgentAction {
        id: "inline-say".into(),
        payload: AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "local response".into(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
        },
    };
    let result = ActionResult::succeeded(&turn, &action, Vec::new(), None);
    let execution = AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: profile.provider,
            model: profile.model,
            raw_text: "local response".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            provider_transcript_events: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "local output".into(),
                actions: vec![action],
            }),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .append_agent_execution_chronology(&turn, &execution)
        .unwrap();
    let committed = service
        .commit_settled_action_results_context(&turn.turn_id, &execution.action_results)
        .unwrap();
    assert!(committed > 0);
    (fixture, service, turn, execution, committed)
}

#[test]
fn inline_say_replay_requires_new_successful_original_results() {
    let (_fixture, service, turn, execution, committed) = source();
    let group = provider_log_execution_group_id(&execution).unwrap();
    let context = &service.agent_turn_contexts()[&turn.turn_id];
    let original = context.clone();
    assert!(context.execution_group_is_pending(&group));
    assert!(
        service
            .capture_settled_inline_say_replay(&turn, &execution, committed)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .capture_settled_inline_say_replay(&turn, &execution, 0)
            .unwrap()
            .is_none()
    );
    let mut candidates = Vec::new();
    let mut running = execution.clone();
    running.action_results[0].status = ActionStatus::Running;
    candidates.push(running);
    let mut failed = execution.clone();
    failed.terminal_state = AgentTurnState::Failed;
    candidates.push(failed);
    let mut foreign = execution.clone();
    foreign.request.turn_id = "other-turn".into();
    candidates.push(foreign);
    let mut unrelated = execution.clone();
    unrelated.action_results[0].action_type = "send_message";
    candidates.push(unrelated);
    let mut mixed = execution.clone();
    mixed.response.action_batch.as_mut().unwrap().actions[0].payload =
        AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        };
    candidates.push(mixed);
    let mut duplicate = execution.clone();
    let original_action = duplicate.response.action_batch.as_ref().unwrap().actions[0].clone();
    duplicate
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions
        .push(original_action);
    duplicate
        .action_results
        .push(execution.action_results[0].clone());
    candidates.push(duplicate);
    for candidate in candidates {
        assert!(
            service
                .capture_settled_inline_say_replay(&turn, &candidate, committed)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
}

#[test]
fn inline_say_replay_stale_frontier_and_renewed_interval_remain_protected() {
    for new_source in [false, true] {
        let (_fixture, mut service, turn, execution, committed) = source();
        let group = provider_log_execution_group_id(&execution).unwrap();
        let receipt = service
            .capture_settled_inline_say_replay(&turn, &execution, committed)
            .unwrap()
            .unwrap();
        if new_source {
            service
                .agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .unwrap()
                .append_evidence_event(
                    mez_agent::ContextSourceKind::CommittedEvidence,
                    "later original observation",
                    "new source after captured result",
                    group.clone(),
                    None,
                    true,
                )
                .unwrap();
        } else {
            service
                .present_settled_inline_say_execution(&turn, &execution, Some(&receipt))
                .unwrap();
            let context = service
                .agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .unwrap();
            assert!(!context.execution_group_is_pending(&group));
            context.retain_pending_execution_group(&group).unwrap();
        }
        assert!(
            service
                .present_settled_inline_say_execution(&turn, &execution, Some(&receipt))
                .is_err()
        );
        assert!(service.agent_turn_contexts()[&turn.turn_id].execution_group_is_pending(&group));
        assert!(
            service
                .capture_settled_inline_say_replay(&turn, &execution, 0)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn inline_say_replay_changed_conversation_refuses_presentation_and_retirement() {
    let (_fixture, mut service, turn, execution, committed) = source();
    let receipt = service
        .capture_settled_inline_say_replay(&turn, &execution, committed)
        .unwrap()
        .unwrap();
    let original = service.agent_turn_contexts()[&turn.turn_id].clone();
    let replacement = String::from(ContextBlockOccurrenceId::allocate());
    // The real shell guard requires releasing its running selection before a
    // rebind. Keep the old canonical body to exercise its stale presentation
    // callback; this grants it no completion or replacement-conversation rights.
    service
        .agent_shell_store_mut()
        .finish_turn("%1", &turn.turn_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage("%1", &replacement, 0, None)
        .unwrap();
    assert!(
        service
            .present_settled_inline_say_execution(&turn, &execution, Some(&receipt))
            .is_err()
    );
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
}

#[test]
fn inline_say_replay_does_not_bypass_an_existing_completion_owner() {
    let (_fixture, mut service, turn, execution, committed) = source();
    let group = provider_log_execution_group_id(&execution).unwrap();
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    // Reserve the actual original group through its production owner. No
    // worker claim, publication milestone, or closure is synthesized.
    service
        .reserve_completion_group(&turn.turn_id, &group)
        .unwrap();
    assert!(
        service
            .capture_settled_inline_say_replay(&turn, &execution, committed)
            .unwrap()
            .is_none()
    );
    assert!(service.agent_turn_contexts()[&turn.turn_id].execution_group_is_pending(&group));
}
