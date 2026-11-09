//! Execution display invalidation belongs to an accepted context replacement,
//! not auxiliary expense or continuation diagnostics. An actual earlier task
//! supplies raw source; display changes require the canonical storage receipt.

use super::*;
use crate::runtime::AgentCompactionEvent;

/// Completes an earlier task with an offline provider, then admits its actual
/// successor. The old instruction becomes eligible through real task admission;
/// no sealed chronology is replaced to fabricate source or execution closure.
fn local_usage_compaction(
    consumed_sample: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    local_usage_compaction_with_mode(consumed_sample, false)
}

fn local_usage_compaction_with_mode(
    consumed_sample: bool,
    raw: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("local-compaction-usage"));
    service.set_agent_transcript_store(store.clone());
    service.replace_config_layers(vec![ConfigLayer {
        name: "local-compaction-usage".into(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"local-usage\"\nshell_mode = \"pane\"\ncompaction_raw_retention_percent = 1\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.local-usage]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 20000\n".into(),
    }]).unwrap();
    if raw {
        raw_tail::enable_raw(&mut service);
    }
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let earlier = service
        .start_agent_prompt_turn("%1", &"completed earlier task ".repeat(250))
        .unwrap();
    let profile = service
        .agent_turn_model_profile(&earlier.turn_id)
        .unwrap()
        .clone();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(&earlier.turn_id, "earlier task completed", true),
    };
    let execution = service
        .execute_agent_turn_with_provider(&earlier.turn_id, &provider, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    // Pane-local expense requires the actual current process incarnation. Start
    // it after the old task so its real environment remains a protected suffix,
    // while the earlier user occurrence supplies the eligible prefix.
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"local-usage","method":"agent/shell/command","params":{"idempotency_key":"local-usage","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
    assert!(context.has_replay_owner());
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        20_000,
        0,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    assert!(plan.changes_context());
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    let sample = mez_agent::ModelTokenUsage {
        input_tokens: 20_000,
        output_tokens: 40,
        reasoning_tokens: 5,
        cached_input_tokens: Some(10_000),
        cache_write_input_tokens: Some(50),
    };
    service.record_agent_provider_token_usage_with_profile("%1", sample, sample, Some(&profile));
    if consumed_sample {
        let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
        let request = crate::integrations::agent::context::assemble_model_request(
            &profile,
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            &turn,
            &service.agent_turn_contexts()[&turn_id],
        )
        .unwrap();
        service.retain_agent_provider_request_chain(&turn, request.clone());
        assert!(
            service
                .defer_agent_provider_for_observed_input_limit(&turn, &profile, sample, &request)
                .unwrap()
        );
    } else {
        assert!(service.queue_agent_active_turn_compaction(
        &turn_id, "local-usage".into(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000, max_input_tokens: 20_000,
        },
        ).unwrap());
    }
    service.checkpoint_agent_session_metadata().unwrap();
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .replay_source
            .is_some()
    );
    (service, store, turn_id)
}

/// Runs the actual frozen storage attempt and original callback. Auxiliary
/// metadata effects retain their ordering/coalescing owner; neither the model
/// response nor physical publication alone may clear the ordinary usage sample.
fn publish_usage_compaction(service: &mut RuntimeSessionService) {
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let before = service.agent_context_usage_display(&conversation);
    assert!(before.is_some());
    let completion = super::replay_publication::execute_pending_publication(service);
    assert_eq!(service.agent_context_usage_display(&conversation), before);
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
}

/// A failed physical attempt and a successful write alone retain live request
/// state. Only acceptance resets the baseline/lineage and revokes old retrieval
/// availability, preserving exact evidence, expense and the consumed sample.
/// Repeated delivery cannot revoke fresh retrieval or clear a newer baseline.
#[test]
fn local_compaction_acceptance_resets_request_cache_and_grants_once() {
    acceptance_resets_request_cache_and_grants_once(false);
}

/// Raw mode must invalidate the same request/MCP/usage owners only after an
/// accepted receipt, with no auxiliary expense and no model completion event.
#[test]
fn local_raw_tail_compaction_acceptance_resets_request_cache_and_grants_once() {
    acceptance_resets_request_cache_and_grants_once(true);
}

fn acceptance_resets_request_cache_and_grants_once(raw: bool) {
    let (mut service, store, turn_id) = local_usage_compaction_with_mode(true, raw);
    let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
    let conversation = turn.conversation_id.clone();
    assert!(service.agent_turn_has_consumed_observed_input_for_tests(&turn_id));
    assert!(service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    let grant = |service: &mut RuntimeSessionService, id: &str| {
        // Public discovery-result rendering supplies scope-bound availability;
        // this fixture executes no MCP call and mints no native/effect receipt.
        let result = mez_agent::ActionResult {
            protocol: "maap/1".into(), turn_id: turn_id.clone(), agent_id: turn.agent_id.clone(),
            action_id: id.into(), action_type: "mcp_server_get", status: mez_agent::ActionStatus::Succeeded,
            content: Vec::new(), structured_content_json: Some(serde_json::json!({"server":{
                "server_id":"availability-fixture", "state":"available", "display_name":"Fixture",
                "purpose":"acceptance coverage", "usage_instructions":"Use echo", "tools":[{
                    "name":"echo", "state":"available", "description":"fixture echo",
                    "input_schema":{"type":"object","properties":{}}
                }]
            }}).to_string()), permission_evaluation: None, is_error: false, error: None,
        };
        let context = service.agent_turn_contexts_mut().get_mut(&turn_id).unwrap();
        let (server, content) =
            mez_agent::mcp_retrieved_manifest_for_action_result(&result, context).unwrap();
        let group = mez_agent::ContextExecutionGroupId::new(format!("retrieval-{id}")).unwrap();
        context
            .append_assistant_event(
                "MCP fixture lookup",
                "retrieve availability metadata",
                group.clone(),
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::McpRetrievedManifest,
                format!(
                    "{}{server}",
                    mez_agent::MCP_RETRIEVED_MANIFEST_CONTEXT_LABEL_PREFIX
                ),
                content,
                group,
                None,
                true,
            )
            .unwrap();
        context.chronology().last().unwrap().clone()
    };
    let available = |service: &RuntimeSessionService| {
        mez_agent::invoked_mcp_tools_for_context(
            &service.agent_turn_contexts()[&turn_id],
            &mez_agent::McpPromptSummary {
                available_servers: Vec::new(),
                available_tools: Vec::new(),
                unavailable_servers: Vec::new(),
            },
        )
    };
    let original = grant(&mut service, "before-rewrite");
    assert_eq!(available(&service).len(), 1);
    let before = service.agent_turn_contexts()[&turn_id].clone();
    let lineage = before.metadata().prompt_cache_lineage_id.clone();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let generation = task.task_generation;
    if raw {
        assert!(task.request.is_none());
        assert!(
            service
                .claim_agent_compaction_task("%1", generation)
                .unwrap()
                .is_none()
        );
        assert!(!service.retains_compaction_expense_for_tests("%1", generation));
    } else {
        complete_runtime_test_compaction(&mut service, "%1", "accepted replacement");
    }
    let expense = service.agent_token_usage_for_conversation(&conversation);
    assert_eq!(service.agent_turn_contexts()[&turn_id], before);
    assert_eq!(available(&service).len(), 1);
    assert!(service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    store.inject_replay_epoch_fault(
        crate::storage::transcript::replay_epoch::Phase::BeforeManifestSwap,
        1,
        false,
    );
    let failed = service
        .take_replay_epoch_attempt_for_tests()
        .unwrap()
        .execute();
    assert!(failed.error().is_some());
    service.apply_replay_epoch_completion(failed).unwrap();
    assert_eq!(service.agent_turn_contexts()[&turn_id], before);
    assert_eq!(available(&service).len(), 1);
    assert!(service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    assert!(service.agent_turn_has_consumed_observed_input_for_tests(&turn_id));
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        expense
    );
    service
        .poll_replay_epoch_publication("%1", generation)
        .unwrap();
    let committed = replay_publication::execute_pending_publication(&mut service);
    assert!(committed.error().is_none());
    assert_eq!(service.agent_turn_contexts()[&turn_id], before);
    assert_eq!(available(&service).len(), 1);
    assert!(service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    service
        .apply_replay_epoch_completion(committed.clone())
        .unwrap();
    let accepted = service.agent_turn_contexts()[&turn_id].clone();
    assert_ne!(accepted.metadata().prompt_cache_lineage_id, lineage);
    assert_eq!(
        accepted.metadata().prompt_cache_lineage_id,
        Some(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .prompt_cache_lineage_id
                .clone()
        )
    );
    assert!(!service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    assert!(available(&service).is_empty());
    assert_eq!(
        accepted
            .chronology()
            .iter()
            .find(|event| event.record().occurrence_id() == original.record().occurrence_id()),
        Some(&original)
    );
    assert!(service.agent_turn_has_consumed_observed_input_for_tests(&turn_id));
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        expense
    );
    let (_, cold) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(
        mez_agent::invoked_mcp_tools_for_context(
            cold.context(),
            &mez_agent::McpPromptSummary {
                available_servers: Vec::new(),
                available_tools: Vec::new(),
                unavailable_servers: Vec::new(),
            }
        )
        .is_empty()
    );
    assert!(cold.context().chronology().contains(&original));
    let fresh = grant(&mut service, "after-rewrite");
    assert_ne!(
        fresh.record().occurrence_id(),
        original.record().occurrence_id()
    );
    assert_eq!(available(&service).len(), 1);
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &service.agent_turn_contexts()[&turn_id],
    )
    .unwrap();
    service.retain_agent_provider_request_chain(&turn, request);
    assert!(
        !service
            .apply_replay_epoch_completion(committed)
            .unwrap()
            .applied
    );
    assert!(service.agent_conversation_has_provider_request_chain_for_tests(&conversation));
    assert_eq!(available(&service).len(), 1);
    assert_eq!(
        service.agent_turn_contexts()[&turn_id]
            .metadata()
            .prompt_cache_lineage_id,
        accepted.metadata().prompt_cache_lineage_id
    );
    assert!(service.agent_turn_has_consumed_observed_input_for_tests(&turn_id));
    assert_eq!(
        service.agent_token_usage_for_conversation(&conversation),
        expense
    );
    service.terminate_all_pane_processes().unwrap();
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

/// A successful accepted replacement clears positive ordinary context/cache
/// samples and checkpoints unknown without resetting any cumulative expense.
/// Auxiliary expense and omitted/zero ordinary usage cannot revive the sample;
/// a later positive ordinary response owns the new display and cache ratio.
#[test]
fn local_compaction_usage_success_clears_display_and_metadata() {
    let (mut service, store, turn_id) = local_usage_compaction(false);
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
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let generation = task.task_generation;
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
        response: Box::new(response.clone()),
    };
    run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
    publish_usage_compaction(&mut service);
    assert_unknown(&mut service, &conversation);
    let (_, published) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(published.revision().get(), 1);
    assert_eq!(
        published.context().chronology()[0].block().content,
        "closed work summarized"
    );
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
    let (mut service, store, turn_id) = local_usage_compaction(false);
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let generation = task.task_generation;
    store.fail_next_agent_session_metadata_write();
    let event = AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(runtime_test_compaction_response("closed work summarized")),
    };
    complete_runtime_test_compaction(&mut service, "%1", "closed work summarized");
    publish_usage_compaction(&mut service);
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
        let (mut service, store, turn_id) = local_usage_compaction(false);
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

    let (mut service, store, turn_id) = local_usage_compaction(false);
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.use_transcript_effect_adapter();
    complete_runtime_test_compaction(&mut service, "%1", "closed work summarized");
    publish_usage_compaction(&mut service);
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
