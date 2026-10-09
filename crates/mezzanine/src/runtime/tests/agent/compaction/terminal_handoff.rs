//! Cross-turn regressions for accepted, source-anchored local compaction.
//!
//! These fixtures run the production compaction completion and terminal
//! bookkeeping paths, then inspect new-turn and reopened-store replay rather
//! than treating a fitting same-turn continuation as durable continuity.

use super::*;

/// Mixed staging must abandon its original selective projection even when the
/// archive already owns the first group. After all summaries fit and terminal
/// bookkeeping commits the remaining live group, a separate terminal witness
/// may prove both ranges without opportunistically publishing the old stage.
#[test]
fn mixed_staged_recovery_publishes_only_at_distinct_terminal_handoff() {
    let (mut service, store, turn_id) = live_fixture_with_source(1_200, true);
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let first = store.inspect(&conversation).unwrap();
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
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
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".into(), "17000".into());
    assert_eq!(profile.max_input_tokens(), Some(17000));
    service.set_agent_turn_model_profile(turn_id.clone(), profile.clone());
    assert!(service.queue_agent_active_turn_compaction(&turn_id, "live".into(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { observed_input_tokens: 20_000, max_input_tokens: 20_000 }, plan).unwrap());
    let mut responses = 0;
    let mut saw_abandoned_projection = false;
    while let Some(task) = service.pending_agent_compaction_task_for_tests("%1") {
        if let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
            staged: Some(staged),
            ..
        } = &task.target
        {
            saw_abandoned_projection |= staged.projection.is_none();
        }
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        complete_runtime_test_compaction(&mut service, "%1", &format!("MIXED_SUMMARY_{responses}"));
        assert!(
            service.agent_turn_is_running(&turn_id),
            "{}",
            service
                .pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n")
        );
        responses += 1;
        assert!(
            responses < 16,
            "synthetic compactor did not finish its bounded source"
        );
    }
    assert!(
        responses >= 2 && saw_abandoned_projection,
        "responses={responses} abandoned={saw_abandoned_projection}"
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let summaries = service
        .agent_turn_contexts()
        .get(&turn_id)
        .unwrap()
        .chronology()
        .iter()
        .filter(|event| event.block().label == "context compaction summary")
        .map(|event| event.block().content.clone())
        .collect::<Vec<_>>();
    complete_turn(&mut service, &turn_id);
    let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
    assert_eq!(
        epoch
            .ranges
            .iter()
            .map(|range| range.summary.clone())
            .collect::<Vec<_>>(),
        summaries
    );
    assert!(store.inspect(&conversation).unwrap().starts_with(&first));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let replay = service.runtime_agent_history_epoch_context("%1").unwrap();
    for summary in summaries {
        assert_eq!(
            replay
                .blocks
                .iter()
                .filter(|block| block.content == summary)
                .count(),
            1
        );
    }
    assert!(
        !replay
            .blocks
            .iter()
            .any(|block| block.content.contains("LIVE_ORIGINAL_SOURCE"))
    );
}

/// Creates a real prompt with two closed live groups and exact steering. These
/// groups have never reached an archive when the first summary is accepted.
fn live_fixture() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    live_fixture_with_words(200)
}

/// Allows the mixed staging regression to create genuine multi-range pressure.
fn live_fixture_with_words(words: usize) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    live_fixture_with_source(words, false)
}

/// Seeds optional committed history before prompt assembly, keeping imported
/// event ownership distinct from the later live group and current prompt.
fn live_fixture_with_source(
    words: usize,
    durable_first: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("terminal-live-handoff"));
    service.set_agent_transcript_store(store.clone());
    service.replace_config_layers(vec![ConfigLayer {
        name: "terminal-live".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"live\"\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.live]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    if durable_first {
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let blocks = [
            ContextBlock::assistant_event("same decision", "LIVE_ORIGINAL_SOURCE ".repeat(words)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "same result",
                "settled result",
            ),
        ];
        let rows = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::System,
                turn_id: "historical-first".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                    block.source,
                    block.label.clone(),
                    block.content.clone(),
                    mez_agent::ContextExecutionGroupId::new("live-group-0").unwrap(),
                    index as u64 + 1,
                    None,
                )
                .unwrap()
                .to_transcript_content(),
            })
            .collect::<Vec<_>>();
        store.append_many(&rows).unwrap();
        service
            .agent_shell_store_mut()
            .record_transcript_entries("%1", rows.len())
            .unwrap();
    }
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live","method":"agent/shell/command","params":{"idempotency_key":"live","input":"LIVE_INITIAL_PROMPT"}}"#, &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn_id = "turn-1".to_string();
    let context = service.agent_turn_contexts_mut().get_mut(&turn_id).unwrap();
    for index in 0..2 {
        if durable_first && index == 0 {
            continue;
        }
        if index > 0 {
            context
                .append_user_event("steering", "EXACT_LIVE_STEERING")
                .unwrap();
        }
        let group = mez_agent::ContextExecutionGroupId::new(format!("live-group-{index}")).unwrap();
        context
            .append_assistant_event(
                "same decision",
                "LIVE_ORIGINAL_SOURCE ".repeat(words),
                group.clone(),
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "same result",
                "settled result",
                group,
                None,
                true,
            )
            .unwrap();
    }
    (service, store, turn_id)
}

/// Runs one model-backed compaction through its actual runtime completion path.
fn compact_live(service: &mut RuntimeSessionService, turn_id: &str, summary: &str, observed: bool) {
    let context = service.agent_turn_contexts().get(turn_id).unwrap();
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
    let profile = service.agent_turn_model_profile(turn_id).unwrap().clone();
    let trigger = if observed {
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000,
            max_input_tokens: 20_000,
        }
    } else {
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
            attempt: 1,
        }
    };
    assert!(
        service
            .queue_agent_active_turn_compaction(turn_id, "live".into(), profile, trigger, plan)
            .unwrap()
    );
    complete_runtime_test_compaction(service, "%1", summary);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
}

/// Persists a terminal execution and then uses ordinary lifecycle cleanup. No
/// source action is re-executed while capturing its canonical transcript.
fn complete_turn(service: &mut RuntimeSessionService, turn_id: &str) {
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .unwrap()
        .clone();
    let response = runtime_say_response(turn_id, "done", true);
    let action = response.action_batch.as_ref().unwrap().actions[0].clone();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(turn_id, &turn.agent_id),
        response,
        latest_response_usage: mez_agent::ModelTokenUsage::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["done".into()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
        .unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &turn,
            AgentTurnState::Completed,
            "terminal_handoff_regression",
        )
        .unwrap();
}

/// Two accepted live summaries survive completed turn A, fresh turns B/C and
/// reopening. Original groups are durably archived exactly once, exact steering
/// remains ordered, and neither unknown nor below-threshold fresh usage can
/// queue proactive compaction merely because A compacted.
#[test]
fn completed_live_summaries_survive_turns_b_c_and_reopen() {
    for observed in [false, true] {
        let (mut service, store, turn_id) = live_fixture();
        compact_live(&mut service, &turn_id, "LIVE_SUMMARY_0", observed);
        compact_live(&mut service, &turn_id, "LIVE_SUMMARY_1", observed);
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        complete_turn(&mut service, &turn_id);
        let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
        assert_eq!(epoch.ranges.len(), 2);
        let archive = store.inspect(&conversation).unwrap();
        assert_eq!(
            archive
                .iter()
                .filter(|row| matches!(
                    mez_agent::TranscriptContextEvent::from_transcript_content(&row.content),
                    Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                        execution_group_id: Some(_),
                        ..
                    })
                ))
                .count(),
            4
        );
        service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        for next in 2..=3 {
            let command = format!(
                r#"{{"jsonrpc":"2.0","id":"next-{next}","method":"agent/shell/command","params":{{"idempotency_key":"next-{next}","input":"NEXT_{next}"}}}}"#
            );
            let result = service.dispatch_runtime_control_body(&command, &primary);
            assert!(result.contains("turn_started"), "{result}");
            let id = format!("turn-{next}");
            let context = service.agent_turn_contexts().get(&id).unwrap();
            let contents = context
                .chronology()
                .iter()
                .map(|event| event.block().content.as_str())
                .collect::<Vec<_>>();
            for summary in [
                "LIVE_SUMMARY_0",
                "LIVE_SUMMARY_1",
                "EXACT_LIVE_STEERING",
                "LIVE_INITIAL_PROMPT",
            ] {
                assert_eq!(
                    contents
                        .iter()
                        .filter(|content| **content == summary)
                        .count(),
                    1,
                    "{contents:?}"
                );
            }
            assert!(
                contents
                    .iter()
                    .position(|content| *content == "LIVE_SUMMARY_0")
                    .unwrap()
                    < contents
                        .iter()
                        .position(|content| *content == "EXACT_LIVE_STEERING")
                        .unwrap()
            );
            assert!(
                !contents
                    .iter()
                    .any(|content| content.contains("LIVE_ORIGINAL_SOURCE"))
            );
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_none()
            );
            let turn = service
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == id)
                .unwrap()
                .clone();
            let profile = service.agent_turn_model_profile(&id).unwrap().clone();
            for input_tokens in [0, 19_999] {
                assert!(
                    !service
                        .defer_agent_provider_for_observed_input_limit(
                            &turn,
                            &profile,
                            mez_agent::ModelTokenUsage {
                                input_tokens,
                                ..Default::default()
                            },
                            &runtime_model_request_fixture_for_agent(&id, &turn.agent_id)
                        )
                        .unwrap()
                );
            }
            complete_turn(&mut service, &id);
        }
        assert!(store.inspect(&conversation).unwrap().starts_with(&archive));
    }
}

/// Intermediate approval persistence can separate adjacent live groups with
/// display rows in the archive. Such a valid but unmappable layout must be
/// reported before certificate admission, never poison future/reopened replay.
#[test]
fn interleaved_approval_history_remains_usable_after_terminalization() {
    let (mut service, store, turn_id) = live_fixture();
    // Remove the exact steering barrier so one accepted summary covers both groups.
    let context = service.agent_turn_contexts_mut().get_mut(&turn_id).unwrap();
    let blocks = context
        .blocks()
        .iter()
        .filter(|block| block.label != "steering")
        .cloned()
        .collect();
    context.replace_after_compaction(blocks).unwrap();
    // Re-import canonical ownership with the original immutable group identities.
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let chronology = context
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id().is_some())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(chronology.len(), 4);
    let mut rows = Vec::new();
    for (index, event) in chronology.iter().enumerate() {
        if index == 2 {
            rows.push(TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence: rows.len() as u64 + 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::Assistant,
                turn_id: turn_id.clone(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "intermediate display".into(),
            });
        }
        let block = event.block();
        rows.push(TranscriptEntry {
            conversation_id: conversation.clone(),
            sequence: rows.len() as u64 + 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: turn_id.clone(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                block.source,
                block.label.clone(),
                block.content.clone(),
                event.execution_group_id().unwrap().clone(),
                (index % 2) as u64 + 1,
                event.provider_owner().cloned(),
            )
            .unwrap()
            .to_transcript_content(),
        });
    }
    store.append_many(&rows).unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", rows.len())
        .unwrap();
    compact_live(&mut service, &turn_id, "JOINED_LIVE_SUMMARY", false);
    complete_turn(&mut service, &turn_id);
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(
        store
            .inspect(&conversation)
            .unwrap()
            .iter()
            .any(|row| row.content.contains("noncontiguous_archive_source"))
    );
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    assert!(service.runtime_agent_history_epoch_context("%1").is_ok());
}

/// Provider rejection deliberately recovers turn-locally. Once the owning
/// turn terminates, its exact typed source must support an independent durable
/// handoff: subsequent turns must see the summary once, not resurrect the raw
/// group. The archive itself must remain byte-for-byte append-only.
#[test]
fn provider_rejection_summary_survives_terminal_and_reopened_replay() {
    let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { trigger, .. } =
        &mut task.target
    else {
        panic!("expected active compaction")
    };
    *trigger =
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
            attempt: 1,
        };
    service.claim_agent_compaction_task_state("%1", task);
    service
        .apply_agent_compaction_completed_event(
            "%1",
            runtime_test_compaction_response("TERMINAL_ACCEPTED_SUMMARY"),
        )
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .unwrap()
        .clone();
    let original = store.inspect(&turn.conversation_id).unwrap();
    assert!(
        store
            .compaction_epoch(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
    service
        .finish_agent_turn("%1", &turn_id, AgentTurnState::Interrupted)
        .unwrap();
    assert!(!service.agent_turn_contexts().contains_key(&turn_id));
    let after = store.inspect(&turn.conversation_id).unwrap();
    assert!(after.starts_with(&original));
    let epoch = store.compaction_epoch(&turn.conversation_id).unwrap();
    assert!(
        epoch.is_some(),
        "accepted local summary was lost at terminalization"
    );
    // Reopen the filesystem-backed store, discarding the original handle.
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    let next = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"next","method":"agent/shell/command","params":{"idempotency_key":"next","input":"NEXT_PROMPT"}}"#,
        &primary,
    );
    assert!(next.contains(r#""kind":"turn_started""#), "{next}");
    let context = service.agent_turn_contexts().get("turn-2").unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "TERMINAL_ACCEPTED_SUMMARY")
            .count(),
        1
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
}
