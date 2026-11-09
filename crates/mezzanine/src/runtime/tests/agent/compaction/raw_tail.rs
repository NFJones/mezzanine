//! Connected raw publication through real admission, claims, storage and adoption.
//! No auth store is attached: even attempting ordinary compactor construction
//! would fail instead of silently passing with a permissive fake provider.

use super::*;

pub(super) fn enable_raw(service: &mut RuntimeSessionService) {
    let mut layers = service.config_layers().to_vec();
    let primary = layers
        .iter_mut()
        .find(|layer| layer.scope == ConfigScope::Primary)
        .unwrap();
    primary.text = primary
        .text
        .replacen("[agents]", "[agents]\ncompaction_mode=\"raw_tail\"", 1);
    service.replace_config_layers(layers).unwrap();
    assert_eq!(
        service.agent_compaction_mode(),
        crate::runtime::config::CompactionMode::RawTail
    );
    assert!(service.auth_store().is_none());
}

/// Live pane overrides do not change config generation. Narrowing authority
/// after source capture must nevertheless refuse the stale raw claim before
/// persistence, with no model dispatch or orphan compaction marker.
#[test]
fn runtime_raw_tail_policy_narrowing_refuses_unclaimed_publication() {
    let (mut service, store, turn) = observed_history::prepared_with_cap(40_000, false);
    enable_raw(&mut service);
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    let record = service.agent_turn_ledger().turn(&turn).unwrap().clone();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    assert!(
        service
            .queue_agent_active_turn_compaction(
                &turn,
                record.model_profile,
                profile,
                crate::runtime::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens: 40_000,
                    max_input_tokens: 40_000,
                }
            )
            .unwrap()
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let archive = store.inspect(&task.conversation_id).unwrap();
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::ReadOnly));
    let result = service.claim_agent_compaction_task("%1", task.task_generation);
    assert!(
        !matches!(result, Ok(Some(_))),
        "stale policy dispatched model work"
    );
    assert!(
        service.take_replay_epoch_attempt_for_tests().is_none(),
        "narrowed policy admitted stale publication"
    );
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert!(!service.retains_compaction_expense_for_tests("%1", task.task_generation));
    let after = store.inspect(&task.conversation_id).unwrap();
    // Terminal failure settles this turn's already accepted pending input;
    // those ordinary appends are separate from a compaction publication.
    assert!(after.starts_with(&archive));
    assert!(after[archive.len()..].iter().all(|row| row.turn_id == turn));
    assert!(
        after
            .iter()
            .any(|row| row.content.contains("CURRENT_USER_PROMPT"))
    );
    assert!(
        store
            .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
}

/// With a locally fitting 100% tail, authoritative rejection still removes
/// complete optional source. Repeated recovery exhausts current membership
/// instead of republishing an identical request or falling back to summary.
#[test]
fn runtime_raw_tail_real_rejection_exhausts_optional_source() {
    let (mut service, store, turn) = observed_history::prepared_with_cap(40_000, false);
    enable_raw(&mut service);
    let mut layers = service.config_layers().to_vec();
    layers[0].text = layers[0].text.replacen(
        "compaction_raw_retention_percent=1\n",
        "compaction_raw_retention_percent=100\n",
        1,
    );
    service.replace_config_layers(layers).unwrap();
    assert_eq!(service.agent_compaction_raw_retention_percent(), 100);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let archive = store.inspect(&conversation).unwrap();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    let original_count = service.agent_turn_contexts()[&turn].chronology().len();
    let mut previous_frontier = usize::MAX;
    let mut publications = 0;
    for attempt in 1..=original_count + 1 {
        if !service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque("agent-%1").unwrap(),
                &turn,
                &error,
                attempt as u32,
            )
            .unwrap()
        {
            break;
        }
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        assert!(task.request.is_none());
        let plan = task.replay_source.as_ref().unwrap().frozen_plan();
        assert!(plan.removed_raw_members().next().is_some());
        let frontier = plan.retained_optional_units().unwrap();
        assert!(frontier < previous_frontier);
        previous_frontier = frontier;
        assert!(
            service
                .claim_agent_compaction_task("%1", task.task_generation)
                .unwrap()
                .is_none()
        );
        let receipt = replay_publication::execute_pending_publication(&mut service);
        service.apply_replay_epoch_completion(receipt).unwrap();
        publications += 1;
    }
    assert!(publications > 0 && publications <= original_count);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert_eq!(store.inspect(&conversation).unwrap(), archive);
    assert!(
        service.agent_turn_contexts()[&turn]
            .blocks()
            .iter()
            .any(|block| block.content.contains("CURRENT_USER_PROMPT"))
    );
}

/// Exercise both actual pressure admissions. The exact active task, including
/// steering and post-freeze source, must survive one headless publication; an
/// original receipt may resume its parent only once.
#[test]
fn runtime_raw_tail_pressure_claims_publish_without_model_dispatch() {
    for provider_rejection in [false, true] {
        let (mut service, store, turn) = observed_history::prepared_with_cap(20_000, false);
        enable_raw(&mut service);
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        if provider_rejection {
            let error = MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                );
            assert!(
                service
                    .recover_agent_provider_context_limit_failure(
                        &AgentId::opaque("agent-%1").unwrap(),
                        &turn,
                        &error,
                        1
                    )
                    .unwrap()
            );
        } else {
            let mut response = runtime_say_response(&turn, "continue", false);
            response.usage.input_tokens = 20_000;
            response.usage.output_tokens = 1;
            let execution = service
                .execute_agent_turn_with_provider(
                    &turn,
                    &RuntimeBatchProvider { response },
                    profile,
                )
                .unwrap();
            assert_eq!(execution.terminal_state, AgentTurnState::Running);
            assert_eq!(execution.latest_response_usage.input_tokens, 20_000);
        }
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        assert!(
            task.request.is_none(),
            "raw mode constructed a compactor request"
        );
        let archive = store.inspect(&task.conversation_id).unwrap();
        let plan = task.replay_source.as_ref().unwrap().frozen_plan();
        assert_eq!(
            plan.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
        let mut expected = plan
            .retained_raw_events()
            .map(|event| event.record().occurrence_id())
            .collect::<Vec<_>>();
        let body = service.agent_turn_contexts_mut().get_mut(&turn).unwrap();
        body.append_user_event("late instruction", "RAW_LATE_ARRIVAL remains exact")
            .unwrap();
        expected.push(body.chronology().last().unwrap().record().occurrence_id());
        let before = body.clone();
        assert!(
            service
                .claim_agent_compaction_task("%1", task.task_generation)
                .unwrap()
                .is_none()
        );
        assert!(!service.retains_compaction_expense_for_tests("%1", task.task_generation));
        assert!(service.agent_is_compacting("%1"));
        assert_eq!(
            service.agent_turn_contexts()[&turn],
            before,
            "claim adopted unpublished authority"
        );
        let completion = replay_publication::execute_pending_publication(&mut service);
        service
            .apply_replay_epoch_completion(completion.clone())
            .unwrap();
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.agent_provider_task_is_pending(&turn));
        let after = service.agent_turn_contexts()[&turn].clone();
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&task.conversation_id).unwrap(),
            after.clone(),
        )
        .unwrap();
        assert!(owner.head().is_none());
        assert_eq!(
            owner.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
        assert_eq!(
            after
                .chronology()
                .iter()
                .map(|event| event.record().occurrence_id())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            after
                .blocks()
                .iter()
                .any(|block| block.content.contains("CURRENT_USER_PROMPT"))
        );
        assert_eq!(store.inspect(&task.conversation_id).unwrap(), archive);
        let (_, reopened) = store
            .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(reopened.context().chronology(), after.chronology());
        service.apply_replay_epoch_completion(completion).unwrap();
        assert_eq!(service.agent_turn_contexts()[&turn], after);
    }
}

/// The public argument-free command selects raw work after a genuine historical
/// import. Claiming must queue persistence even without a provider credential.
#[test]
fn runtime_raw_tail_manual_command_publishes_without_credentials() {
    let (mut service, store) =
        manual_model::queued("raw-manual", "eligible old history ".repeat(2_000));
    service.cancel_current_agent_compaction_task("%1");
    enable_raw(&mut service);
    let primary = service
        .attach_primary("raw-command", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let reply = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(reply.contains("state=queued"), "{reply}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert!(task.request.is_none());
    let archive = store.inspect(&task.conversation_id).unwrap();
    assert!(
        service
            .claim_agent_compaction_task("%1", task.task_generation)
            .unwrap()
            .is_none()
    );
    assert!(!service.retains_compaction_expense_for_tests("%1", task.task_generation));
    let receipt = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_turn_ledger().turns().is_empty());
    let (_, history) = store
        .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(history.head().is_none());
    assert_eq!(
        history.head_policy(),
        mez_agent::ReplayHeadPolicy::IntentionalOmission
    );
    assert_eq!(store.inspect(&task.conversation_id).unwrap(), archive);
}
