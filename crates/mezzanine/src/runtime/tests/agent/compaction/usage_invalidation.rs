//! Execution display invalidation belongs to an accepted context replacement,
//! not auxiliary expense or continuation diagnostics. Exercise the unpublished
//! first-turn path and exact completion ownership without calling a provider.

use super::*;
use crate::runtime::AgentCompactionEvent;

/// Creates a first-turn observed-input compaction with no publishable archive,
/// positive ordinary usage, and separately retained cumulative expense. The
/// closed live group can be replaced, while its raw history remains authoritative.
fn local_usage_compaction() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let store = AgentTranscriptStore::new(temp_root("local-compaction-usage"));
    service.set_agent_transcript_store(store.clone());
    service.replace_config_layers(vec![ConfigLayer {
        name: "local-compaction-usage".into(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"local-usage\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.local-usage]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 20000\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"local-usage","method":"agent/shell/command","params":{"idempotency_key":"local-usage","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn_id = "turn-1";
    let context = service.agent_turn_contexts_mut().get_mut(turn_id).unwrap();
    context
        .replace_after_compaction(vec![
            ContextBlock::assistant_event("closed decision", "decision ".repeat(200)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "closed result",
                "result ".repeat(200),
            ),
            ContextBlock::user_event("exact steering", "preserve this instruction"),
        ])
        .unwrap();
    let context = service.agent_turn_contexts().get(turn_id).unwrap();
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        100,
        0,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    assert!(plan.changes_context());
    let profile = service.agent_turn_model_profile(turn_id).unwrap().clone();
    let sample = mez_agent::ModelTokenUsage {
        input_tokens: 20_000,
        output_tokens: 40,
        reasoning_tokens: 5,
        cached_input_tokens: Some(10_000),
        cache_write_input_tokens: Some(50),
    };
    service.record_agent_provider_token_usage_with_profile("%1", sample, sample, Some(&profile));
    assert!(service.queue_agent_active_turn_compaction(
        turn_id, "local-usage".into(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000, max_input_tokens: 20_000,
        }, plan,
    ).unwrap());
    service.checkpoint_agent_session_metadata().unwrap();
    (service, store, turn_id.into())
}

/// Checks every execution presentation owner, not just raw accounting: frame,
/// status cache/context, structured context sample, and latest request sample.
fn assert_unknown(service: &mut RuntimeSessionService, conversation: &str) {
    assert_eq!(service.agent_latest_request_usage(conversation), None);
    assert_eq!(service.agent_context_usage_snapshot(conversation), None);
    assert_eq!(service.agent_context_usage_display(conversation), None);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get("%1")
            .unwrap()
            .agent_context_usage,
        None
    );
    let status = service.runtime_agent_status_display("%1").unwrap();
    assert!(
        status.contains("| Latest request cache hit | unknown |"),
        "{status}"
    );
    assert!(!status.contains("50%"), "{status}");
}

/// A successful unpublished replacement clears positive ordinary context/cache
/// samples and checkpoints unknown without resetting any cumulative expense.
/// Auxiliary expense and omitted/zero ordinary usage cannot revive the sample;
/// a later positive ordinary response owns the new display and cache ratio.
#[test]
fn local_compaction_usage_success_clears_display_and_metadata() {
    let (mut service, store, turn_id) = local_usage_compaction();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let pane_before = service.agent_token_usage_for_pane("%1");
    assert_eq!(
        service
            .agent_context_usage_display(&conversation)
            .as_deref(),
        Some("50%")
    );
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = task.task_generation;
    service.claim_agent_compaction_task_state("%1", task);
    let mut response = runtime_test_compaction_response("closed work summarized");
    response.usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        cached_input_tokens: Some(2),
        ..Default::default()
    };
    let event = AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(response),
    };
    assert!(
        service
            .apply_agent_compaction_transition(event.clone())
            .unwrap()
            .applied
    );
    assert_unknown(&mut service, &conversation);
    assert_eq!(store.compaction_epoch(&conversation).unwrap(), None);
    assert!(!store.transcript_path(&conversation).unwrap().exists());
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let key = mez_agent::ModelTokenUsageKey::new("runtime-batch", "test");
    let mut expected = before.clone();
    expected
        .get_mut(&key)
        .unwrap()
        .add_assign(mez_agent::ModelTokenUsage {
            input_tokens: 17,
            output_tokens: 3,
            cached_input_tokens: Some(2),
            ..Default::default()
        });
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        expected
    );
    let mut pane_expected = pane_before;
    pane_expected
        .get_mut(&key)
        .unwrap()
        .add_assign(mez_agent::ModelTokenUsage {
            input_tokens: 17,
            output_tokens: 3,
            cached_input_tokens: Some(2),
            ..Default::default()
        });
    assert_eq!(service.agent_token_usage_for_pane("%1"), pane_expected);
    let metadata = store
        .load_agent_session_metadata(service.session.id.as_str())
        .unwrap();
    let metadata = metadata
        .iter()
        .find(|record| record.conversation_id == conversation)
        .unwrap();
    assert_eq!(metadata.context_usage, None);
    assert_eq!(metadata.context_usage_snapshot, None);
    assert_eq!(metadata.latest_request_usage, None);
    assert_eq!(metadata.token_usage_by_model, expected);
    assert!(
        !service
            .apply_agent_compaction_transition(event)
            .unwrap()
            .applied
    );
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        expected
    );
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    service.record_agent_provider_token_usage_by_model(
        "%1",
        &std::collections::BTreeMap::from([(
            key,
            mez_agent::ModelTokenUsage {
                input_tokens: 11,
                ..Default::default()
            },
        )]),
    );
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        Default::default(),
        Default::default(),
        Some(&profile),
    );
    assert_unknown(&mut service, &conversation);
    let fresh = mez_agent::ModelTokenUsage {
        input_tokens: 4_000,
        cached_input_tokens: Some(3_000),
        ..Default::default()
    };
    service.record_agent_provider_token_usage_with_profile("%1", fresh, fresh, Some(&profile));
    assert_eq!(
        service
            .agent_context_usage_display(&conversation)
            .as_deref(),
        Some("10%")
    );
    assert_eq!(
        service
            .agent_latest_request_usage(&conversation)
            .unwrap()
            .usage,
        fresh
    );
    let status = service.runtime_agent_status_display("%1").unwrap();
    assert!(
        status.contains("| Latest request cache hit | 75.00%"),
        "{status}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A checkpoint failure after replacement must not first announce/admit an
/// ordinary continuation. The exact waiting turn is failed once, accepted
/// replacement usage stays unknown, and cumulative expense remains untouched.
#[test]
fn local_compaction_usage_checkpoint_failure_precedes_recovery_admission() {
    let (mut service, store, turn_id) = local_usage_compaction();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = task.task_generation;
    service.claim_agent_compaction_task_state("%1", task);
    store.fail_next_agent_session_metadata_write();
    let event = AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(runtime_test_compaction_response("closed work summarized")),
    };
    assert!(
        service
            .apply_agent_compaction_transition(event.clone())
            .unwrap()
            .applied
    );
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(
        !trace.contains("observed input limit turn local compaction"),
        "continuation admitted before checkpoint: {trace}"
    );
    assert_unknown(&mut service, &conversation);
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        before
    );
    assert!(
        !service
            .apply_agent_compaction_transition(event)
            .unwrap()
            .applied
    );
    service.checkpoint_agent_session_metadata().unwrap();
    let records = store
        .load_agent_session_metadata(service.session.id.as_str())
        .unwrap();
    let record = records
        .iter()
        .find(|record| record.conversation_id == conversation)
        .unwrap();
    assert_eq!(record.context_usage_snapshot, None);
    assert_eq!(record.latest_request_usage, None);
    service.terminate_all_pane_processes().unwrap();
}

/// Failed and stale completions never own a replacement. A newer positive
/// sample and its checkpoint must survive, with no continuation queued by the
/// rejected generation and no duplicate settlement when its event is repeated.
#[test]
fn local_compaction_usage_failed_and_stale_preserve_new_sample() {
    for stale in [false, true] {
        let (mut service, store, turn_id) = local_usage_compaction();
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        let generation = task.task_generation;
        let replacement = task.clone();
        service.claim_agent_compaction_task_state("%1", task);
        let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        let fresh = mez_agent::ModelTokenUsage {
            input_tokens: 4_000,
            cached_input_tokens: Some(3_000),
            ..Default::default()
        };
        service.record_agent_provider_token_usage_with_profile("%1", fresh, fresh, Some(&profile));
        service.checkpoint_agent_session_metadata().unwrap();
        let before = service.agent_token_usage_for_conversation(&conversation);
        let event = if stale {
            // Keep the old claim issued, but supersede its ownership with a
            // live generation. Late completion must retire only the old claim.
            service.queue_agent_compaction_task(replacement);
            AgentCompactionEvent::Completed {
                pane_id: "%1".into(),
                task_generation: generation,
                response: Box::new(runtime_test_compaction_response("must not apply")),
            }
        } else {
            AgentCompactionEvent::Failed {
                pane_id: "%1".into(),
                task_generation: generation,
                usage: Default::default(),
                kind: "forbidden".into(),
                message: "compactor rejected".into(),
                provider_failure_json: None,
                provider_raw_text: None,
            }
        };
        let replacement_generation = service.pending_agent_compaction_task_generation("%1");
        assert_eq!(
            service
                .apply_agent_compaction_transition(event.clone())
                .unwrap()
                .applied,
            !stale
        );
        assert_eq!(
            service
                .agent_latest_request_usage(&conversation)
                .unwrap()
                .usage,
            fresh
        );
        assert_eq!(
            service
                .agent_context_usage_display(&conversation)
                .as_deref(),
            Some("10%")
        );
        assert_eq!(
            service.agent_token_usage_for_conversation(&conversation),
            before
        );
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            !service
                .apply_agent_compaction_transition(event)
                .unwrap()
                .applied
        );
        let records = store
            .load_agent_session_metadata(service.session.id.as_str())
            .unwrap();
        let record = records
            .iter()
            .find(|record| record.conversation_id == conversation)
            .unwrap();
        assert_eq!(record.latest_request_usage.as_ref().unwrap().usage, fresh);
        if stale {
            assert!(replacement_generation.is_some_and(|current| current > generation));
            assert_eq!(
                service.pending_agent_compaction_task_generation("%1"),
                replacement_generation
            );
            assert!(service.agent_is_compacting("%1"));
            assert!(!service.agent_compaction_task_is_claimed("%1", generation));
        }
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Deferred write errors retry only the immutable cleared metadata snapshot,
/// never provider work. Once a fresh execution supersedes that generation, a
/// repeated old failure cannot restore the cleared sample over the new one.
#[test]
fn local_compaction_usage_deferred_checkpoint_retry_keeps_exact_owner() {
    use crate::runtime::{PersistenceEvent, RuntimeSideEffect};

    let (mut service, store, turn_id) = local_usage_compaction();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.use_transcript_effect_adapter();
    complete_runtime_test_compaction(&mut service, "%1", "closed work summarized");
    assert_unknown(&mut service, &conversation);
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let effect = service
        .drain_transcript_persistence_transition()
        .side_effects
        .into_iter()
        .find(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PersistAgentSessionMetadata { .. }
            )
        })
        .unwrap();
    let RuntimeSideEffect::PersistAgentSessionMetadata { records, .. } = &effect else {
        unreachable!()
    };
    let record = records
        .iter()
        .find(|record| record.conversation_id == conversation)
        .unwrap();
    assert_eq!(record.context_usage_snapshot, None);
    assert_eq!(record.latest_request_usage, None);
    let failed = PersistenceEvent::AgentSessionMetadataFailed {
        effect: Box::new(effect),
        path: store.agent_session_metadata_path_for_tests(),
        error: "injected deferred checkpoint failure".into(),
    };
    let retry = service
        .apply_persistence_transition(failed.clone())
        .unwrap()
        .side_effects
        .into_iter()
        .find(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PersistAgentSessionMetadata { .. }
            )
        })
        .unwrap();
    let RuntimeSideEffect::PersistAgentSessionMetadata {
        records,
        retry_attempt,
        ..
    } = retry
    else {
        unreachable!()
    };
    assert_eq!(retry_attempt, 1);
    assert_eq!(
        records
            .iter()
            .find(|record| record.conversation_id == conversation)
            .unwrap()
            .latest_request_usage,
        None
    );
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    let fresh = mez_agent::ModelTokenUsage {
        input_tokens: 4_000,
        cached_input_tokens: Some(3_000),
        ..Default::default()
    };
    service.record_agent_provider_token_usage_with_profile("%1", fresh, fresh, Some(&profile));
    service.checkpoint_agent_session_metadata().unwrap();
    let current = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(current.iter().any(|effect| matches!(effect, RuntimeSideEffect::PersistAgentSessionMetadata { records, .. } if records.iter().any(|record| record.latest_request_usage.as_ref().is_some_and(|sample| sample.usage == fresh)))));
    let stale = service.apply_persistence_transition(failed).unwrap();
    assert!(!stale.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PersistAgentSessionMetadata { .. }
    )));
    assert_eq!(
        service
            .agent_latest_request_usage(&conversation)
            .unwrap()
            .usage,
        fresh
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    service.terminate_all_pane_processes().unwrap();
}
