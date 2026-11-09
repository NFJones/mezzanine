//! Automatic pressure must not repeatedly consume the same just-published
//! source. Actual task completion supplies new eligibility; a new request alone
//! cannot authorize another identical compaction of the retained closed tail.

use super::*;

fn context_limit() -> MezError {
    MezError::invalid_state("provider context length exceeded").with_provider_failure_json(
        r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
    )
}

fn source() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    crate::test_support::compaction_source::recovery_source(
        test_runtime_service(),
        temp_root("compaction-automatic-reentry"),
        &"original closed task source ".repeat(50),
        "original closed task completed",
        100,
    )
}

fn projection(model: &str) -> mez_agent::ReplayProviderProjection {
    mez_agent::ReplayProviderProjection::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "runtime-batch",
        model,
    )
    .unwrap()
}

/// The public idle command bypasses automatic reentry without inventing a turn
/// or consuming its own temporary command prompt as fresh historical source.
#[test]
fn runtime_manual_compaction_explicitly_rearms_unchanged_published_source() {
    let (mut service, store, turn) = source();
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn,
        "current task really completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let archive = store.inspect(&conversation).unwrap();
    for (revision, summary) in [(1, "MANUAL_REENTRY_ONE"), (2, "MANUAL_REENTRY_TWO")] {
        let command = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(command.contains("state=queued"), "{command}");
        let requests = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response(summary)),
        );
        assert_eq!(requests.len(), 1);
        let receipt =
            crate::test_support::compaction_publication::execute_pending_publication(&mut service);
        assert!(
            service
                .apply_replay_epoch_completion(receipt)
                .unwrap()
                .applied
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(!service.agent_is_compacting("%1"));
        let (_, mut current) = store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        current
            .select_provider_projection(projection("test"))
            .unwrap();
        assert_eq!(current.revision().get(), revision);
        assert_eq!(current.context().chronology()[0].block().content, summary);
        assert!(
            !current
                .automatic_compaction_is_fresh(
                    mez_agent::ReplayTailPolicy::new(40_000, 100).unwrap()
                )
                .unwrap()
        );
        assert!(matches!(
            current
                .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 100).unwrap())
                .unwrap(),
            mez_agent::ReplayCompactionPreparation::Ready(_)
        ));
    }
    assert_eq!(store.inspect(&conversation).unwrap(), archive);
}

/// Capture selects the actual compactor model on an isolated owner even when
/// the live cache belongs to an earlier model. After capture, a live view switch
/// or a task-profile substitution refuses before credentials or a model worker.
#[test]
fn runtime_compaction_freezes_actual_model_projection_without_mutating_live_source() {
    for alter_profile in [false, true] {
        let (mut service, store, turn) = source();
        service
            .agent_turn_contexts_mut()
            .get_mut(&turn)
            .unwrap()
            .update_replay_owner(|owner| owner.select_provider_projection(projection("prior-view")))
            .unwrap();
        let before = service.agent_turn_contexts()[&turn].clone();
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn,
                    &context_limit(),
                    1,
                )
                .unwrap()
        );
        let frozen = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        assert_eq!(
            frozen
                .replay_source
                .as_ref()
                .unwrap()
                .frozen_plan()
                .provider_projection(),
            Some(&projection("test"))
        );
        assert_eq!(service.agent_turn_contexts()[&turn], before);
        let conversation = frozen.conversation_id.clone();
        if alter_profile {
            let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
            task.model_profile.model = "different-model".into();
            service.queue_agent_compaction_task(task);
        } else {
            service
                .agent_turn_contexts_mut()
                .get_mut(&turn)
                .unwrap()
                .update_replay_owner(|owner| {
                    owner.select_provider_projection(projection("unexpected-view"))
                })
                .unwrap();
        }
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        let error = service
            .claim_agent_compaction_task("%1", generation)
            .expect_err("changed source/profile must refuse before provider construction");
        assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
        assert!(
            error.message().contains(if alter_profile {
                "model profile changed"
            } else {
                "provider projection changed"
            }),
            "{error}"
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(!service.retains_compaction_expense_for_tests("%1", generation));
        // A host's fallback failure event is a duplicate after synchronous
        // undispatched settlement, with no new completion or model-work proof.
        assert!(
            !service
                .apply_agent_compaction_transition(
                    crate::test_support::compaction_source::model_result_event(
                        "%1",
                        generation,
                        Err(error),
                    ),
                )
                .unwrap()
                .applied
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_tasks().is_empty());
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            AgentTurnState::Failed
        );
    }
}

/// A 100% optional reservation leaves closed source after the mandatory oldest
/// unit is compacted. That remaining eligibility makes repeated automatic
/// recovery a real regression, rather than an incidental no-prefix result.
#[test]
fn runtime_automatic_compaction_reentry_requires_fresh_closed_source() {
    let (mut service, store, turn) = source();
    let agent = AgentId::opaque("agent-%1").unwrap();
    assert!(
        service
            .recover_agent_provider_context_limit_failure(&agent, &turn, &context_limit(), 1)
            .unwrap()
    );
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("FIRST_REENTRY_HEAD")),
    );
    assert_eq!(requests.len(), 1);
    replay_observed::adopt(&mut service, &turn);
    let before = service.agent_turn_contexts()[&turn].clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        before.clone(),
    )
    .unwrap();
    assert!(
        matches!(
            owner
                .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 100).unwrap())
                .unwrap(),
            mez_agent::ReplayCompactionPreparation::Ready(_)
        ),
        "closed retained source remains eligible for explicit work"
    );
    let first_head = owner.head().unwrap().clone();
    assert!(
        !service
            .recover_agent_provider_context_limit_failure(&agent, &turn, &context_limit(), 2)
            .unwrap(),
        "unchanged published source must report context unavailable without another compactor"
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.agent_provider_task_is_pending(&turn));
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert_eq!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap()
            .1
            .head(),
        Some(&first_head)
    );

    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn,
        "the compacted current task really completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    let started = service
        .execute_agent_shell_command(&primary, "a genuinely new exact task")
        .unwrap();
    assert!(started.contains(r#""state":"running""#), "{started}");
    let successor = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    assert!(
        service
            .recover_agent_provider_context_limit_failure(&agent, &successor, &context_limit(), 1)
            .unwrap(),
        "newly closed task source must rearm automatic recovery"
    );
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("SECOND_REENTRY_HEAD")),
    );
    assert_eq!(requests.len(), 1);
    replay_observed::adopt(&mut service, &successor);
    let (_, current) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_ne!(current.head(), Some(&first_head));
    assert_eq!(current.revision().get(), 2);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_provider_task_is_pending(&successor));
}
