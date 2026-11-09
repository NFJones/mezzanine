//! Runtime tests for agent compaction behavior.

use super::*;
use crate::runtime::{current_unix_seconds, execute_runtime_agent_prompt_history_work};
use mez_agent::messaging::{Envelope, MessageScope};

mod failure_diagnostics;
mod first_turn;
mod legacy_raw;
mod manual_commands;
mod manual_history;
mod manual_model;
mod observed_continuation;
mod observed_fit;
mod observed_history;
mod observed_staging;
mod parity;
mod preparation;
mod prompt_display;
mod provider_backoff;
mod provider_continuity;
mod provider_limit_replay;
mod provider_recovery;
mod raw_continuity;
mod raw_tail;
mod reentry;
mod replay_compatibility;
mod replay_copies;
mod replay_observed;
mod replay_publication;
mod restored_history;
mod routed_copies;
mod single_head;
mod summary_correction;
mod terminal_handoff;
mod transport_retries;
mod usage_invalidation;

/// Legacy reference history joins the same private reduction and single-head
/// publication, with or without an already imported prefix summary.
#[test]
fn runtime_staged_compaction_legacy_range_recovers_turn_locally() {
    observed_staging::legacy_source_survives_publication(false);
    observed_staging::legacy_source_survives_publication(true);
}

/// An external epoch change after private inference rejects publication and
/// cannot install stale summaries or queue a provider continuation.
#[test]
fn runtime_staged_turn_local_compaction_rejects_changed_epoch() {
    observed_staging::changed_epoch_rejects_publication();
}

/// Later live work has no durable row yet. It remains raw behind its current
/// user root while private reduction publishes only the frozen closed prefix.
#[test]
fn runtime_staged_compaction_live_range_recovers_turn_locally() {
    observed_staging::pending_live_source_survives_publication();
}

/// Verifies the runtime applies raw-retention config for compaction recovery.
///
/// Provider context-limit recovery and manual compaction both use the
/// raw-retention percentage to decide how much exact recent context remains
/// after compaction.
#[test]
fn runtime_config_reload_applies_compaction_raw_retention() {
    let mut service = test_runtime_service();

    assert_eq!(service.agent_compaction_raw_retention_percent(), 10);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ncompaction_raw_retention_percent = 25\n".to_string(),
        }])
        .unwrap();

    assert_eq!(service.agent_compaction_raw_retention_percent(), 25);
}

/// Reload changes future mode selection and removing the key restores summary.
#[test]
fn runtime_config_reload_applies_compaction_mode() {
    let mut service = test_runtime_service();
    assert_eq!(service.agent_compaction_mode().name(), "summary");
    for text in ["[agents]\ncompaction_mode=\"raw_tail\"\n", "[agents]\n"] {
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "mode".into(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: text.into(),
            }])
            .unwrap();
        assert_eq!(
            service.agent_compaction_mode().name(),
            if text.contains("raw_tail") {
                "raw_tail"
            } else {
                "summary"
            }
        );
    }
}

/// A byte-heavy direct-user instruction cannot become summary input merely
/// because the earlier word-budgeted range leaves the full request oversized.
/// Exhaustion must settle boundedly without a continuation or partial epoch,
/// preserving the original durable transcript rather than truncating the prompt.
#[test]
fn runtime_provider_compaction_does_not_summarize_byte_heavy_exact_history() {
    let (mut service, store, turn_id) = observed_history::queued();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let original_rows = store.inspect(&conversation_id).unwrap();
    let exact = "x".repeat(200_000);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("exact retained instruction", exact.clone())
        .unwrap();
    let before = service.agent_turn_contexts()[&turn_id].clone();
    let retained = before.chronology().last().unwrap().clone();
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", "short summary")
            .unwrap()
            > 20_000
    );
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("short summary")),
    );
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|message| message.content.contains(&exact))
    );
    assert_eq!(service.agent_turn_contexts()[&turn_id], before);
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    replay_observed::adopt(&mut service, &turn_id);
    replay_observed::assert_head(&service, &store, &turn_id, "short summary");
    assert_eq!(
        service.agent_turn_ledger().turn(&turn_id).unwrap().state,
        AgentTurnState::Running
    );
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(
        service.agent_turn_contexts()[&turn_id]
            .chronology()
            .contains(&retained)
    );
    assert_eq!(store.inspect(&conversation_id).unwrap(), original_rows);
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
}

/// Saved-history replay must allow another compaction range after an earlier
/// summary leaves byte-heavy evidence above the provider's token allowance.
/// Persist synthetic closed execution groups around exact user barriers so the
/// regression is hermetic on Linux and macOS, preserves causal ownership, and
/// never reads or modifies a developer's private saved conversations.
#[test]
fn runtime_saved_history_compaction_replans_retained_byte_heavy_evidence() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = temp_root("saved-history-compaction-replanning");
    let store = AgentTranscriptStore::new(root.clone());
    let conversation_id = "synthetic-compaction-history";
    let first_group = mez_agent::ContextExecutionGroupId::new("first-history-group").unwrap();
    let second_group = mez_agent::ContextExecutionGroupId::new("second-history-group").unwrap();
    let execution_block = |source, label, content, group, ordinal| {
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            source, label, content, group, ordinal, None,
        )
        .unwrap()
        .to_transcript_content()
    };
    let entries = [
        (
            TranscriptRole::User,
            "Keep the original task exact".to_string(),
            "first-turn",
        ),
        (
            TranscriptRole::System,
            execution_block(
                ContextSourceKind::TranscriptAssistant,
                "first decision",
                "first decision".to_string(),
                first_group.clone(),
                1,
            ),
            "first-turn",
        ),
        (
            TranscriptRole::System,
            execution_block(
                ContextSourceKind::ActionResult,
                "first result",
                "first ".repeat(4_000),
                first_group.clone(),
                2,
            ),
            "first-turn",
        ),
        (
            TranscriptRole::User,
            "Preserve the exact later instruction".to_string(),
            "second-turn",
        ),
        (
            TranscriptRole::System,
            execution_block(
                ContextSourceKind::TranscriptAssistant,
                "second decision",
                "second decision".to_string(),
                second_group.clone(),
                1,
            ),
            "second-turn",
        ),
        (
            TranscriptRole::System,
            execution_block(
                ContextSourceKind::ActionResult,
                "byte-heavy result",
                "x".repeat(200_000),
                second_group.clone(),
                2,
            ),
            "second-turn",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (role, content, turn_id))| TranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence: u64::try_from(index + 1).unwrap(),
        created_at_unix_seconds: 1,
        role,
        turn_id: turn_id.to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content,
    })
    .collect::<Vec<_>>();
    store.append_many(&entries).unwrap();
    let entries = store.inspect(conversation_id).unwrap();
    let history = crate::runtime::control::runtime_agent_transcript_context("%1", &entries);
    assert_eq!(history.execution_events.len(), 4);
    let mut context = history.records.import_context(history.blocks).unwrap();
    context
        .restore_imported_execution_events(&history.execution_events)
        .unwrap();
    let high_water = context.event_sequence_high_water_mark();
    let projection = mez_agent::ProviderBudgetProjection::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "openai",
    );
    let plan = mez_agent::plan_model_context_compaction_for_provider(
        &context, 22_500, 10, high_water, projection,
    )
    .unwrap();
    assert!(plan.changes_context());
    assert!(!plan.requires_additional_segments());
    assert_eq!(plan.replacement_blocks().len(), 2);
    assert!(
        context
            .chronology()
            .iter()
            .filter(|event| {
                plan.replacement_event_sequences()
                    .contains(&event.sequence())
            })
            .all(|event| event.execution_group_id() == Some(&first_group))
    );
    let (candidate, _) =
        mez_agent::apply_model_context_compaction_plan(context, &plan, "synthetic short summary")
            .unwrap();
    let optional: usize = candidate
        .blocks()
        .iter()
        .zip(mez_agent::projected_context_block_input_tokens(
            &candidate,
            Some(projection),
        ))
        .filter(|(block, _)| {
            block.retention() != mez_agent::ContextRetention::Exact
                && block.source != ContextSourceKind::TranscriptUser
        })
        .map(|(_, cost)| cost)
        .sum();
    assert!(optional > 28_500);
    let next = mez_agent::plan_model_context_compaction_for_provider_tokens(
        &candidate, 28_500, 0, high_water, projection,
    )
    .unwrap();
    assert!(next.changes_context());
    assert_eq!(next.replacement_blocks().len(), 2);
    assert!(
        candidate
            .chronology()
            .iter()
            .filter(|event| {
                next.replacement_event_sequences()
                    .contains(&event.sequence())
            })
            .all(|event| event.execution_group_id() == Some(&second_group))
    );
    let (compacted, _) = mez_agent::apply_model_context_compaction_plan(
        candidate,
        &next,
        "remaining evidence summarized",
    )
    .unwrap();
    let contents = compacted
        .chronology()
        .iter()
        .map(|event| event.block().content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        contents,
        [
            "Keep the original task exact",
            "synthetic short summary",
            "Preserve the exact later instruction",
            "remaining evidence summarized",
        ]
    );
    let tokens: usize =
        mez_agent::projected_context_block_input_tokens(&compacted, Some(projection))
            .into_iter()
            .sum();
    assert!(tokens < 28_500);
    assert_eq!(store.inspect(conversation_id).unwrap(), entries);
    fs::remove_dir_all(root).unwrap();
}

/// Verifies prompt submission does not run fallback context accounting before
/// appending prompt-derived state.
///
/// Provider responses and provider context-limit errors are the source of truth
/// for context-size handling, so prompt submission must start the turn even when
/// a local estimate would have crossed the model window.
#[test]
fn runtime_agent_prompt_does_not_preflight_compact_before_context_append() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-preflight-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-preflight-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-preflight-test"]
default_model = "gpt-compact-preflight-test"
[model_profiles.compact-preflight-test]
provider = "openai"
model = "gpt-compact-preflight-test"
context_window_tokens = 1024
max_input_tokens = 1
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-preflight"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "as-preflight".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-previous".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("large prior context {}", "context-pressure ".repeat(900)),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-preflight", 1)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "continue with the next item")
        .unwrap();

    assert!(response.contains(r#""state":"running""#), "{response}");
    assert!(
        !response.contains(r#""kind":"requires_runtime""#),
        "{response}"
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        transcript_store.prompt_history("as-preflight").unwrap(),
        vec!["continue with the next item".to_string()]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("continue with the next item"),
        "{pane_text}"
    );
}

/// Two observed-input rewrites span actual completed tasks. The next rewrite
/// consumes the previous typed head and newly closed source exactly once, while
/// inference and physical publication remain separate from accepted adoption.
#[test]
fn runtime_observed_compaction_extends_previous_selective_epoch() {
    let (mut service, store, mut turn_id) = provider_limit_replay::recovery_source(
        &format!("FIRST_OBSERVED_SOURCE {}", "original ".repeat(5_000)),
        "first task completed",
        1,
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let mut prior_head = None;
    for (revision, summary) in [
        (1, "first observed summary"),
        (2, "second observed summary"),
    ] {
        let before = service.agent_turn_contexts()[&turn_id].clone();
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            before.clone(),
        )
        .unwrap();
        let selected = match owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 1).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected closed observed source: {other:?}"),
        };
        let expected_tail = before.chronology()[selected.summary_input().len()..].to_vec();
        if let Some(head) = prior_head {
            assert_eq!(selected.summary_input()[0].record().occurrence_id(), head);
            assert!(
                selected
                    .summary_input()
                    .iter()
                    .any(|event| event.block().content.contains("SECOND_OBSERVED_SOURCE"))
            );
            assert!(
                !selected
                    .summary_input()
                    .iter()
                    .any(|event| event.block().content.contains("FIRST_OBSERVED_SOURCE"))
            );
        }
        let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        assert!(service.queue_agent_active_turn_compaction(
            &turn_id, "provider-context-limit-test".into(), profile,
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: 40_000, max_input_tokens: 40_000,
            },
        ).unwrap());
        let input = crate::test_support::compaction_source::rendered_source(
            service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(input.contains("FIRST_OBSERVED_SOURCE"), revision == 1);
        if revision == 2 {
            assert_eq!(input.matches("first observed summary").count(), 1);
            assert!(input.contains("SECOND_OBSERVED_SOURCE"));
        }
        complete_runtime_test_compaction(&mut service, "%1", summary);
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        let completion = replay_publication::execute_pending_publication(&mut service);
        let duplicate = completion.clone();
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        let after = &service.agent_turn_contexts()[&turn_id];
        assert_eq!(after.chronology()[0].block().content, summary);
        assert_eq!(&after.chronology()[1..], expected_tail);
        let (_, cold) = AgentTranscriptStore::new(store.root())
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.revision().get(), revision);
        assert_eq!(cold.context().chronology(), after.chronology());
        let head = cold.head().unwrap().occurrence_id();
        assert_ne!(prior_head, Some(head));
        prior_head = Some(head);
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert!(service.pending_agent_compaction_tasks().is_empty());
        assert!(
            !service
                .apply_replay_epoch_completion(duplicate)
                .unwrap()
                .applied
        );
        if revision == 1 {
            let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
            let provider = RuntimeBatchProvider {
                response: runtime_say_response(
                    &turn_id,
                    &format!("SECOND_OBSERVED_SOURCE {}", "later ".repeat(1_000)),
                    true,
                ),
            };
            assert_eq!(
                service
                    .execute_agent_turn_with_provider(&turn_id, &provider, profile)
                    .unwrap()
                    .terminal_state,
                AgentTurnState::Completed
            );
            turn_id = service
                .start_agent_prompt_turn("%1", "keep the new task exact")
                .unwrap()
                .turn_id;
        }
    }
}

/// An oversized second range cannot publish the earlier staged summary or
/// leave a shortened raw replay suffix after recovery fails.
#[test]
fn runtime_observed_compaction_second_range_failure_keeps_original_epoch() {
    observed_staging::second_range_failure_keeps_original_epoch();
}

/// Reads the terminal durable projection after ordinary execution state has
/// retired. Auxiliary provenance must survive there without a fabricated
/// canonical MAAP recovery-facts record.
fn persisted_compaction_failure(
    service: &crate::runtime::RuntimeSessionService,
    store: &AgentTranscriptStore,
    conversation: &str,
    turn: &str,
) -> String {
    assert!(!service.agent_turn_executions().contains_key(turn));
    let rows = store.inspect(conversation).unwrap();
    assert!(
        !rows
            .iter()
            .any(|row| row.turn_id == turn && row.content.contains("canonical recovery facts"))
    );
    rows.iter()
        .find(|row| {
            row.turn_id == turn
                && row.role == mez_agent::transcript::TranscriptRole::Assistant
                && row.content.contains("Auxiliary compaction:")
        })
        .expect("terminal assistant record must retain auxiliary provenance")
        .content
        .clone()
}

/// Verifies transcript rows appended after compaction planning stay in raw
/// replay without displacing exact history that the plan deliberately retained.
#[test]
fn runtime_observed_compaction_preserves_history_when_transcript_arrives_after_queue() {
    let (mut service, transcript_store, turn) = observed_history::queued();
    let session = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session");
    let conversation_id = session.session_id.clone();
    let arrival = observed_history::append_external_task(
        &service,
        &transcript_store,
        &turn,
        "LATE_POST_PLAN_TRANSCRIPT_ENTRY",
    );

    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "observed input summary without exact user instructions",
    );
    // Inference has no authority to publish or discard the external archive
    // arrival. The real writer joins it after the frozen prefix once.
    replay_observed::adopt(&mut service, &turn);
    let mut fresh = test_runtime_service();
    fresh.set_agent_transcript_store(AgentTranscriptStore::new(transcript_store.root()));
    fresh.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    fresh
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation_id, 0)
        .unwrap();
    let cold = fresh
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(
        cold.context()
            .chronology()
            .iter()
            .filter(|event| event.record() == arrival.record())
            .count(),
        1
    );
    assert_eq!(cold.context().chronology().last().unwrap(), &arrival);
    let next_context = service
        .agent_context_for_pane_prompt("%1", "NEXT_USER_PROMPT Continue.", 0)
        .unwrap();
    let replay = next_context
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        replay.contains("LATE_POST_PLAN_TRANSCRIPT_ENTRY"),
        "{replay}"
    );
    assert!(replay.contains("EXACT_OLDER_USER_INSTRUCTION"), "{replay}");
    assert!(replay.contains("EXACT_SECOND_USER_INSTRUCTION"), "{replay}");
    assert!(replay.contains("EXACT_THIRD_USER_INSTRUCTION"), "{replay}");
}

/// Post-frontier oversized work remains exact; a next-request estimate cannot
/// revoke a usable final or pull the new arrival into another compactor request.
#[test]
fn runtime_observed_input_limit_preserves_late_oversized_transcript_after_commit() {
    let (mut service, store, turn) = observed_history::queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let original = store.inspect(&conversation).unwrap();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .update_replay_owner(|owner| {
            owner.append_execution_events([mez_agent::ContextConversationAppend::assistant(
                "late oversized transcript",
                "late-oversized-transcript-token ".repeat(21000),
                mez_agent::ContextExecutionGroupId::new("late-oversized-transcript")?,
            )])?;
            Ok(())
        })
        .unwrap();
    let arrival = service.agent_turn_contexts()[&turn]
        .chronology()
        .last()
        .unwrap()
        .clone();
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", "usable summary")
            .unwrap()
            > 20000
    );
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("usable summary")),
    );
    assert_eq!(sent.len(), 1);
    assert!(
        !sent[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("late-oversized-transcript-token")
    );
    assert_eq!(store.inspect(&conversation).unwrap(), original);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "usable summary");
    assert_eq!(
        service.agent_turn_contexts()[&turn]
            .chronology()
            .iter()
            .filter(|event| event.record() == arrival.record())
            .count(),
        1
    );
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// A usable nonreducing final publishes once. Duplicate physical callbacks do
/// not rerun inference, replace the head, or admit a second ordinary continuation.
#[test]
fn runtime_observed_input_limit_accepts_nonreducing_final_once() {
    let (mut service, store, turn) = observed_history::queued();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let summary = format!("{}END", "non-reducing candidate ".repeat(3000));
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", &summary)
            .unwrap()
            > 20000
    );
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&summary)),
    );
    assert_eq!(sent.len(), 1);
    let duplicate = crate::runtime::AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: task.task_generation,
        response: Box::new(runtime_test_compaction_response(&summary)),
    };
    assert!(
        !service
            .apply_agent_compaction_transition(duplicate.clone())
            .unwrap()
            .applied
    );
    replay_observed::adopt(&mut service, &turn);
    assert!(
        !service
            .apply_agent_compaction_transition(duplicate)
            .unwrap()
            .applied
    );
    replay_observed::assert_head(&service, &store, &turn, &summary);
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|pending| pending.turn_id == turn)
            .count(),
        1
    );
}

/// Verifies provider output-limit incomplete responses first trigger a compact
/// request-local mode, then max-output escalation, without mutating durable
/// active-turn context.
///
/// Output exhaustion means the provider accepted the input but cut generation
/// off, so the recovery path should first select the stable compact-response
/// behavior before escalating the output budget or discarding chronology.
#[test]
fn runtime_provider_output_limit_error_guides_then_escalates_without_compaction() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-output-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-output-limit-test]
provider = "runtime-batch"
model = "test"
max_output_tokens = 4096
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-output-limit-recovery","method":"agent/shell/command","params":{"idempotency_key":"agent-output-limit-recovery","input":"continue with the current implementation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic retained action result".to_string(),
            content: "output-limit-retained-context".to_string(),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeOutputLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-output-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3);
    let conversation_id = &service.agent_shell_store().get("%1").unwrap().session_id;
    let usage = service.agent_token_usage_for_conversation(conversation_id);
    let spent = usage
        .get(&mez_agent::ModelTokenUsageKey::new("runtime-batch", "test"))
        .unwrap();
    assert_eq!(spent.input_tokens, 250);
    assert_eq!(spent.output_tokens, 45);
    assert_eq!(spent.reasoning_tokens, 10);
    assert_eq!(spent.cached_input_tokens, Some(25));
    assert_eq!(requests[0].max_output_tokens, Some(4096));
    assert_eq!(requests[1].max_output_tokens, Some(4096));
    assert_eq!(requests[2].max_output_tokens, Some(4096));
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("output-limit-retained-context"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("Mezzanine interaction mode: output_limit_retry"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("[safe partial assistant text]\npartial safe response"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("Treat the safe assistant text below as already emitted"),
        "{second_request_text}"
    );
    assert!(!second_request_text.contains("output_limit_recovery_attempt="));
    assert!(
        !second_request_text.contains("error_message="),
        "{second_request_text}"
    );
    let third_request_text = requests[2]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        third_request_text.contains("Mezzanine interaction mode: output_limit_retry"),
        "{third_request_text}"
    );
    assert!(!third_request_text.contains("output_limit_recovery_attempt="));
    assert!(
        !second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = pane_text.replace("\n", "");
    assert!(
        pane_text_unwrapped
            .contains("provider response hit output limit; continuing from safe partial output"),
        "{pane_text}"
    );
    assert!(
        pane_text_unwrapped
            .contains("provider response hit output limit again; starting one fresh compact"),
        "{pane_text}"
    );
}

/// Creates an old claimed compaction and a newer queued compaction owned by a
/// replacement conversation in the same pane. Both sources come from real
/// completed tasks and successor admissions; no task is relabelled to a foreign
/// CID. The fixture retains each actual idle binding before crossing the stale
/// worker boundary, without executing auxiliary inference.
fn runtime_service_with_replacement_compaction() -> (RuntimeSessionService, String, u64, String, u64)
{
    use crate::test_support::compaction_source::{
        complete_task, complete_task_and_admit_successor,
    };
    let api = mez_agent::ProviderApiCompatibility::OpenAiResponses;
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "stale-compaction-regression".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-stale-test"
compaction_raw_retention_percent = 1
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.compact-stale-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("stale-compaction-regression"));
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "stale-compaction-original", 0)
        .unwrap();
    let successor = complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("ORIGINAL_SOURCE {}", "history ".repeat(2_000)),
        "original completed answer",
        "original current instruction",
        api,
    );
    complete_task(&mut service, &successor, "original current answer", api);
    let original = service.agent_shell_store().get("%1").unwrap().clone();

    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "stale-compaction-replacement", 0)
        .unwrap();
    let successor = complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("REPLACEMENT_SOURCE {}", "different ".repeat(2_000)),
        "replacement completed answer",
        "replacement current instruction",
        api,
    );
    complete_task(&mut service, &successor, "replacement current answer", api);
    let replacement = service.agent_shell_store().get("%1").unwrap().clone();
    let queued = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(queued.contains("state=queued"), "{queued}");
    let mut replacement_task = service.take_pending_agent_compaction_task("%1").unwrap();
    assert!(replacement_task.replay_source.is_some());
    // Return only this undispatched fixture operation to idle; retain its exact
    // source for the later generation. No source fields or CID are reassigned.
    service.cancel_current_agent_compaction_task("%1");
    assert!(!service.agent_is_compacting("%1"));
    service
        .agent_shell_store_mut()
        .restore_session("%1", original)
        .unwrap();
    let queued = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(queued.contains("state=queued"), "{queued}");
    let old_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("original conversation compaction task");
    let original_conversation_id = old_task.conversation_id.clone();
    let old_generation = old_task.task_generation;
    assert!(old_task.replay_source.is_some());
    assert!(
        crate::test_support::compaction_source::rendered_source(&old_task)
            .unwrap()
            .contains("ORIGINAL_SOURCE")
    );
    let replacement_input =
        crate::test_support::compaction_source::rendered_source(&replacement_task).unwrap();
    assert!(replacement_input.contains("REPLACEMENT_SOURCE"));
    assert!(!replacement_input.contains("ORIGINAL_SOURCE"));
    service.claim_agent_compaction_task_state("%1", old_task);
    let replacement_conversation_id = replacement.session_id.clone();
    service
        .agent_shell_store_mut()
        .restore_session("%1", replacement)
        .unwrap();
    replacement_task.task_generation = 0;
    replacement_task.compaction_epoch = 0;
    service.queue_agent_compaction_task(replacement_task);
    let replacement_generation = service
        .pending_agent_compaction_task_generation("%1")
        .expect("replacement compaction generation");

    (
        service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    )
}

/// An expired old claim cannot clear a replacement conversation's compaction
/// marker or publish a late summary into that replacement.
#[test]
fn runtime_compaction_claim_expiry_is_generation_and_conversation_fenced() {
    let (mut service, _, old_generation, replacement_conversation, replacement_generation) =
        runtime_service_with_replacement_compaction();
    assert!(
        service
            .expire_claimed_agent_compaction_task("%1", old_generation)
            .unwrap()
    );
    assert!(!service.agent_compaction_task_is_claimed("%1", old_generation));
    assert!(service.retains_compaction_expense_for_tests("%1", old_generation));
    assert!(service.agent_is_compacting("%1"));
    assert_eq!(
        service.pending_agent_compaction_task_generation("%1"),
        Some(replacement_generation)
    );
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        replacement_conversation
    );
    assert!(
        !service
            .expire_claimed_agent_compaction_task("%1", old_generation)
            .unwrap()
    );
    let late = service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(runtime_test_compaction_response(
                "late summary must not publish",
            )),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(!service.retains_compaction_expense_for_tests("%1", old_generation));
    assert!(service.agent_is_compacting("%1"));
}

/// Verifies a stale completion accounts usage to its original conversation
/// without settling or clearing replacement-conversation compaction work.
#[test]
fn runtime_agent_compaction_stale_completion_preserves_replacement_task() {
    let (
        mut service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    ) = runtime_service_with_replacement_compaction();
    let replacement_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("replacement compaction is queued");
    let replacement_entries = replacement_task.transcript_entries;
    service.claim_agent_compaction_task_state("%1", replacement_task);
    assert_eq!(
        service.claimed_agent_compaction_task_generation("%1"),
        Some(replacement_generation)
    );
    assert!(
        service
            .claim_agent_compaction_task("%1", old_generation)
            .unwrap()
            .is_none()
    );

    let mut response = runtime_test_compaction_response("stale summary must be ignored");
    response.usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        reasoning_tokens: 0,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
    };
    response.quota_usage = vec![mez_agent::ProviderQuotaUsage {
        name: "requests".to_string(),
        used_basis_points: 250,
        limit: 100,
        remaining: 98,
        reset: None,
    }];

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response),
        })
        .unwrap();

    assert_eq!(
        service.claimed_agent_compaction_task_generation("%1"),
        Some(replacement_generation),
        "stale completion must preserve the replacement claim"
    );
    assert!(service.agent_is_compacting("%1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, replacement_entries);
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert!(
        service
            .agent_token_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
    assert_eq!(
        service.agent_quota_usage_for_conversation(&original_conversation_id),
        &[mez_agent::ProviderQuotaUsage {
            name: "requests".to_string(),
            used_basis_points: 250,
            limit: 100,
            remaining: 98,
            reset: None,
        }]
    );
    assert!(
        service
            .agent_quota_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
}

/// Verifies a late completion remains attributable after its replacement task
/// has already settled, but duplicate delivery is ignored.
#[test]
fn runtime_agent_compaction_late_completion_accounts_after_replacement_settles() {
    let (
        mut service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    ) = runtime_service_with_replacement_compaction();
    let replacement_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("replacement compaction is queued");
    service.claim_agent_compaction_task_state("%1", replacement_task);

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
            pane_id: "%1".to_string(),
            task_generation: replacement_generation,
            kind: "forbidden".to_string(),
            message: "replacement provider failure".to_string(),
            usage: Default::default(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();
    assert!(!service.agent_is_compacting("%1"));
    let transcript_entries = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .transcript_entries;

    let mut response = runtime_test_compaction_response("stale summary must be ignored");
    response.usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        reasoning_tokens: 0,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
    };
    response.quota_usage = vec![mez_agent::ProviderQuotaUsage {
        name: "requests".to_string(),
        used_basis_points: 250,
        limit: 100,
        remaining: 98,
        reset: None,
    }];
    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response.clone()),
        })
        .unwrap();

    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, transcript_entries);
    assert!(!service.agent_is_compacting("%1"));
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert!(
        service
            .agent_token_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
    assert_eq!(
        service.agent_quota_usage_for_conversation(&original_conversation_id),
        response.quota_usage
    );
    assert!(
        service
            .agent_quota_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response),
        })
        .unwrap();
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert_eq!(
        service
            .agent_quota_usage_for_conversation(&original_conversation_id)
            .len(),
        1
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        transcript_entries
    );
}

/// Verifies a stale failure does not clear replacement task ownership or its
/// compacting marker after the pane has switched conversations.
#[test]
fn runtime_agent_compaction_stale_failure_preserves_replacement_task() {
    let (mut service, _, old_generation, replacement_conversation_id, replacement_generation) =
        runtime_service_with_replacement_compaction();

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            kind: "forbidden".to_string(),
            message: "stale failure must be ignored".to_string(),
            usage: Default::default(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();

    let replacement = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("stale failure must preserve the replacement task");
    assert_eq!(replacement.task_generation, replacement_generation);
    assert_eq!(replacement.conversation_id, replacement_conversation_id);
    let replacement_entries = replacement.transcript_entries;
    assert!(service.agent_is_compacting("%1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, replacement_entries);
}

/// Verifies pane compaction is a dispatch barrier for ordinary model work.
///
/// Action settlement or local messages can queue a continuation while model
/// compaction owns the pane. That continuation must remain queued without
/// becoming visible or claimable until compaction completion has rebuilt the
/// running context and cleared the pane's compacting marker.
#[test]
fn runtime_agent_compaction_blocks_provider_dispatch_until_context_is_ready() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compaction-dispatch-barrier","method":"agent/shell/command","params":{"idempotency_key":"compaction-dispatch-barrier","input":"continue after rebuilding context"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);

    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_provider_task_is_pending(&task.turn_id));
    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(service.agent_provider_task_is_pending(&task.turn_id));

    let cleared = service.fail_current_agent_compaction_task("%1");
    assert!(cleared.had_task());
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
}
