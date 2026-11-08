//! First-turn pressure with no archive and no eligible canonical prefix.
//!
//! These retain the original regression names, but selective turn-local
//! summaries are no longer a supported outcome. Real provider execution leaves
//! closed work behind the active user root; neither pressure nor a rejection
//! permits skipping that root or manufacturing a second replay authority.

use super::*;

/// Admits the first actual task before its ordinary archive has been written.
/// There is no completed predecessor, imported history or invented closure.
fn first_turn() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("first-turn-protected-prefix"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "first-turn-protected-prefix".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "first-turn"
shell_mode = "pane"
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

/// A normal progress response creates the assistant/result group and its real
/// completion witness. The measured pressure belongs to this actual request,
/// rather than a fixture request without the original instruction snapshot.
fn progress(
    service: &mut RuntimeSessionService,
    turn: &str,
    text: &str,
    input_tokens: u64,
) -> mez_agent::AgentTurnExecution {
    let mut response = runtime_say_response(turn, text, false);
    response.action_batch.as_mut().unwrap().actions[0].id = format!(
        "say-{}",
        service.agent_turn_contexts()[turn].event_sequence_high_water_mark()
    );
    response.usage.input_tokens = input_tokens;
    response.usage.output_tokens = 1;
    let provider = RuntimeBatchProvider { response };
    let profile = service.agent_turn_model_profile(turn).unwrap().clone();
    let execution = service
        .execute_agent_turn_with_provider(turn, &provider, profile)
        .unwrap();
    assert_eq!(execution.latest_response_usage.input_tokens, input_tokens);
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

/// Proves the actual root, live controller and source—not just the empty
/// archive—explain why pressure must not queue auxiliary model work.
fn assert_no_eligible_prefix(
    service: &RuntimeSessionService,
    store: &AgentTranscriptStore,
    turn: &str,
) {
    let conversation = &service
        .agent_turn_ledger()
        .turn(turn)
        .unwrap()
        .conversation_id;
    let context = &service.agent_turn_contexts()[turn];
    context.validate_durable().unwrap();
    let root = context
        .chronology()
        .iter()
        .find(|event| event.block().content.starts_with("FIRST_ACTIVE_USER_ROOT"))
        .unwrap();
    assert_eq!(root.retention(), mez_agent::ContextRetention::Exact);
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(conversation).unwrap(),
        context.clone(),
    )
    .unwrap();
    assert!(matches!(
        owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(20_000, 1).unwrap())
            .unwrap(),
        mez_agent::ReplayCompactionPreparation::NoWork(_)
    ));
    assert!(owner.head().is_none());
    assert!(!store.transcript_path(conversation).unwrap().exists());
    assert!(
        store
            .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert!(!service.agent_is_compacting("%1"));
}

/// A first-turn observed overflow with no archive retains the complete active
/// task. Its fitting ordinary continuation stays pending, with no private
/// summary or premature archive/publication acknowledgment.
#[test]
fn runtime_observed_first_turn_without_archive_recovers_turn_locally() {
    let (mut service, store, turn) = first_turn();
    let root = service.agent_turn_contexts()[&turn].chronology().to_vec();
    progress(
        &mut service,
        &turn,
        &"FIRST_TURN_EVIDENCE ".repeat(500),
        20_000,
    );
    let context = &service.agent_turn_contexts()[&turn];
    assert!(context.chronology().starts_with(&root));
    assert!(
        context
            .chronology()
            .iter()
            .any(|event| event.block().content.contains("FIRST_TURN_EVIDENCE"))
    );
    assert_no_eligible_prefix(&service, &store, &turn);
    assert!(service.agent_provider_task_is_pending(&turn));
    assert!(!service.agent_turn_has_consumed_observed_input_for_tests(&turn));
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let pending = service
        .persistence
        .pending_transcript_entries(&conversation);
    // Progress remains live source until ordinary bookkeeping exports it;
    // absence of an archive is not a fabricated pending-append receipt.
    assert!(pending.is_empty());
    let before = context.clone();
    assert!(
        !service
            .apply_agent_compaction_completed_event(
                "%1",
                runtime_test_compaction_response("unclaimed first-turn summary"),
            )
            .unwrap()
    );
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries(&conversation),
        pending
    );
}

/// Closed groups on both sides of exact steering do not justify dropping the
/// original task root. The next real provider request resumes with every exact
/// occurrence, rather than adopting a seemingly fitting selective summary.
#[test]
fn runtime_observed_first_turn_resumes_fitting_local_summary() {
    let (mut service, store, turn) = first_turn();
    progress(&mut service, &turn, &"FIRST_DECISION ".repeat(200), 0);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .append_user_event("steering", "preserve this exact instruction")
        .unwrap();
    let original = service.agent_turn_contexts()[&turn].chronology().to_vec();
    progress(&mut service, &turn, &"SECOND_DECISION ".repeat(200), 20_000);
    let before = service.agent_turn_contexts()[&turn].clone();
    assert!(before.chronology().starts_with(&original));
    assert_no_eligible_prefix(&service, &store, &turn);
    assert!(
        !service
            .apply_agent_compaction_completed_event("%1", runtime_test_compaction_response("x"),)
            .unwrap()
    );
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    let execution = progress(&mut service, &turn, "ordinary continuation", 0);
    let request = execution
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for text in [
        "FIRST_ACTIVE_USER_ROOT",
        "FIRST_DECISION",
        "preserve this exact instruction",
        "SECOND_DECISION",
    ] {
        assert!(request.contains(text), "ordinary continuation lost {text}");
    }
    assert!(
        service.agent_turn_contexts()[&turn]
            .chronology()
            .starts_with(before.chronology())
    );
    assert_no_eligible_prefix(&service, &store, &turn);
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// A real rejection of the complete first-turn request cannot start selective
/// walkback after its protected root. Failure settles once and preserves all
/// original source and pending audit rows, without retrying actions or inventing
/// a durable/turn-local summary to make the request appear smaller.
#[test]
fn runtime_observed_first_turn_compactor_walkback_is_turn_local() {
    let (mut service, store, turn) = first_turn();
    for marker in ["OLDER_FIRST_WALKBACK", "NEWER_FIRST_WALKBACK"] {
        progress(
            &mut service,
            &turn,
            &format!("{marker} {}", "evidence ".repeat(1_000)),
            20_000,
        );
    }
    assert_no_eligible_prefix(&service, &store, &turn);
    let before = service.agent_turn_contexts()[&turn].clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    assert!(
        service
            .persistence
            .pending_transcript_entries(&conversation)
            .is_empty()
    );
    let provider = RejectRequest {
        calls: std::cell::Cell::new(0),
    };
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let error = service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap_err();
    assert!(
        error.message().contains("context length exceeded"),
        "{error}"
    );
    assert_eq!(provider.calls.get(), 1);
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!store.transcript_path(&conversation).unwrap().exists());
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    // Terminal cleanup retires the live context. Its actual bookkeeping owner
    // must export the original source; an empty live map is not source loss.
    assert!(!service.agent_turn_contexts().contains_key(&turn));
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
    assert!(!pending.is_empty());
    let records = pending
        .iter()
        .filter_map(|row| {
            mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
        })
        .collect::<Vec<_>>();
    for original in before.chronology() {
        let exported = records
            .iter()
            .filter(|record| record.record() == original.record())
            .collect::<Vec<_>>();
        assert_eq!(
            exported.len(),
            1,
            "original source must export exactly once"
        );
        match exported[0].event() {
            mez_agent::TranscriptContextEvent::UserEvent { label, content, .. }
            | mez_agent::TranscriptContextEvent::TaskAdmission { label, content, .. }
            | mez_agent::TranscriptContextEvent::PromptBoundary { label, content, .. }
            | mez_agent::TranscriptContextEvent::ExecutionBlock { label, content, .. } => {
                assert_eq!(label, &original.block().label);
                assert_eq!(content, &original.block().content);
            }
            mez_agent::TranscriptContextEvent::EnvironmentSnapshot { content, .. }
            | mez_agent::TranscriptContextEvent::McpCatalogSnapshot { content, .. } => {
                assert_eq!(content, &original.block().content);
            }
            other => panic!("unexpected first-turn fixture source: {other:?}"),
        }
    }
    for marker in [
        "FIRST_ACTIVE_USER_ROOT",
        "OLDER_FIRST_WALKBACK",
        "NEWER_FIRST_WALKBACK",
    ] {
        assert!(pending.iter().any(|row| row.content.contains(marker)));
    }
    assert!(!store.transcript_path(&conversation).unwrap().exists());
}

/// Whole-request overhead may exhaust the estimate-derived summary allowance
/// after the provider has already accepted execution. With no eligible prefix,
/// that estimate must not turn successful progress into an application error.
#[test]
fn runtime_observed_first_turn_irreducible_pressure_does_not_fail_execution() {
    let (mut service, store, turn) = first_turn();
    let mut profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".into(), "100".into());
    service.set_agent_turn_model_profile(turn.clone(), profile);
    progress(
        &mut service,
        &turn,
        &"PROTECTED_FIRST_TURN_RESULT ".repeat(500),
        100,
    );
    assert_no_eligible_prefix(&service, &store, &turn);
    assert!(service.agent_provider_task_is_pending(&turn));
    assert!(!service.agent_turn_has_consumed_observed_input_for_tests(&turn));
    let execution = progress(&mut service, &turn, "continue with exact source", 0);
    assert!(
        execution
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("PROTECTED_FIRST_TURN_RESULT"))
    );
}

/// Rejects only the real execution request. Any unexpected compactor request
/// fails its interaction assertion before it can masquerade as recovery.
struct RejectRequest {
    calls: std::cell::Cell<usize>,
}

impl crate::integrations::agent::provider::ModelProvider for RejectRequest {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    fn send_request(&self, request: &mez_agent::ModelRequest) -> Result<mez_agent::ModelResponse> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(
            request.interaction_kind,
            mez_agent::ModelInteractionKind::ActionExecution
        );
        for marker in [
            "FIRST_ACTIVE_USER_ROOT",
            "OLDER_FIRST_WALKBACK",
            "NEWER_FIRST_WALKBACK",
        ] {
            assert!(
                request
                    .messages
                    .iter()
                    .any(|message| message.content.contains(marker))
            );
        }
        Err(
            MezError::invalid_state("provider context length exceeded").with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            ),
        )
    }
}
