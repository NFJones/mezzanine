//! Observed-input fixtures backed by actual prior task execution and admission.
//! Auxiliary transport and output tests share this production source rather
//! than manufacturing lifecycle closure for a legacy transcript layout.

use super::*;

/// A legacy planner proposal cannot replace the canonical source lost at an
/// active-turn boundary. Both pressure triggers must fail before model work,
/// without publishing a head or changing the original physical archive.
#[test]
fn runtime_active_compaction_rejects_unowned_source_before_model_queue() {
    use crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger;

    for trigger in [
        RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 40_000,
            max_input_tokens: 40_000,
        },
        RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { attempt: 1 },
    ] {
        let (mut service, store, turn) = queued();
        // A descriptive legacy body and its own valid advisory plan carry no
        // task-completion or original replay-owner authority.
        let unowned = mez_agent::AgentContext::new(vec![
            ContextBlock::assistant_event("legacy answer", "legacy source ".repeat(5_000)),
            ContextBlock::user_event("user prompt", "current protected instruction"),
        ])
        .unwrap();
        let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
            &unowned,
            2_000,
            0,
            unowned.event_sequence_high_water_mark(),
            mez_agent::ProviderBudgetProjection::new(
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
                "runtime-batch",
            ),
        )
        .unwrap();
        assert!(plan.changes_context());
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        service.cancel_current_agent_compaction_task("%1");
        assert!(!unowned.has_replay_owner());
        service
            .agent_turn_contexts_mut()
            .insert(turn.clone(), unowned.clone());
        let rows = store.inspect(&task.conversation_id).unwrap();
        let result = service.queue_agent_active_turn_compaction(
            &turn,
            task.model_profile_name,
            task.model_profile,
            trigger,
            plan,
        );
        assert!(result.is_err(), "unowned source admitted: {result:?}");
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_tasks().is_empty());
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        assert_eq!(service.agent_turn_contexts()[&turn], unowned);
        assert_eq!(store.inspect(&task.conversation_id).unwrap(), rows);
        assert!(
            store
                .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
}

/// A claimed task that loses either canonical source or its original storage
/// owner cannot use transport backoff, input splitting or malformed-output
/// correction to create another inference request. These are faults after
/// genuine source admission, not permission to restore a historical writer.
#[test]
fn runtime_active_compaction_rejects_owner_loss_before_recovery_queue() {
    for missing_replay_source in [false, true] {
        for (kind, message, failure) in [
            ("io", "connection reset", None),
            (
                "invalid_state",
                "provider context length exceeded",
                Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
            ),
            (
                "invalid_state",
                "malformed completed output",
                Some(r#"{"type":"malformed_model_output"}"#),
            ),
        ] {
            let (mut service, store, turn) = queued();
            let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
            let conversation = task.conversation_id.clone();
            let archive = store.inspect(&conversation).unwrap();
            if missing_replay_source {
                task.replay_source = None;
            } else {
                task.source_owner = None;
            }
            service.claim_agent_compaction_task_state("%1", task);
            assert!(
                service
                    .apply_agent_compaction_failed_event("%1", kind, message, failure)
                    .unwrap()
            );
            assert!(
                service.pending_agent_compaction_tasks().is_empty(),
                "owner loss queued another auxiliary request: replay={missing_replay_source} failure={message}"
            );
            assert!(
                !service.agent_is_compacting("%1"),
                "stranded marker: replay={missing_replay_source} failure={message} pending={:?} claimed={:?}",
                service.pending_agent_compaction_task_generation("%1"),
                service.claimed_agent_compaction_task_generation("%1")
            );
            assert!(!service.agent_provider_task_is_pending(&turn));
            assert_eq!(
                service.agent_turn_ledger().turn(&turn).unwrap().state,
                AgentTurnState::Failed
            );
            assert!(service.take_replay_epoch_attempt_for_tests().is_none());
            assert!(
                store
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
            assert!(store.inspect(&conversation).unwrap().starts_with(&archive));
        }
    }
}

/// Queues one observed-input rewrite of a real completed task. Its successor's
/// active instruction and all retained occurrences keep their original owners.
pub(super) fn queued() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    queued_with_service(test_runtime_service())
}

/// Lifecycle tests start the real pane process before source capture. Starting
/// it later changes the process incarnation and must invalidate frozen work.
pub(super) fn queued_with_service(
    service: RuntimeSessionService,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, turn) = crate::test_support::compaction_source::recovery_source(
        service,
        temp_root("canonical-observed-recovery"),
        &format!("ORIGINAL_OBSERVED_TASK {}", "source ".repeat(5_000)),
        "the original task completed",
        10,
    );
    let context = &service.agent_turn_contexts()[&turn];
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        40_000,
        1,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    assert!(service.queue_agent_active_turn_compaction(
        &turn, "provider-context-limit-test".into(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 40_000, max_input_tokens: 40_000,
        }, plan,
    ).unwrap());
    (service, store, turn)
}

/// Executes the original queued persistence attempt and applies its receipt.
/// Inference alone cannot wake the ordinary provider or mutate live context.
pub(super) fn adopt(service: &mut RuntimeSessionService, turn: &str) {
    let before = service.agent_turn_contexts()[turn].clone();
    assert_dispatch_waits_for_adoption(service, turn);
    let completion = replay_publication::execute_pending_publication(service);
    assert_eq!(service.agent_turn_contexts()[turn], before);
    assert_dispatch_waits_for_adoption(service, turn);
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    assert!(service.agent_provider_task_is_pending(turn));
}

/// A late message can retain a wake intent while the publication owns the
/// actual dispatch fence. Exercise that public claim before and after physical
/// commit; accepting a claim or retiring the waiting turn is not a valid hold.
fn assert_dispatch_waits_for_adoption(service: &mut RuntimeSessionService, turn: &str) {
    assert!(
        !service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn)
    );
    let agent = AgentId::opaque(
        service
            .agent_turn_ledger()
            .turn(turn)
            .unwrap()
            .agent_id
            .clone(),
    )
    .unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&agent, turn)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.agent_turn_ledger().turn(turn).unwrap().state,
        AgentTurnState::Running
    );
}

/// Verifies actual cold head bytes and retained native/lifecycle metadata.
pub(super) fn assert_head(
    service: &RuntimeSessionService,
    store: &AgentTranscriptStore,
    turn: &str,
    summary: &str,
) {
    let conversation = &service
        .agent_turn_ledger()
        .turn(turn)
        .unwrap()
        .conversation_id;
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(
        cold.context().chronology()[0].block().content == summary,
        "cold head bytes differ from accepted summary"
    );
    assert!(
        cold.context().chronology() == service.agent_turn_contexts()[turn].chronology(),
        "cold canonical chronology differs from accepted live state"
    );
}
