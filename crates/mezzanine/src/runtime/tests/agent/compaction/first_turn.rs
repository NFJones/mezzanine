//! First-task compaction crosses protected islands without summarizing them.
//! Inference, publication, receipt adoption and continuation remain distinct.

use super::*;

/// Actual rejection of an exact/pending-only first task must settle once and
/// export every original occurrence through its ordinary bookkeeping owner.
/// No auxiliary request, fabricated summary or action replay is permitted.
#[test]
fn runtime_first_turn_exact_only_provider_rejection_preserves_terminal_source() {
    struct Reject(std::cell::Cell<usize>);
    impl crate::integrations::agent::provider::ModelProvider for Reject {
        fn provider_id(&self) -> &str {
            "runtime-batch"
        }
        fn send_request(
            &self,
            request: &mez_agent::ModelRequest,
        ) -> Result<mez_agent::ModelResponse> {
            self.0.set(self.0.get() + 1);
            assert_eq!(
                request.interaction_kind,
                mez_agent::ModelInteractionKind::ActionExecution
            );
            for marker in ["FIRST_ACTIVE_USER_ROOT", "PENDING_ISLAND_RESULT"] {
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.content.contains(marker))
                );
            }
            Err(MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                ))
        }
    }
    let (mut service, store, turn) = first_turn();
    pending_island(&mut service, &turn);
    let before = service.agent_turn_contexts()[&turn].clone();
    let conversation = conversation(&service, &turn);
    let provider = Reject(std::cell::Cell::new(0));
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    assert!(
        service
            .execute_agent_turn_with_provider(&turn, &provider, profile)
            .is_err()
    );
    assert_eq!(provider.0.get(), 1);
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let work = service.claim_bookkeeping_candidates();
    assert!(!work.is_empty());
    for work in work {
        let history = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, history)
                .unwrap()
        );
    }
    let pending = service
        .persistence
        .pending_transcript_entries(&conversation);
    let records = pending
        .iter()
        .filter_map(|row| {
            mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
        })
        .collect::<Vec<_>>();
    for original in before.chronology() {
        assert_eq!(
            records
                .iter()
                .filter(|record| record.record() == original.record())
                .count(),
            1,
            "original source must export exactly once"
        );
        assert!(
            pending
                .iter()
                .any(|row| row.content.contains(&original.block().content))
        );
    }
    assert!(!store.transcript_path(&conversation).unwrap().exists());
}

fn first_turn() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("first-turn-protected-islands"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "first-turn-protected-islands".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "first-turn"
shell_mode = "pane"
compaction_raw_retention_percent = 1
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.first-turn]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
            .into(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(
            &primary,
            "FIRST_ACTIVE_USER_ROOT continue with the collected evidence",
        )
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    (service, store, turn)
}

/// Real provider execution supplies the completion witness; test source is not
/// made eligible through a bare retention annotation or fabricated group close.
fn progress(
    service: &mut RuntimeSessionService,
    turn: &str,
    text: &str,
    input_tokens: u64,
) -> mez_agent::AgentTurnExecution {
    static NEXT_ACTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut response = runtime_say_response(turn, text, false);
    response.action_batch.as_mut().unwrap().actions[0].id = format!(
        "first-turn-say-{}",
        NEXT_ACTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );
    response.usage.input_tokens = input_tokens;
    response.usage.output_tokens = 1;
    let profile = service.agent_turn_model_profile(turn).unwrap().clone();
    let execution = service
        .execute_agent_turn_with_provider(turn, &RuntimeBatchProvider { response }, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let context = &service.agent_turn_contexts()[turn];
    let group = context
        .chronology()
        .last()
        .unwrap()
        .execution_group_id()
        .unwrap();
    assert!(!context.execution_group_is_pending(group));
    execution
}

fn conversation(service: &RuntimeSessionService, turn: &str) -> String {
    service
        .agent_turn_ledger()
        .turn(turn)
        .unwrap()
        .conversation_id
        .clone()
}

fn owner(service: &RuntimeSessionService, turn: &str) -> mez_agent::ReplayEpochContext {
    mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(conversation(service, turn)).unwrap(),
        service.agent_turn_contexts()[turn].clone(),
    )
    .unwrap()
}

fn plan(service: &RuntimeSessionService, turn: &str) -> mez_agent::ReplayCompactionPlan {
    match owner(service, turn)
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(20_000, 1).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("completed first-task source was ineligible: {other:?}"),
    }
}

fn assert_unarchived(service: &RuntimeSessionService, store: &AgentTranscriptStore, turn: &str) {
    let conversation = conversation(service, turn);
    assert!(!store.transcript_path(&conversation).unwrap().exists());
    assert!(
        service
            .persistence
            .pending_transcript_entries(&conversation)
            .is_empty()
    );
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
}

/// Result-shaped source without a completion witness must remain a whole exact
/// island, even when independent closed groups on either side are summarized.
fn pending_island(
    service: &mut RuntimeSessionService,
    turn: &str,
) -> mez_agent::ContextExecutionGroupId {
    let group = mez_agent::ContextExecutionGroupId::new("first-task-pending-island").unwrap();
    let context = service.agent_turn_contexts_mut().get_mut(turn).unwrap();
    context
        .append_assistant_event("pending decision", "PENDING_ISLAND_DECISION", group.clone())
        .unwrap();
    context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "result without closure",
            "PENDING_ISLAND_RESULT ".repeat(500),
            group.clone(),
            None,
            true,
        )
        .unwrap();
    assert!(context.execution_group_is_pending(&group));
    group
}

fn queue_observed(service: &mut RuntimeSessionService, turn: &str) -> bool {
    let profile = service.agent_turn_model_profile(turn).unwrap().clone();
    service
        .queue_agent_active_turn_compaction(
            turn,
            "first-turn".into(),
            profile,
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: 20_000,
                max_input_tokens: 20_000,
            },
        )
        .unwrap()
}

/// Only accepted receipt adoption may replace live source or wake execution.
/// Verify complete retained occurrences, not a length-based prefix cut.
fn publish(
    service: &mut RuntimeSessionService,
    store: &AgentTranscriptStore,
    turn: &str,
    selected: &mez_agent::ReplayCompactionPlan,
    summary: &str,
) -> mez_agent::ReplayEpochContext {
    let before = service.agent_turn_contexts()[turn].clone();
    let retained = selected.retained_raw_events().cloned().collect::<Vec<_>>();
    let removed = selected.removed_events().cloned().collect::<Vec<_>>();
    let source = crate::test_support::compaction_source::rendered_source(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .expect("source queued"),
    )
    .unwrap();
    for event in selected.summary_input() {
        assert!(source.contains(&event.block().content));
    }
    for marker in [
        "FIRST_ACTIVE_USER_ROOT",
        "preserve this exact instruction",
        "PENDING_ISLAND_DECISION",
        "PENDING_ISLAND_RESULT",
    ] {
        assert!(
            !source.contains(marker),
            "protected source entered summary input: {marker}"
        );
    }
    assert!(!service.agent_provider_task_is_pending(turn));
    let requests = run_runtime_test_compaction_wave(
        service,
        "%1",
        Ok(runtime_test_compaction_response(summary)),
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(service.agent_turn_contexts()[turn], before);
    assert!(!service.agent_provider_task_is_pending(turn));
    replay_observed::adopt(service, turn);
    let after = &service.agent_turn_contexts()[turn];
    after.validate_durable().unwrap();
    assert_eq!(after.chronology()[0].block().content, summary);
    assert_eq!(&after.chronology()[1..], retained);
    for event in &removed {
        assert!(
            !after
                .chronology()
                .iter()
                .any(|kept| kept.record().occurrence_id() == event.record().occurrence_id())
        );
    }
    for event in before
        .chronology()
        .iter()
        .filter(|event| event.retention() == mez_agent::ContextRetention::Exact)
    {
        assert_eq!(
            after
                .chronology()
                .iter()
                .filter(|kept| *kept == event)
                .count(),
            1
        );
    }
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(
            &conversation(service, turn),
            mez_agent::AgentContext::empty(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), after.chronology());
    assert_eq!(cold.members().len(), retained.len());
    assert!(service.agent_provider_task_is_pending(turn));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        service.agent_turn_ledger().turn(turn).unwrap().state,
        AgentTurnState::Running
    );
    cold
}

fn request_text(execution: &mez_agent::AgentTurnExecution) -> String {
    execution
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Two actual pressure samples in the same first task must compact new closed
/// groups, without a once-per-turn latch or reprocessing already covered source.
#[test]
fn runtime_observed_first_turn_without_archive_recovers_turn_locally() {
    let (mut service, store, turn) = first_turn();
    let root = service.agent_turn_contexts()[&turn].chronology().to_vec();
    let mut prior_head = None;
    for (revision, marker, summary) in [
        (1, "FIRST_TURN_EVIDENCE", "first task summary one"),
        (2, "FRESH_SAME_TURN_EVIDENCE", "first task summary two"),
    ] {
        let execution = progress(
            &mut service,
            &turn,
            &format!("{marker} {}", "evidence ".repeat(500)),
            20_000,
        );
        if revision == 1 {
            assert_unarchived(&service, &store, &turn);
        } else {
            let request = request_text(&execution);
            assert!(request.contains("first task summary one"));
            assert!(request.contains("FIRST_ACTIVE_USER_ROOT"));
            assert!(!request.contains("FIRST_TURN_EVIDENCE"));
        }
        assert!(service.agent_is_compacting("%1"));
        let selected = plan(&service, &turn);
        assert!(
            selected
                .summary_input()
                .iter()
                .any(|event| event.block().content.contains(marker))
        );
        if let Some(head) = prior_head {
            assert_eq!(selected.summary_input()[0].record().occurrence_id(), head);
        }
        let cold = publish(&mut service, &store, &turn, &selected, summary);
        assert_eq!(cold.revision().get(), revision);
        for event in &root {
            assert!(cold.context().chronology().contains(event));
        }
        prior_head = Some(cold.head().unwrap().occurrence_id());
        let before = service.agent_turn_contexts()[&turn].clone();
        assert!(!queue_observed(&mut service, &turn));
        assert_eq!(service.agent_turn_contexts()[&turn], before);
        assert!(service.pending_agent_compaction_tasks().is_empty());
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        assert!(service.agent_provider_task_is_pending(&turn));
    }
    let continuation = progress(&mut service, &turn, "ordinary continuation", 0);
    let request = request_text(&continuation);
    assert!(request.contains("first task summary two"));
    assert!(request.contains("FIRST_ACTIVE_USER_ROOT"));
    assert!(!request.contains("FIRST_TURN_EVIDENCE"));
    assert!(!request.contains("FRESH_SAME_TURN_EVIDENCE"));
}

#[test]
fn runtime_observed_first_turn_resumes_fitting_local_summary() {
    let (mut service, store, turn) = first_turn();
    progress(&mut service, &turn, &"FIRST_DECISION ".repeat(500), 0);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .append_user_event("steering", "preserve this exact instruction")
        .unwrap();
    let pending = pending_island(&mut service, &turn);
    progress(&mut service, &turn, &"SECOND_DECISION ".repeat(500), 20_000);
    assert_unarchived(&service, &store, &turn);
    let selected = plan(&service, &turn);
    for marker in ["FIRST_DECISION", "SECOND_DECISION"] {
        assert!(
            selected
                .summary_input()
                .iter()
                .any(|event| event.block().content.contains(marker))
        );
    }
    assert!(
        selected
            .summary_input()
            .iter()
            .all(|event| event.execution_group_id() != Some(&pending))
    );
    assert!(selected.retained_raw_events().count() > selected.retained_tail().len());
    let cold = publish(
        &mut service,
        &store,
        &turn,
        &selected,
        "closed decisions summarized",
    );
    assert!(cold.context().execution_group_is_pending(&pending));
    let execution = progress(&mut service, &turn, "ordinary continuation", 0);
    let request = request_text(&execution);
    for marker in [
        "FIRST_ACTIVE_USER_ROOT",
        "preserve this exact instruction",
        "PENDING_ISLAND_DECISION",
        "PENDING_ISLAND_RESULT",
        "closed decisions summarized",
    ] {
        assert!(request.contains(marker), "continuation lost {marker}");
    }
    for marker in ["FIRST_DECISION", "SECOND_DECISION"] {
        assert!(
            !request.contains(marker),
            "covered source replayed: {marker}"
        );
    }
}

#[test]
fn runtime_observed_first_turn_compactor_walkback_is_turn_local() {
    let (mut service, store, turn) = first_turn();
    for marker in ["OLDER_FIRST_WALKBACK", "NEWER_FIRST_WALKBACK"] {
        progress(
            &mut service,
            &turn,
            &format!("{marker} {}", "evidence ".repeat(500)),
            0,
        );
    }
    assert_unarchived(&service, &store, &turn);
    let selected = plan(&service, &turn);
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
                1,
            )
            .unwrap()
    );
    publish(
        &mut service,
        &store,
        &turn,
        &selected,
        "provider rejection recovered",
    );
    let execution = progress(&mut service, &turn, "continue after rejection", 0);
    let request = request_text(&execution);
    assert!(request.contains("FIRST_ACTIVE_USER_ROOT"));
    assert!(request.contains("provider rejection recovered"));
    assert!(!request.contains("OLDER_FIRST_WALKBACK"));
    assert!(!request.contains("NEWER_FIRST_WALKBACK"));
}

/// A source containing only exact obligations cannot produce fake summary work.
/// An accepted final is still usable even if a local request estimate is small.
#[test]
fn runtime_observed_first_turn_irreducible_pressure_does_not_fail_execution() {
    let (mut service, store, turn) = first_turn();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .append_user_event(
            "irreducible steering",
            "EXACT_ONLY_INSTRUCTION ".repeat(500),
        )
        .unwrap();
    let pending = pending_island(&mut service, &turn);
    let before = service.agent_turn_contexts()[&turn].clone();
    assert!(before.chronology().iter().all(|event| event.retention()
        == mez_agent::ContextRetention::Exact
        || event.execution_group_id() == Some(&pending)));
    assert!(matches!(
        owner(&service, &turn)
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(20_000, 1).unwrap())
            .unwrap(),
        mez_agent::ReplayCompactionPreparation::NoWork(_)
    ));
    assert!(!queue_observed(&mut service, &turn));
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert!(!service.agent_is_compacting("%1"));
    assert_unarchived(&service, &store, &turn);
    let mut profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".into(), "100".into());
    let mut response = runtime_say_response(&turn, "usable final with exact source", true);
    response.usage.input_tokens = 100;
    let execution = service
        .execute_agent_turn_with_provider(&turn, &RuntimeBatchProvider { response }, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = request_text(&execution);
    for marker in [
        "FIRST_ACTIVE_USER_ROOT",
        "EXACT_ONLY_INSTRUCTION",
        "PENDING_ISLAND_RESULT",
    ] {
        assert!(request.contains(marker));
    }
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
}
