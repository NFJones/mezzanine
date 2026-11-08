//! Cross-turn regressions for accepted, source-anchored local compaction.
//!
//! These fixtures run the production compaction completion and terminal
//! bookkeeping paths, then inspect new-turn and reopened-store replay rather
//! than treating a fitting same-turn continuation as durable continuity.

use super::*;

mod completion_owner;
/// Late same-group evidence keeps its original full-chronology occurrence.
mod occurrences;
mod receive_admission;
mod source_owner;
mod typed_source;

/// Mixed durable and pending completed tasks share one frozen source. Private
/// summaries acquire no authority; one final publication precedes terminal
/// append settlement, which must not create another head or selective writer.
#[test]
fn mixed_staged_recovery_publishes_only_at_distinct_terminal_handoff() {
    let (mut service, store, predecessor) = observed_history::prepared_with_cap(20_000, false);
    let conversation = service
        .agent_turn_ledger()
        .turn(&predecessor)
        .unwrap()
        .conversation_id
        .clone();
    let first = store.inspect(&conversation).unwrap();
    service.use_transcript_effect_adapter();
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &predecessor,
        &"MIXED_PENDING_SOURCE ".repeat(300),
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    typed_source::admit_bookkeeping(&mut service);
    let turn_id = service
        .start_agent_prompt_turn("%1", "EXACT_MIXED_SUCCESSOR")
        .unwrap()
        .turn_id;
    let pending = service
        .persistence
        .pending_transcript_entries(&conversation);
    assert!(!pending.is_empty());
    assert_eq!(store.inspect(&conversation).unwrap(), first);
    typed_source::queue(&mut service, &turn_id, false);
    let original = service.agent_turn_contexts()[&turn_id].clone();
    let baseline = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap();
    assert!(baseline.is_none());
    let rejected = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            )),
    );
    assert_eq!(rejected.len(), 1);
    let input = &rejected[0].messages.last().unwrap().content;
    assert!(input.contains("TYPED_OLD_WORK") && input.contains("MIXED_PENDING_SOURCE"));
    let mut responses = 0;
    for _ in 0..16 {
        if service.pending_agent_compaction_tasks().is_empty() {
            break;
        }
        let sent = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response("MIXED_FINAL_HEAD")),
        );
        responses += sent.len();
        assert_eq!(service.agent_turn_contexts()[&turn_id], original);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
    assert!(responses >= 2, "actual raw fragment and final carry");
    let completion = replay_publication::execute_pending_publication(&mut service);
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    let (_, accepted) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(accepted.revision().get(), 1);
    assert_eq!(
        accepted.context().chronology()[0].block().content,
        "MIXED_FINAL_HEAD"
    );
    assert!(
        service
            .terminal_compaction_handoff_content(&turn_id)
            .unwrap()
            .is_none()
    );
    service.stop_agent_turn_for_pane("%1").unwrap();
    typed_source::settle(&mut service);
    let archive = store.inspect(&conversation).unwrap();
    assert!(archive.starts_with(&first));
    assert!(
        archive
            .iter()
            .any(|row| row.content.contains("MIXED_PENDING_SOURCE"))
    );
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let replay = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(replay.head(), accepted.head());
    assert_eq!(replay.revision(), accepted.revision());
    assert_eq!(
        replay
            .context()
            .chronology()
            .iter()
            .filter(|event| event.block().content == "MIXED_FINAL_HEAD")
            .count(),
        1
    );
    assert!(
        !replay
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK")
                || block.content.contains("MIXED_PENDING_SOURCE"))
    );
}

/// Chooses the actual persisted import format before the real prompt replay.
/// Both formats retain their original source bytes when terminalized later.
fn live_fixture_with_source_codec(
    words: usize,
    durable_first: bool,
    accounted_history: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    live_fixture_with_source_store(words, durable_first, accounted_history, true)
}

/// Captures a genuine missing-store prompt when storage is intentionally absent.
fn live_fixture_with_source_store(
    words: usize,
    durable_first: bool,
    accounted_history: bool,
    with_store: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("terminal-live-handoff"));
    if with_store {
        service.set_agent_transcript_store(store.clone());
    }
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
                let content = if accounted_history {
                    mez_agent::transcript::TranscriptContextRecord::new(
                        event,
                        mez_agent::ContextBlockRecord::allocate(block).unwrap(),
                        None,
                    )
                    .unwrap()
                    .to_transcript_content()
                    .unwrap()
                } else {
                    event.to_transcript_content()
                };
                TranscriptEntry {
                    conversation_id: conversation.clone(),
                    sequence: index as u64 + 1,
                    created_at_unix_seconds: 1,
                    role: TranscriptRole::System,
                    turn_id: "historical-first".into(),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content,
                }
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

/// Retains the real dispatch boundary so fixtures may replace storage while
/// provider work is in flight without changing the accepted request source.
fn queue_live_compaction(service: &mut RuntimeSessionService, turn_id: &str, observed: bool) {
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
            .queue_agent_active_turn_compaction(turn_id, "live".into(), profile, trigger)
            .unwrap()
    );
}

/// Imported row images stay immutable across actual rewrites and ordinary
/// completion. The latest head keeps its original record/cost through cold
/// replay, while accounting and occurrence metadata never enter provider wire.
#[test]
fn terminal_handoff_preserves_exact_imported_and_live_accounted_sources() {
    for accounted in [false, true] {
        let (mut service, store, turn_id) = typed_source::saved(accounted);
        let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
        let original = store.inspect(&turn.conversation_id).unwrap();
        let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        let mut records = Vec::new();
        for summary in ["IMPORTED_SUMMARY", "LIVE_SUMMARY"] {
            typed_source::queue(&mut service, &turn_id, true);
            typed_source::complete(&mut service, &turn_id, summary);
            records.push(service.agent_turn_contexts()[&turn_id].chronology()[0].record());
        }
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &turn_id,
            "done",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        let archive = store.inspect(&turn.conversation_id).unwrap();
        assert!(archive.starts_with(&original));
        assert!(!archive.iter().any(|row| {
            row.content
                .starts_with(crate::storage::transcript::compaction_handoff::MARKER)
        }));
        service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
        let context = service
            .agent_context_for_pane_prompt("%1", "reopen summary records", 100)
            .unwrap();
        let summaries = context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::CompactionPrefix)
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].record(), records[1]);
        assert_eq!(summaries[0].block().content, "LIVE_SUMMARY");
        let request = crate::integrations::agent::context::assemble_model_request(
            &profile,
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            &turn,
            &context,
        )
        .unwrap();
        let wire = mez_agent::openai_responses_request_body(&request).unwrap();
        for record in records {
            let origin: String = record.occurrence_id().into();
            assert!(!wire.contains(&origin));
        }
        assert!(!wire.contains("rendered_sha256"));
        assert!(!wire.contains("framed_input_tokens"));
        let replay = service.runtime_agent_history_epoch_context("%1").unwrap();
        assert_eq!(
            replay
                .blocks
                .iter()
                .filter(|block| block.content == "LIVE_SUMMARY")
                .count(),
            1
        );
        assert!(
            !replay
                .blocks
                .iter()
                .any(|block| block.content == "IMPORTED_SUMMARY"
                    || block.content.contains("LIVE_ORIGINAL_SOURCE"))
        );
        assert_eq!(store.inspect(&turn.conversation_id).unwrap(), archive);
    }
}

/// A covered original row cannot be edited after actual summary publication.
/// Even valid revised accounted bodies preserving occurrence IDs must refuse
/// before changing source or authority, rather than fail only at later handoff.
#[test]
fn terminal_handoff_rejects_imported_source_edit_after_summary_acceptance() {
    for accounted in [false, true] {
        let (mut service, store, turn_id) = typed_source::saved(accounted);
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id
            .clone();
        let original = store.inspect(&conversation).unwrap();
        for summary in ["IMPORTED_SUMMARY", "LIVE_SUMMARY"] {
            typed_source::queue(&mut service, &turn_id, true);
            typed_source::complete(&mut service, &turn_id, summary);
        }
        let manifest = store
            .root()
            .join(&conversation)
            .join("compaction-epoch.json");
        let accepted = fs::read(&manifest).unwrap();
        let block = ContextBlock::assistant_event("same decision", "EDITED_IMPORTED_SOURCE");
        let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            block.source,
            block.label.clone(),
            block.content.clone(),
            mez_agent::ContextExecutionGroupId::new("live-group-0").unwrap(),
            1,
            None,
        )
        .unwrap();
        let changed = match mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
            &original[0].content,
        )
        .unwrap()
        {
            Some(record) => mez_agent::transcript::TranscriptContextRecord::new(
                event,
                record.record().revised(&block).unwrap(),
                None,
            )
            .unwrap()
            .to_transcript_content()
            .unwrap(),
            None => event.to_transcript_content(),
        };
        let error = store
            .compare_and_swap_entry_content(
                &conversation,
                original[0].sequence,
                &original[0].pane_id,
                &store.transcript_entry_revision(&original[0]).unwrap(),
                changed,
            )
            .unwrap_err();
        assert!(
            error
                .message()
                .contains("already represented by a compaction epoch"),
            "{error}"
        );
        assert_eq!(store.inspect(&conversation).unwrap(), original);
        assert_eq!(fs::read(&manifest).unwrap(), accepted);
        assert!(service.runtime_agent_history_epoch_context("%1").is_ok());
    }
}

/// Two real rewrites publish while the original ordinary archive is still
/// empty. Latest head and exact active input/steering survive turns B/C and a
/// reopened store. Unknown/below-threshold usage cannot itself queue compaction.
#[test]
fn completed_live_summaries_survive_turns_b_c_and_reopen() {
    for observed in [false, true] {
        let (mut service, store, turn_id) = typed_source::pending();
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id
            .clone();
        let original = service
            .persistence
            .pending_transcript_entries(&conversation);
        service
            .agent_turn_contexts_mut()
            .get_mut(&turn_id)
            .unwrap()
            .append_user_event("steering", "EXACT_LIVE_STEERING")
            .unwrap();
        let steering = service.agent_turn_contexts()[&turn_id]
            .chronology()
            .last()
            .unwrap()
            .clone();
        for (index, summary) in ["LIVE_SUMMARY_0", "LIVE_SUMMARY_1"].into_iter().enumerate() {
            typed_source::queue_at_attempt(&mut service, &turn_id, observed, index as u32 + 1);
            typed_source::complete(&mut service, &turn_id, summary);
            assert!(store.inspect(&conversation).unwrap().is_empty());
            assert_eq!(
                service
                    .persistence
                    .pending_transcript_entries(&conversation),
                original
            );
        }
        let (_, accepted) = store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(accepted.revision().get(), 2);
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &turn_id,
            "done",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        typed_source::settle(&mut service);
        let archive = store.inspect(&conversation).unwrap();
        assert!(archive.starts_with(&original));
        for row in &original {
            assert_eq!(
                archive.iter().filter(|candidate| *candidate == row).count(),
                1
            );
        }
        service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        for next in 2..=3 {
            let command = format!(
                r#"{{"jsonrpc":"2.0","id":"next-{next}","method":"agent/shell/command","params":{{"idempotency_key":"next-{next}","input":"NEXT_{next}"}}}}"#
            );
            let result = service.dispatch_runtime_control_body(&command, &primary);
            assert!(result.contains("turn_started"), "{result}");
            let id = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
            let context = &service.agent_turn_contexts()[&id];
            for value in [
                "LIVE_SUMMARY_1",
                "EXACT_LIVE_STEERING",
                "LIVE_INITIAL_PROMPT",
            ] {
                assert_eq!(
                    context
                        .chronology()
                        .iter()
                        .filter(|event| event.block().content == value)
                        .count(),
                    1
                );
            }
            assert!(
                !context
                    .blocks()
                    .iter()
                    .any(|block| block.content == "LIVE_SUMMARY_0"
                        || block.content.contains("LIVE_ORIGINAL_SOURCE"))
            );
            let head = context.chronology().first().unwrap();
            assert_eq!(head.record(), accepted.context().chronology()[0].record());
            let retained = context
                .chronology()
                .iter()
                .find(|event| event.record() == steering.record())
                .unwrap();
            assert_eq!(retained.block(), steering.block());
            assert_eq!(retained.sequence(), steering.sequence());
            assert!(head.sequence() < retained.sequence());
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_none()
            );
            let turn = service.agent_turn_ledger().turn(&id).unwrap().clone();
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
                            &runtime_model_request_fixture_for_agent(&id, &turn.agent_id),
                        )
                        .unwrap()
                );
            }
            crate::test_support::compaction_source::complete_task(
                &mut service,
                &id,
                "done",
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
            );
            typed_source::settle(&mut service);
        }
        assert!(store.inspect(&conversation).unwrap().starts_with(&archive));
    }
}

/// Intermediate execution persistence interleaves display and canonical rows.
/// Actual completed tasks, not reconstructed closed groups, must remain usable
/// through one summary publication, terminal settlement and reopened replay.
#[test]
fn interleaved_approval_history_remains_usable_after_terminalization() {
    let (mut service, store, original_turn) = observed_history::prepared_with_cap(20_000, false);
    let turn = service
        .agent_turn_ledger()
        .turn(&original_turn)
        .unwrap()
        .clone();
    let conversation = turn.conversation_id.clone();
    for index in 0..2 {
        let mut response = runtime_say_response(
            &original_turn,
            &format!("INTERLEAVED_PROGRESS_{index}"),
            false,
        );
        response.action_batch.as_mut().unwrap().actions[0].id = format!("progress-{index}");
        let profile = service
            .agent_turn_model_profile(&original_turn)
            .unwrap()
            .clone();
        let execution = service
            .execute_agent_turn_with_provider(
                &original_turn,
                &RuntimeBatchProvider { response },
                profile,
            )
            .unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        service
            .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
            .unwrap();
    }
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &original_turn,
        "interleaved task completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let archive = store.inspect(&conversation).unwrap();
    let display = archive
        .iter()
        .enumerate()
        .filter(|(_, row)| row.role == TranscriptRole::Assistant)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert!(display.len() >= 3);
    assert!(
        display.windows(2).any(
            |pair| archive[pair[0] + 1..pair[1]].iter().any(|row| matches!(
                mez_agent::TranscriptContextEvent::from_transcript_content(&row.content),
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock { .. })
            ))
        )
    );
    let turn_id = service
        .start_agent_prompt_turn("%1", "keep the successor exact")
        .unwrap()
        .turn_id;
    typed_source::queue(&mut service, &turn_id, false);
    typed_source::complete(&mut service, &turn_id, "JOINED_LIVE_SUMMARY");
    let (_, accepted) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(
        service
            .terminal_compaction_handoff_content(&turn_id)
            .unwrap()
            .is_none()
    );
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn_id,
        "successor completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    assert!(store.inspect(&conversation).unwrap().starts_with(&archive));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let replay = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(replay.head(), accepted.head());
    assert_eq!(replay.revision(), accepted.revision());
    assert_eq!(
        replay
            .context()
            .chronology()
            .iter()
            .filter(|event| event.block().content == "JOINED_LIVE_SUMMARY")
            .count(),
        1
    );
    assert!(
        !replay
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("INTERLEAVED_PROGRESS_")
                || block.content.contains("TYPED_OLD_WORK"))
    );
}

/// A genuine provider rejection publishes before interruption. Terminal append
/// settlement and a new prompt must retain the same head once, without another
/// rewrite or resurrection of covered source; ordinary audit remains append-only.
#[test]
fn provider_rejection_summary_survives_terminal_and_reopened_replay() {
    let (mut service, store, turn_id) = typed_source::pending();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let original = service
        .persistence
        .pending_transcript_entries(&conversation);
    typed_source::queue(&mut service, &turn_id, false);
    typed_source::complete(&mut service, &turn_id, "TERMINAL_ACCEPTED_SUMMARY");
    let (_, accepted) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(store.inspect(&conversation).unwrap().is_empty());
    service.stop_agent_turn_for_pane("%1").unwrap();
    assert_eq!(
        service.agent_turn_ledger().turn(&turn_id).unwrap().state,
        AgentTurnState::Interrupted
    );
    assert!(!service.agent_turn_contexts().contains_key(&turn_id));
    typed_source::settle(&mut service);
    let archive = store.inspect(&conversation).unwrap();
    assert!(archive.starts_with(&original));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    let next = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"next","method":"agent/shell/command","params":{"idempotency_key":"next","input":"NEXT_PROMPT"}}"#, &primary,
    );
    assert!(next.contains(r#""kind":"turn_started""#), "{next}");
    let next_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap_or_else(|| panic!("next prompt did not run: {next}"));
    let context = &service.agent_turn_contexts()[&next_id];
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "TERMINAL_ACCEPTED_SUMMARY")
            .count(),
        1
    );
    assert_eq!(
        context.chronology()[0].record(),
        accepted.context().chronology()[0].record()
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("LIVE_ORIGINAL_SOURCE"))
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert_eq!(store.inspect(&conversation).unwrap(), archive);
}
