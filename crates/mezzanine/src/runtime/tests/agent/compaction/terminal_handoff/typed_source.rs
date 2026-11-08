//! Real task source following an explicitly imported historical prefix.
//!
//! The historical codec varies independently of task eligibility. Its original
//! grouped rows remain audit evidence; prefix normalization grants no execution
//! closure. Only the genuinely completed later task supplies compactable source.

use super::*;

/// Captures both old raw and accounted formats through their supported prefix
/// migration, then completes a real task and admits its exact successor.
pub(super) fn saved(accounted: bool) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, primary) = configured(true);
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let rows = [
        ContextBlock::assistant_event("same decision", "LEGACY_ORIGINAL_SOURCE"),
        ContextBlock::evidence_event(
            ContextSourceKind::ActionResult,
            "same result",
            "legacy result",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, block)| {
        let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            block.source,
            block.label.clone(),
            block.content.clone(),
            mez_agent::ContextExecutionGroupId::new("live-group-0").unwrap(),
            index as u64 + 1,
            None,
        )
        .unwrap();
        TranscriptEntry {
            conversation_id: conversation.clone(),
            sequence: index as u64 + 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: "historical-first".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: if accounted {
                mez_agent::transcript::TranscriptContextRecord::new(
                    event,
                    mez_agent::ContextBlockRecord::allocate(&block).unwrap(),
                    None,
                )
                .unwrap()
                .to_transcript_content()
                .unwrap()
            } else {
                event.to_transcript_content()
            },
        }
    })
    .collect::<Vec<_>>();
    store.append_many(&rows).unwrap();
    store
        .save_compaction_epoch(&conversation, 2, "ORIGINAL_IMPORTED_PREFIX")
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", rows.len())
        .unwrap();
    let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("LIVE_ORIGINAL_SOURCE {}", "source ".repeat(5_000)),
        "actual original task completed",
        "LIVE_INITIAL_PROMPT",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    assert_eq!(
        &store.inspect(&conversation).unwrap()[..rows.len()],
        rows.as_slice()
    );
    assert_eq!(
        service.agent_turn_contexts()[&turn].chronology()[0]
            .block()
            .content,
        "ORIGINAL_IMPORTED_PREFIX"
    );
    (service, store, turn)
}

/// Real completed source whose ordinary append is still pending. Logical
/// bookkeeping acceptance permits successor admission, but no archive row or
/// summary receipt is fabricated. First publication must back this exact source.
pub(super) fn pending() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, _) = configured(true);
    service.persistence.enable_transcript_adapter();
    let first = service
        .start_agent_prompt_turn(
            "%1",
            &format!("LIVE_ORIGINAL_SOURCE {}", "source ".repeat(5_000)),
        )
        .unwrap();
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &first.turn_id,
        "actual pending task completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    admit_bookkeeping(&mut service);
    let turn = service
        .start_agent_prompt_turn("%1", "LIVE_INITIAL_PROMPT")
        .unwrap()
        .turn_id;
    let conversation = &service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id;
    assert!(!store.transcript_path(conversation).unwrap().exists());
    assert!(
        !service
            .persistence
            .pending_transcript_entries(conversation)
            .is_empty()
    );
    (service, store, turn)
}

/// Executes original immutable reads before actual candidate admission.
pub(super) fn admit_bookkeeping(service: &mut RuntimeSessionService) {
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
}

/// Runs genuine original batches and delivers their exact completion events.
/// Other transcript effects retain their owner; no guessed byte count or
/// fabricated append receipt can clear a terminal or initial publication fence.
pub(super) fn settle(service: &mut RuntimeSessionService) {
    admit_bookkeeping(service);
    let mut writes = 0;
    for effect in service
        .drain_transcript_persistence_transition()
        .side_effects
    {
        match effect {
            RuntimeSideEffect::PersistTranscriptEntries { store, batch, path } => {
                let bytes = store.commit_accounted_transcript_batch(&batch, 1).unwrap();
                let event = crate::runtime::PersistenceEvent::TranscriptCompleted {
                    conversation_id: batch.conversation_id().into(),
                    first_sequence: batch.first_sequence(),
                    batch,
                    path,
                    bytes,
                };
                assert!(service.apply_persistence_transition(event).unwrap().applied);
                writes += 1;
            }
            effect => service.queue_transcript_for_tests(effect),
        }
    }
    assert!(writes > 0);
}

/// A genuine unsaved predecessor survives cleanup through the NoStore binding.
/// No archive is ever installed, so its source cannot acquire later write rights.
pub(super) fn memory() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, primary) = configured(false);
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage("%1", "memory-terminal", 0, None)
        .unwrap();
    let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("LIVE_ORIGINAL_SOURCE {}", "source ".repeat(5_000)),
        "actual unsaved task completed",
        "LIVE_INITIAL_PROMPT",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    assert!(service.agent_shell_store().get("%1").unwrap().ephemeral);
    assert!(
        service
            .agent_turn_history_source(&turn)
            .unwrap()
            .store()
            .is_none()
    );
    assert!(fs::read_dir(store.root()).unwrap().next().is_none());
    (service, store, turn)
}

/// Preserves the ordinary save policy with genuinely absent storage. Its first
/// task has no eligible prefix; neither later store installation nor a summary
/// fixture may pretend that the exact active root became completed history.
pub(super) fn missing_store() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, _) = configured(false);
    let turn = service
        .start_agent_prompt_turn("%1", "LIVE_INITIAL_PROMPT")
        .unwrap()
        .turn_id;
    assert!(!service.agent_shell_store().get("%1").unwrap().ephemeral);
    let source = service.agent_turn_history_source(&turn).unwrap();
    assert!(source.store().is_none());
    assert_eq!(source.ephemeral(), Some(false));
    (service, store, turn)
}

/// Common actual provider/pane configuration, before original source admission.
fn configured(
    saved: bool,
) -> (
    RuntimeSessionService,
    AgentTranscriptStore,
    mez_core::ids::ClientId,
) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("typed-terminal-source"));
    if saved {
        service.set_agent_transcript_store(store.clone());
    }
    service.replace_config_layers(vec![ConfigLayer {
        name: "typed-terminal-source".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"live\"\ncompaction_raw_retention_percent=10\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.live]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, store, primary)
}

/// Each trigger enters its actual retry owner rather than faking a recovering
/// attempt for later adoption. The original installed store may already differ.
pub(super) fn queue(service: &mut RuntimeSessionService, turn: &str, observed: bool) {
    queue_at_attempt(service, turn, observed, 1);
}

/// A later provider rejection keeps its actual monotonically advancing attempt.
pub(super) fn queue_at_attempt(
    service: &mut RuntimeSessionService,
    turn: &str,
    observed: bool,
    attempt: u32,
) {
    if observed {
        queue_live_compaction(service, turn, true);
    } else {
        let error = MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            );
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    turn,
                    &error,
                    attempt,
                )
                .unwrap()
        );
    }
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .replay_source
            .is_some()
    );
}

/// Actual inference output, worker receipt and actor adoption stay distinct.
pub(super) fn complete(service: &mut RuntimeSessionService, turn: &str, summary: &str) {
    let original = service.agent_turn_contexts()[turn].clone();
    complete_runtime_test_compaction(service, "%1", summary);
    assert_eq!(service.agent_turn_contexts()[turn], original);
    assert!(!service.agent_provider_task_is_pending(turn));
    let completion =
        crate::test_support::compaction_publication::execute_pending_publication(service);
    assert_eq!(service.agent_turn_contexts()[turn], original);
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    assert!(service.agent_provider_task_is_pending(turn));
    assert_eq!(
        service.agent_turn_contexts()[turn].chronology()[0]
            .block()
            .content,
        summary
    );
}
