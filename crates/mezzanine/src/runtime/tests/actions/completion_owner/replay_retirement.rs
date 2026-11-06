//! Actual actor completion and native continuity over retained replay ownership.
//!
//! The external worker outcomes are inert synthetic values, but admission,
//! source append, claim, publication holds, retirement and provider queueing use
//! the production paths. No HTTP, paid model or native process is executed.

use super::*;
use mez_agent::{AgentContext, ReplayConversationId, ReplayEpochContext};

/// The real completion group's retired receipt must not release a later
/// protection allocation for the same unchanged native source. All original
/// callback/publication gates have succeeded before the explicit renewal.
#[tokio::test]
async fn runtime_completion_replay_retired_receipt_cannot_close_renewed_protection() {
    let mut fixture = fixture().await;
    let retired = settle_with_publication_held(&mut fixture);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    fixture.service.retry_completion_publications();
    let group = mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap();
    let context = fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap();
    assert!(!context.execution_group_is_pending(&group));
    let chronology = context.chronology().to_vec();
    context.retain_pending_execution_group(&group).unwrap();
    assert_eq!(context.chronology(), chronology);
    assert!(
        fixture
            .service
            .retry_completion_replay_retirement_for_tests(&fixture.turn.turn_id)
            .is_err(),
        "retired completion adopted renewed protection"
    );
    assert!(
        fixture
            .service
            .completion_replay_context_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .execution_group_is_pending(&group)
    );
    assert_eq!(
        fixture
            .service
            .completion_replay_owner_for_tests(&fixture.turn.turn_id),
        Some(&retired)
    );
}

/// A completed no-action response has no native/effect retirement obligation.
/// Its actual append closes only that response; a different pending native
/// completion group remains protected and an exact replay appends nothing.
#[tokio::test]
async fn runtime_completion_replay_no_action_response_closes_only_its_source() {
    let mut fixture = fixture().await;
    let mut execution = fixture.retained();
    execution.response.raw_text = "settled plain response".into();
    execution.response.action_batch = None;
    execution.response.provider_transcript_events.clear();
    execution.action_results.clear();
    fixture
        .service
        .append_agent_execution_chronology(&fixture.turn, &execution)
        .unwrap();
    let context = fixture
        .service
        .agent_turn_contexts()
        .get(&fixture.turn.turn_id)
        .unwrap();
    let group = context
        .chronology()
        .last()
        .unwrap()
        .execution_group_id()
        .unwrap()
        .clone();
    assert!(!context.execution_group_is_pending(&group));
    assert!(context.execution_group_is_pending(
        &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
    ));
    let before = context.clone();
    fixture
        .service
        .append_agent_execution_chronology(&fixture.turn, &execution)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_turn_contexts()
            .get(&fixture.turn.turn_id),
        Some(&before)
    );
}

/// A genuine result append against an unexpectedly advanced original group
/// must reject before changing the live body or its retained retirement fence.
/// Unlike the no-op duplicate control, this would otherwise append new source.
#[tokio::test]
async fn runtime_completion_replay_conflicting_append_is_atomic() {
    let mut fixture = fixture().await;
    let original = fixture
        .service
        .completion_replay_owner_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .append_assistant_event(
            "unqualified later source",
            "original fence changed",
            mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap(),
        )
        .unwrap();
    let before = fixture
        .service
        .agent_turn_contexts()
        .get(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    assert!(
        fixture
            .service
            .append_action_result_context_if_absent(&fixture.turn.turn_id, &fixture.expected)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_contexts()
            .get(&fixture.turn.turn_id),
        Some(&before)
    );
    assert_eq!(
        fixture
            .service
            .completion_replay_owner_for_tests(&fixture.turn.turn_id),
        Some(&original)
    );
}

/// Admits native source and the same two real external-action attempt owners
/// as the established completion fixture, with replay ownership selected first.
async fn fixture() -> CompletionFixture {
    Box::pin(fixture_with_final_turn(false)).await
}

/// Selects real terminal versus continuation settlement before source admission.
async fn fixture_with_final_turn(final_turn: bool) -> CompletionFixture {
    let root = CompletionRoot(
        temp_root("replay-completion-source"),
        CompletionRootCleanup::RemoveOwned,
    );
    fs::create_dir(root.0.join("transcripts")).unwrap();
    let mut service = RuntimeServiceFixture::new()
        .control_socket(root.0.join("control.sock"))
        .build();
    service.set_agent_transcript_store(AgentTranscriptStore::new(root.0.join("transcripts")));
    let primary = service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    Box::pin(CompletionFixture::admit_prepared_with_replay(
        service, primary, root, final_turn, false, true,
    ))
    .await
}

/// Terminal completion must enforce the same original source fence before
/// deleting live context. A valid paired group reaches Completed; a later append
/// must retain protected original backing instead of reporting terminal success.
#[tokio::test]
async fn runtime_completion_replay_terminal_retirement_keeps_original_fence() {
    for changed in [false, true] {
        let mut fixture = fixture_with_final_turn(true).await;
        settle_with_publication_held(&mut fixture);
        if changed {
            fixture
                .service
                .agent_turn_contexts_mut()
                .get_mut(&fixture.turn.turn_id)
                .unwrap()
                .append_assistant_event(
                    "late terminal source",
                    "outside original terminal fence",
                    mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap(),
                )
                .unwrap();
        }
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        fixture.service.retry_completion_publications();
        let state = fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state;
        if changed {
            assert_ne!(
                state,
                AgentTurnState::Completed,
                "terminal callback adopted newer source"
            );
            assert!(
                fixture
                    .service
                    .completion_replay_context_for_tests(&fixture.turn.turn_id)
                    .unwrap()
                    .execution_group_is_pending(
                        &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
                    )
            );
        } else {
            assert_eq!(state, AgentTurnState::Completed);
        }
    }
}

/// Settles both actual admitted attempts while keeping final result publication
/// outstanding. Native pairing may close, but retirement must still be pending.
fn settle_with_publication_held(
    fixture: &mut CompletionFixture,
) -> mez_agent::ReplayExecutionOwner {
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    assert!(
        fixture
            .service
            .complete_approved_external_action(fixture.outcome())
            .unwrap()
    );
    let context = fixture
        .service
        .completion_replay_context_for_tests(&fixture.turn.turn_id)
        .unwrap();
    assert!(context.execution_group_is_pending(
        &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
    ));
    assert!(!context.chronology().iter().any(|event| matches!(
        mez_agent::ProviderTranscriptEvent::from_transcript_content(&event.block().content),
        Some(mez_agent::ProviderTranscriptEvent::OpenAiChatCompletionsToolResult { .. })
    )));
    let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    let dispatch = fixture
        .service
        .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
        .unwrap()
        .unwrap();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let result = ActionResult::succeeded(
        &fixture.turn,
        &sibling,
        vec!["owned sibling settled".into()],
        None,
    );
    assert!(
        fixture
            .service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: fixture.turn.turn_id.clone(),
                    action_id: sibling.id,
                    attempt: dispatch.attempt,
                    result: Ok(result),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    let context = fixture
        .service
        .completion_replay_context_for_tests(&fixture.turn.turn_id)
        .unwrap();
    assert!(context.execution_group_is_pending(
        &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
    ));
    assert_eq!(
        context
            .chronology()
            .iter()
            .filter(|event| matches!(
                mez_agent::ProviderTranscriptEvent::from_transcript_content(&event.block().content),
                Some(mez_agent::ProviderTranscriptEvent::OpenAiChatCompletionsToolResult { .. })
            ))
            .count(),
        1
    );
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    fixture
        .service
        .completion_replay_owner_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .clone()
}

/// Actual native result append advances the retained original fence, while
/// publication holds prevent premature release. After the real gates settle,
/// queue admission closes exactly that source and repeated recovery is a no-op.
#[tokio::test]
async fn runtime_completion_replay_retirement_uses_actual_append_fence() {
    let mut fixture = fixture().await;
    let declaration = fixture
        .service
        .completion_replay_owner_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    let completed = settle_with_publication_held(&mut fixture);
    assert!(completed.frontier() > declaration.frontier());
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(fixture.service.retry_completion_publications() > 0);
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    let context = fixture
        .service
        .completion_replay_context_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    assert!(!context.execution_group_is_pending(
        &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
    ));
    let owner = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new(&fixture.turn.conversation_id).unwrap(),
        context.clone(),
    )
    .unwrap();
    assert!(owner.execution_group_finished(&completed));
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(
        fixture
            .service
            .completion_replay_context_for_tests(&fixture.turn.turn_id)
            .unwrap(),
        &context
    );
}

/// An unrelated same-group append invalidates the retained original fence.
/// Re-delivering an already committed action result must not refresh that fence
/// from the newer live body and authorize stale-source retirement.
#[tokio::test]
async fn runtime_completion_replay_duplicate_does_not_adopt_newer_frontier() {
    let mut fixture = fixture().await;
    let original = settle_with_publication_held(&mut fixture);
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .append_assistant_event(
            "later source",
            "outside original completion append",
            mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap(),
        )
        .unwrap();
    fixture
        .service
        .append_action_result_context_if_absent(&fixture.turn.turn_id, &fixture.expected)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .completion_replay_owner_for_tests(&fixture.turn.turn_id),
        Some(&original)
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    fixture.service.retry_completion_publications();
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    assert!(
        fixture
            .service
            .completion_replay_context_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .execution_group_is_pending(
                &mez_agent::ContextExecutionGroupId::new(fixture.group.clone()).unwrap()
            )
    );
}

/// A plain or cold-recreated body under the same public turn/CID cannot replace
/// the original live incarnation. Existing complete native pairs and equal
/// public labels do not authorize the old completion to retire a new owner.
#[tokio::test]
async fn runtime_completion_replay_refuses_replacement_incarnations() {
    for cold in [false, true] {
        let mut fixture = fixture().await;
        settle_with_publication_held(&mut fixture);
        let original = fixture
            .service
            .agent_turn_contexts()
            .get(&fixture.turn.turn_id)
            .unwrap()
            .clone();
        let replacement = if cold {
            let owner = ReplayEpochContext::from_owned_context(
                ReplayConversationId::new(&fixture.turn.conversation_id).unwrap(),
                original,
            )
            .unwrap();
            ReplayEpochContext::from_checkpoint(
                ReplayConversationId::new(&fixture.turn.conversation_id).unwrap(),
                owner.checkpoint().unwrap(),
                AgentContext::empty(),
            )
            .unwrap()
            .into_owned_context()
        } else {
            AgentContext::import_durable_blocks(original.blocks().to_vec()).unwrap()
        };
        fixture
            .service
            .agent_turn_contexts_mut()
            .insert(fixture.turn.turn_id.clone(), replacement);
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        fixture.service.retry_completion_publications();
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert!(
            fixture
                .service
                .completion_replay_owner_for_tests(&fixture.turn.turn_id)
                .is_some()
        );
    }
}
