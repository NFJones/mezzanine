//! Complete offline joined-child clarification through production runtime APIs.
//!
//! The child must obtain its parent's answer before completing. One scheduler
//! slot proves logical dependency liveness rather than spare execution capacity.

use super::*;

/// Owns every pane process created by a dialogue fixture, including on panic.
struct DialogueRuntime(RuntimeSessionService);

impl std::ops::Deref for DialogueRuntime {
    type Target = RuntimeSessionService;
    /// Borrows the real runtime without changing its service behavior.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for DialogueRuntime {
    /// Borrows the real actor-owned state for the synchronous offline fixture.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for DialogueRuntime {
    /// Retires this fixture's own pane resources on success and unwinding.
    fn drop(&mut self) {
        let _ = self.0.terminate_all_pane_processes();
    }
}

/// Produces one offline model response with the caller's explicit actions.
fn dialogue_provider(actions: Vec<mez_agent::AgentAction>) -> RuntimeBatchProvider {
    RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "offline joined dialogue".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "coordinate the same outstanding joined child".into(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    }
}

/// Sends authenticated session mail through the actual action dispatcher.
fn dialogue_send(id: &str, recipient: &str, payload: &str) -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: id.into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            expect_reply: false,
            recipient: format!("agent:{recipient}"),
            scope: Some("session".into()),
            content_type: "text/plain; charset=utf-8".into(),
            payload: payload.into(),
            correlation_id: None,
        },
    }
}

/// Starts real joined children at one slot with original-target persistence.
/// Native roots are validated before queued child inference can acquire.
fn dialogue_fixture(
    count: usize,
) -> (
    DialogueRuntime,
    mez_agent::AgentTurnRecord,
    Vec<mez_agent::AgentTurnRecord>,
    mez_agent::AgentTurnExecution,
) {
    let (service, parent, children, execution, _) =
        dialogue_fixture_with_actions(count, Vec::new());
    (service, parent, children, execution)
}

/// Adds genuinely dispatched sibling work and exposes the attached primary for
/// lifecycle tests. No completion receipt or scheduler ownership is fabricated.
fn dialogue_fixture_with_actions(
    count: usize,
    siblings: Vec<mez_agent::AgentAction>,
) -> (
    DialogueRuntime,
    mez_agent::AgentTurnRecord,
    Vec<mez_agent::AgentTurnRecord>,
    mez_agent::AgentTurnExecution,
    mez_core::ids::ClientId,
) {
    let mut service = DialogueRuntime(test_runtime_service());
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        temp_root("joined-dialogue-matrix"),
    ));
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("dialogue-matrix", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let started = service
        .start_agent_prompt_turn("%1", "delegate and answer necessary questions")
        .unwrap();
    let provider = dialogue_provider(
        (0..count)
            .map(|index| {
                runtime_spawn_agent_action(
                    &format!("matrix-spawn-{index}"),
                    "ask the parent then wait for an answer",
                )
            })
            .chain(siblings)
            .collect(),
    );
    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.status == ActionStatus::Running)
    );
    let parent = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let children = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.turn_id != parent.turn_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(children.len(), count);
    for child in &children {
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.set_pane_approval_policy_override(
            &child.pane_id,
            Some(mez_agent::ApprovalPolicy::AutoAllow),
        );
    }
    service.start_ready_agent_turns().unwrap();
    (service, parent, children, execution, primary)
}

/// Executes one real response without selecting an unrelated newly eligible task.
fn dialogue_step(
    service: &mut RuntimeSessionService,
    turn: &str,
    provider: RuntimeBatchProvider,
) -> mez_agent::AgentTurnExecution {
    service
        .execute_agent_turn_with_provider(
            turn,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap()
}

/// Sends a real question followed by its own MAAP-valid Wait batch.
fn dialogue_question_and_wait(
    service: &mut RuntimeSessionService,
    child: &mez_agent::AgentTurnRecord,
    parent: &mez_agent::AgentTurnRecord,
) {
    dialogue_step(
        service,
        &child.turn_id,
        dialogue_provider(vec![dialogue_send(
            &format!("question-{}", child.turn_id),
            &parent.agent_id,
            "MATRIX_QUESTION",
        )]),
    );
    let wait = dialogue_step(
        service,
        &child.turn_id,
        dialogue_provider(vec![mez_agent::AgentAction {
            id: format!("wait-{}", child.turn_id),
            payload: mez_agent::AgentActionPayload::Wait {
                for_message: None,
                from_agent: None,
            },
        }]),
    );
    assert_eq!(wait.action_results[0].status, ActionStatus::Running);
}

/// Completes an actual child task without synthesizing terminal controller facts.
fn dialogue_child_done(
    service: &mut RuntimeSessionService,
    child: &mez_agent::AgentTurnRecord,
) -> mez_agent::AgentTurnExecution {
    dialogue_step(
        service,
        &child.turn_id,
        RuntimeBatchProvider {
            response: runtime_say_response_for_agent(
                &child.turn_id,
                &child.agent_id,
                &format!("MATRIX_RESULT_{}", child.turn_id),
                true,
            ),
        },
    )
}

mod basic;
mod continuation;
mod effects;
mod lifecycle;
