//! Owned persistent-child failure regressions through product lifecycle owners.
//!
//! No child model send is fabricated: the runtime's provider failure settlement
//! must preserve local failure evidence and resume only its owning parent's wait.

use super::*;

/// Builds a genuine persistent child with an initial task and parks its parent
/// in an explicit peer wait. Both actors use the runtime's normal spawn and
/// provider execution boundaries, not a forged task-result envelope.
fn owned_child_wait_fixture() -> (RuntimeSessionService, AgentTurnRecord, AgentTurnRecord) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-owned-child-failure-wake",
    )));
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service
        .start_agent_prompt_turn("%1", "ask owned reviewer for explicit MMP answer")
        .unwrap();
    let parent = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let mut spawn =
        runtime_spawn_agent_action("owned-reviewer", "review and send an explicit MMP answer");
    let mez_agent::AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut spawn.payload
    else {
        unreachable!()
    };
    *lifetime = mez_agent::SubagentLifetime::Persistent;
    *objective = Some("Independently review assigned work".into());
    let spawned = service
        .execute_spawn_action_for_turn(&parent, &spawn)
        .unwrap();
    assert_eq!(spawned.status, ActionStatus::Succeeded);
    let spawned: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child_id = spawned["spawn"]["agent"]["id"].as_str().unwrap();
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child_id)
        .unwrap()
        .clone();
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "wait for owned reviewer".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "explicit review answer is outstanding".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "wait-owned-reviewer".into(),
                    payload: mez_agent::AgentActionPayload::Wait,
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    (service, parent, child)
}

/// A persistent reviewer can fail in its provider boundary before producing
/// any model-authored MMP answer. Its exact failure must enter parent context
/// before one fair same-turn continuation settles the retained wait; repeated
/// lifecycle fallback and delivery sweeps must not create a second continuation.
#[test]
fn runtime_owned_child_provider_failure_resumes_parent_wait_without_model_send() {
    let (mut service, parent, child) = owned_child_wait_fixture();
    let turn_count = service.agent_turn_ledger().turns().len();
    service
        .fail_agent_turn_for_provider_error(
            &child,
            "runtime-batch",
            &runtime_model_profile("runtime-batch", "test"),
            &MezError::invalid_state("offline child provider failed before review answer"),
        )
        .unwrap();
    let context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert!(
        context.blocks().iter().any(|block| block
            .content
            .contains("offline child provider failed before review answer")
            && block.content.contains(&child.turn_id)),
        "exact child failure must precede wake"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let execution = service
        .agent_turn_executions()
        .get(&parent.turn_id)
        .unwrap();
    assert_eq!(
        execution
            .action_results
            .iter()
            .filter(
                |result| result.action_type == "wait" && result.status == ActionStatus::Succeeded
            )
            .count(),
        1
    );
    service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Failed)
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(service.agent_peer_message_turn_count(&parent.agent_id), 0);
    service.terminate_all_pane_processes().unwrap();
}
