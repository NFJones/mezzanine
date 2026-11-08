//! Runtime tests for agent compaction behavior.

use super::*;
use crate::runtime::{current_unix_seconds, execute_runtime_agent_prompt_history_work};
use mez_agent::messaging::{Envelope, MessageScope};

mod first_turn;
mod legacy_raw;
mod manual_model;
mod observed_continuation;
mod observed_fit;
mod observed_history;
mod observed_staging;
mod preparation;
mod provider_backoff;
mod provider_continuity;
mod provider_limit_replay;
mod provider_recovery;
mod replay_compatibility;
mod replay_copies;
mod replay_observed;
mod replay_publication;
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
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(
        !task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains(&exact)),
        "new exact instructions cannot enter the frozen compactor source"
    );
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", "short summary",)
            .unwrap()
            > 20_000
    );
    complete_runtime_test_compaction(&mut service, "%1", "short summary");
    assert_eq!(
        service.agent_turn_ledger().turn(&turn_id).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert!(
        store
            .inspect(&conversation_id)
            .unwrap()
            .starts_with(&original_rows)
    );
    let text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let failure = normalized_pane_log_text(&text);
    assert!(
        failure.contains("canonical compaction candidate cannot reduce"),
        "{text}"
    );
    assert!(
        store
            .inspect(&conversation_id)
            .unwrap()
            .iter()
            .any(|row| row.content.contains(&exact)),
        "terminal history retains the exact instruction"
    );
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

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-paste-history"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(1229);
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let persisted_history = transcript_store
        .structured_prompt_history("conversation-1")
        .unwrap();
    assert_eq!(persisted_history.len(), 1);
    assert_eq!(
        persisted_history[0].text,
        format!("prefix {payload} suffix")
    );
    assert_eq!(
        persisted_history[0].rendered(),
        "prefix [Pasted 1.2 KiB] suffix"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> prefix [Pasted 1.2 KiB] suffix"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(&payload), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies a mixed prompt keeps its typed prefix and suffix around one large
/// paste after durable history reload and Up-arrow recall.
///
/// Real terminals deliver typing and bracketed paste as separate input batches.
/// Reloading that submission in a later agent session must reconstruct only the
/// pasted payload as an opaque block; it must not hide or absorb adjacent text.
#[test]
fn runtime_agent_prompt_recalls_mixed_paste_display_after_reload() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-mixed-paste-recall"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(70 * 1024);
    let expected = format!("typed before {payload} typed after");
    let pasted = format!("\x1b[200~{payload}\x1b[201~");
    for input in [
        b"typed before ".as_slice(),
        pasted.as_bytes(),
        b" typed after\r".as_slice(),
    ] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let prompt = &service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt;
    assert_eq!(prompt.buffer.expanded_line(), expected);
    assert_eq!(
        prompt.buffer.rendered_line(),
        "typed before [Pasted 70.0 KiB] typed after"
    );
    assert_eq!(
        prompt.render(),
        "⟩ typed before [Pasted 70.0 KiB] typed after"
    );
}

/// Verifies compact pasted placeholders are used for bracketed paste payloads
/// that exceed the visible agent prompt height even when the byte size is small.
///
/// Agent prompt rendering only shows up to six input rows. A seven-line
/// bracketed paste must collapse to the same inline placeholder form as a
/// large byte paste so surrounding prompt text remains editable and readable.
#[test]
fn runtime_agent_prompt_displays_over_height_paste_as_compact_block() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (1..=7)
        .map(|index| format!("tiny-line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted "), "{pane_text}");
    assert!(pane_text.contains(" suffix"), "{pane_text}");
    assert!(!pane_text.contains("tiny-line-7"), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies `/list-modified-files` renders compact modified-file rows.
///
/// Agent mutation previews already show `edited path (+N -M)` style summaries;
/// the slash command should expose the tracked aggregate in the same compact
/// form instead of a verbose nested object list.
#[test]
fn runtime_agent_shell_list_modified_files_reports_compact_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.record_agent_modified_file_delta("%1", "src/lib.rs".to_string(), 12, 3);

    let response = service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();
    assert!(
        response.contains(r#""body":null"#),
        "the deferred lane acknowledges /list-modified-files: {response}"
    );
    let response = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred /list-modified-files applies its page");

    assert!(response.contains("## modified files"), "{response}");
    assert!(response.contains("edited `src/lib.rs`"), "{response}");
    assert!(
        response.contains(r#"<span class=\"mez-diff-addition\">+12</span>"#),
        "{response}"
    );
    assert!(
        response.contains(r#"<span class=\"mez-diff-deletion\">-3</span>"#),
        "{response}"
    );
    assert!(!response.contains("Added:"), "{response}");
    assert!(!response.contains("Removed:"), "{response}");
    assert!(!response.contains("`summary`"), "{response}");
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(None),
        "## modified files\n\nno modified files tracked for this agent conversation."
    );
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(Some(
            &std::collections::BTreeMap::new()
        )),
        "## modified files\n\nno modified files tracked for this agent conversation.",
        "an empty tracked map renders the same empty state as no map at all"
    );
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
        let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
            &before,
            40_000,
            1,
            before.event_sequence_high_water_mark(),
            mez_agent::ProviderBudgetProjection::new(
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
                "runtime-batch",
            ),
        )
        .unwrap();
        assert!(service.queue_agent_active_turn_compaction(
            &turn_id, "provider-context-limit-test".into(), profile,
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: 40_000, max_input_tokens: 40_000,
            }, plan,
        ).unwrap());
        let input = &service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .request
            .messages
            .last()
            .unwrap()
            .content;
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

/// Verifies oversized transcript history arriving after observed-input
/// compaction was queued is included in the final request check before commit.
#[test]
fn runtime_observed_input_limit_rejects_late_oversized_transcript_without_commit() {
    let (mut service, transcript_store, turn_id) = observed_history::queued();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session")
        .session_id
        .clone();
    // This original live occurrence is a pending transcript member, not a
    // fabricated closed historical task or a count-derived archive import.
    let original_rows = transcript_store.inspect(&conversation_id).unwrap();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .update_replay_owner(|owner| {
            owner.append_execution_events([mez_agent::ContextConversationAppend::assistant(
                "late oversized transcript",
                "late-oversized-transcript-token ".repeat(21_000),
                mez_agent::ContextExecutionGroupId::new("late-oversized-transcript")?,
            )])?;
            Ok(())
        })
        .unwrap();
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests(
                "%1",
                "small observed input summary",
            )
            .unwrap()
            > 20_000
    );
    assert_eq!(
        transcript_store.inspect(&conversation_id).unwrap(),
        original_rows
    );

    complete_runtime_test_compaction(&mut service, "%1", "small observed input summary");
    // Earlier consumed ranges may still be summarized privately. The late
    // arrival must never become eligible or allow an oversized continuation.
    for _ in 0..8 {
        let Some(task) = service.pending_agent_compaction_task_for_tests("%1") else {
            break;
        };
        let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
            &task.target
        else {
            panic!("expected active recovery");
        };
        assert!(
            !plan
                .replacement_blocks()
                .iter()
                .any(|block| block.content.contains("late-oversized-transcript-token"))
        );
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            transcript_store
                .compaction_epoch(&conversation_id)
                .unwrap()
                .is_none()
        );
        complete_runtime_test_compaction(&mut service, "%1", "another short summary");
    }

    assert_eq!(
        transcript_store.compaction_epoch(&conversation_id).unwrap(),
        None
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Failed)
    );
}

/// Repeated oversized output cannot make progress by mutating an observation
/// counter. Both candidates are measured through the actual complete request;
/// neither may publish a head or resume ordinary work.
#[test]
fn runtime_observed_input_limit_rejects_non_reducing_candidate_without_commit() {
    let (mut service, transcript_store, turn_id) = observed_history::queued();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session")
        .session_id
        .clone();
    let summary = "non-reducing candidate ".repeat(3_000);
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", &summary,)
            .unwrap()
            > 20_000
    );
    let original = service.agent_turn_contexts()[&turn_id].clone();
    complete_runtime_test_compaction(&mut service, "%1", &summary);
    assert_eq!(service.agent_turn_contexts()[&turn_id], original);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    complete_runtime_test_compaction(&mut service, "%1", &summary);

    assert_eq!(
        transcript_store.compaction_epoch(&conversation_id).unwrap(),
        None
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Failed)
    );
    assert!(service.agent_turn_contexts().get(&turn_id).is_none());
}

/// Verifies an explicit input cap defers an oversized ordinary provider claim
/// before provider I/O and resumes only after model-backed context compaction.
///
/// Proactive compaction is not provider-error recovery, so it must preserve the
/// pending logical turn without consuming a provider retry attempt. The rebuilt
/// request must pass the same complete-wire preflight before becoming claimable.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_compacts_before_provider_claim() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-preflight".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-test"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.configured-input-cap-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let transcript_store = AgentTranscriptStore::new(temp_root("configured-input-cap-history"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "configured-input-cap-history".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-history".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!(
                "configured-cap-marker {} secRET-CROSS-BOUNDARY source-tail-sentinel",
                "compactable ".repeat(20_000)
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "configured-input-cap-history", 1)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-preflight","input":"continue after proactive compaction"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();
    let execution_usage = mez_agent::ModelTokenUsage {
        input_tokens: 1_200,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(100),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        execution_usage,
        execution_usage,
        Some(&task.model_profile),
    );

    let same_turn_group =
        mez_agent::ContextExecutionGroupId::new("configured-input-cap-same-turn").unwrap();
    let same_turn_marker = "same-turn-raw-marker ".repeat(20_000);
    let turn_context = service
        .agent_turn_contexts_mut()
        .get_mut(&task.turn_id)
        .expect("the configured-input turn owns an active context");
    turn_context
        .append_assistant_event(
            "same-turn compaction action",
            "inspect same-turn evidence before configured-input compaction",
            same_turn_group.clone(),
        )
        .unwrap();
    turn_context
        .append_evidence_event(
            mez_agent::ContextSourceKind::ActionResult,
            "same-turn large evidence",
            same_turn_marker.clone(),
            same_turn_group,
            None,
            true,
        )
        .unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| {
            let turns = service.agent_turn_ledger().turns();
            let pane_text = service
                .pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            panic!(
                "configured input cap should queue active-turn compaction; turns={turns:?}; pane={pane_text}"
            )
        });
    assert_eq!(queued.source, "configured-input-limit");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        trigger:
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ConfiguredInputLimit {
                pass,
                previous_input_tokens,
                max_input_tokens,
            },
        plan,
        ..
    } = &queued.target
    else {
        panic!("expected configured-input-limit active-turn compaction");
    };
    assert_eq!(*pass, 1);
    assert!(*previous_input_tokens > *max_input_tokens);
    assert_eq!(
        queued.request.max_output_tokens,
        Some(plan.summary_budget_words()),
        "the active compactor must not emit a profile-sized summary that exceeds the frozen plan"
    );

    let compactor_estimate = mez_agent::provider_request_input_estimate(
        &queued.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    let compactor_cap = compactor_estimate.input_tokens.saturating_sub(1);
    assert!(compactor_cap > 0);
    service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .unwrap()
        .model_profile
        .provider_options
        .insert("max_input_tokens".to_string(), compactor_cap.to_string());

    let mut compactor_requests = 0usize;
    let mut observed_split = false;
    let mut observed_tail_sentinel = false;
    let mut observed_redaction = false;
    while service
        .pending_agent_compaction_task_for_tests("%1")
        .is_some()
    {
        let dispatch = service
            .claim_agent_compaction_task("%1")
            .unwrap()
            .expect("configured-cap compactor request should become claimable");
        compactor_requests = compactor_requests.saturating_add(1);
        let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
            pending_blocks,
            plan,
            ..
        } = &dispatch.task.target
        else {
            panic!("expected active-turn compactor dispatch");
        };
        observed_tail_sentinel |= dispatch
            .task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("source-tail-sentinel"));
        observed_redaction |= dispatch
            .task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("[redacted]"));
        assert!(
            !dispatch
                .task
                .request
                .messages
                .iter()
                .any(|message| message.content.contains("secRET-CROSS-BOUNDARY")),
            "temporary compactor chunks must be redacted before splitting"
        );
        observed_split |= !pending_blocks.is_empty();
        assert_eq!(
            dispatch.task.request.max_output_tokens,
            Some(plan.summary_budget_words()),
            "every post-split compactor dispatch must retain the frozen summary ceiling"
        );
        assert!(
            compactor_requests <= 256,
            "configured-cap compactor recursion did not settle"
        );
        let mut response = runtime_test_compaction_response(&format!(
            "proactively compacted summary {compactor_requests}"
        ));
        response.usage = mez_agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 5,
            reasoning_tokens: 1,
            cached_input_tokens: Some(4),
            cache_write_input_tokens: None,
        };
        assert!(
            service
                .apply_agent_compaction_completed_event("%1", response)
                .unwrap()
        );
    }
    assert!(observed_split, "oversized compactor source was not split");
    assert!(compactor_requests > 1);
    assert!(
        observed_tail_sentinel,
        "the tail of a selected source block must reach a compactor request"
    );
    assert!(
        observed_redaction,
        "selected sensitive source must be redacted"
    );
    assert!(
        service
            .agent_latest_request_usage("configured-input-cap-history")
            .is_none(),
        "context replacement must leave execution usage unknown until execution resumes"
    );
    assert!(
        service
            .agent_context_usage_display("configured-input-cap-history")
            .is_none(),
        "compactor usage must not become the context display"
    );
    assert!(
        service
            .agent_context_usage_snapshot("configured-input-cap-history")
            .is_none(),
        "compactor usage must not become the context snapshot"
    );
    let usage_by_model = service.agent_token_usage_for_conversation("configured-input-cap-history");
    let accumulated = usage_by_model
        .get(&mez_agent::ModelTokenUsageKey::new(
            &task.model_profile.provider,
            &task.model_profile.model,
        ))
        .expect("execution and compactor usage must remain accounted");
    assert_eq!(
        accumulated.input_tokens,
        execution_usage.input_tokens + 40 * compactor_requests as u64
    );
    assert_eq!(
        accumulated.output_tokens,
        execution_usage.output_tokens + 5 * compactor_requests as u64
    );
    assert!(
        !service
            .agent_turn_contexts()
            .get(&task.turn_id)
            .expect("compacted turn context remains active")
            .blocks()
            .iter()
            .any(|block| block.content.contains("same-turn-raw-marker")),
        "configured-input compaction must not resurrect selected same-turn evidence"
    );
    assert!(service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    let dispatch = service
        .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
        .unwrap()
        .expect("smaller rebuilt request should become claimable");
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(|block| block.content.contains("proactively compacted summary"))
    );

    assert!(service.agent_turn_executions().get(&task.turn_id).is_none());
    service
        .record_claimed_agent_provider_task(&dispatch, 1, 30_000)
        .unwrap();
    let retained_context = service
        .agent_turn_contexts()
        .get(&task.turn_id)
        .cloned()
        .unwrap();
    let (prepared, _) = service
        .prepare_agent_turn_model_context(
            &dispatch.turn,
            retained_context.clone(),
            &service.mcp_registry().prompt_summary(),
            &dispatch.model_profile,
        )
        .unwrap();
    let retained_request = prepared
        .previous_request()
        .expect("claim should retain the exact OpenAI request for the next worker");
    assert_eq!(retained_request.provider, "runtime-batch");
    assert_eq!(retained_request.model, "test");

    let response = runtime_say_response(&task.turn_id, "first turn completed", true);
    let action = response
        .action_batch
        .as_ref()
        .and_then(|batch| batch.actions.first())
        .cloned()
        .expect("completion response should contain a final say action");
    service
        .apply_agent_provider_execution(
            &dispatch.turn,
            &dispatch.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &dispatch.turn,
                    &action,
                    vec!["first turn completed".to_string()],
                    None,
                )],
                final_turn: true,
                terminal_state: AgentTurnState::Completed,
            },
        )
        .unwrap();
    assert!(service.agent_turn_contexts().get(&task.turn_id).is_none());

    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"configured-input-cap-next","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-next","input":"small follow-up"}}"#,
        &primary,
    );
    assert!(second.contains(r#""state":"running""#), "{second}");
    let second_task = service.pending_agent_provider_tasks().remove(0);
    let second_agent_id = AgentId::opaque(second_task.agent_id.clone()).unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&second_agent_id, &second_task.turn_id)
            .unwrap()
            .is_some(),
        "the next turn should use the persisted compacted epoch"
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());

    service.clear_agent_turn_provider_request_chain(&task.turn_id);
    let (new_epoch, _) = service
        .prepare_agent_turn_model_context(
            &dispatch.turn,
            retained_context,
            &service.mcp_registry().prompt_summary(),
            &dispatch.model_profile,
        )
        .unwrap();
    assert!(new_epoch.previous_request().is_none());
}

/// Verifies an explicit input cap fails before provider dispatch when only
/// protected request material remains and no durable context is compactable.
///
/// The runtime must not queue an ineffective compaction pass or consume a
/// provider retry attempt. The public claim boundary should settle the turn
/// through its normal typed provider-failure path with no provider worker.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_fails_without_compactable_context() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-protected-only".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-protected-only"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.configured-input-cap-protected-only]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 1
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-protected-only-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-protected-only","input":"this exact user instruction cannot be compacted"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id).unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == task.turn_id && turn.state == AgentTurnState::Failed })
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped
            .contains("configured input cap is smaller than fixed provider request overhead"),
        "{pane_text}"
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
        old_task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("ORIGINAL_SOURCE"))
    );
    assert!(
        replacement_task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("REPLACEMENT_SOURCE"))
    );
    assert!(
        !replacement_task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("ORIGINAL_SOURCE"))
    );
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

/// Verifies overlapping compaction attempts are rejected before they can start
/// another model request for the same pane.
#[test]
fn runtime_agent_shell_compact_rejects_overlapping_pane_compaction() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-overlap","method":"agent/shell/command","params":{"idempotency_key":"compact-overlap","input":"/compact"}}"#,
        &primary,
    );

    assert!(response.contains("cannot mutate pane state"), "{response}");
}

/// Verifies manual compaction owns the pane: plain text is retained for the
/// post-compaction epoch and conversation replacement is rejected meanwhile.
#[test]
fn runtime_manual_compaction_queues_steering_and_blocks_conversation_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let prompt = service
        .execute_agent_shell_command(&primary, "first steering prompt")
        .unwrap();
    assert!(prompt.contains("\"command\":\"compacting\""), "{prompt}");
    assert!(service.pending_agent_provider_tasks().is_empty());
    service
        .execute_agent_shell_command(&primary, "second steering prompt")
        .unwrap();

    let blocked = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(blocked.contains("cannot mutate pane state"), "{blocked}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation_id
    );
    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].prompt,
        "first steering prompt\n\nsecond steering prompt"
    );
    assert!(service.agent_command_is_active("%1"));
    assert!(service.take_agent_compaction_steering("%1").is_empty());
    assert_eq!(resumed[0].steering_receipts.len(), 2);
    let ids = resumed[0]
        .steering_receipts
        .iter()
        .map(|receipt| receipt.id.clone())
        .collect::<Vec<_>>();
    assert_ne!(ids[0], ids[1]);
    assert!(service.claim_agent_prompt_history_preparation(&resumed[0]));
    let history = execute_runtime_agent_prompt_history_work(resumed[0].history_work.clone());
    assert!(
        service
            .complete_agent_prompt_history_preparation(&resumed[0], history)
            .unwrap()
    );
    assert!(
        !service
            .complete_agent_prompt_history_preparation(
                &resumed[0],
                execute_runtime_agent_prompt_history_work(resumed[0].history_work.clone())
            )
            .unwrap()
    );
    let receipts = service.steering_receipts_for_tests("turn-1");
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(receipts[0].sequence, receipts[1].sequence);
    assert!(receipts[0].sequence > 0);
    let event = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .chronology()
        .iter()
        .find(|event| event.sequence().get() == receipts[0].sequence)
        .unwrap();
    assert_eq!(
        event.block().content,
        "first steering prompt\n\nsecond steering prompt"
    );
}

/// Verifies cancelling a real queued compaction releases manual steering on
/// the unchanged replay epoch and ignores a late result from that generation.
#[test]
fn runtime_manual_compaction_cancellation_ignores_late_provider_result() {
    // Literal historical reference needs no invented execution completion.
    // The shared fixture admits the actual manual canonical operation.
    let (mut service, store) = manual_model::queued("manual-cancel", "cancel source".into());
    let archive = store.inspect("manual-cancel").unwrap();
    let primary = service.session().layout_owner_client_id().unwrap().clone();
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .expect("queued compaction generation");
    let steering = service
        .execute_agent_shell_command(&primary, "continue after cancellation")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );

    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "continue after cancellation");

    let late = service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: generation,
            response: Box::new(runtime_test_compaction_response("must be ignored")),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.take_pending_agent_prompt_history_for_tests().len(),
        0
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 0);
    assert_eq!(store.inspect("manual-cancel").unwrap(), archive);
    assert!(store.compaction_epoch("manual-cancel").unwrap().is_none());
}

/// Verifies prompts retained for one conversation are discarded when the pane
/// is rebound, rather than being resumed in a later conversation's compaction.
#[test]
fn runtime_manual_compaction_steering_does_not_cross_conversation_rebind() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original_conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .queue_agent_compaction_steering(
            "%1",
            primary.clone(),
            original_conversation.clone(),
            service.agent_compaction_epoch("%1"),
            "private old-conversation instruction".to_string(),
            "private old-conversation instruction".to_string(),
        )
        .unwrap();

    let rebound = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(rebound.contains("\"command\":\"new\""), "{rebound}");
    assert_ne!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        original_conversation
    );
    assert!(service.take_agent_compaction_steering("%1").is_empty());

    service.mark_agent_compacting_for_tests("%1", 1);
    service.cancel_current_agent_compaction_task("%1");
    assert!(!service.resume_agent_compaction_steering("%1").unwrap());
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
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

/// Verifies compaction keeps only a bounded raw transcript tail when the active
/// conversation is larger than the exact-reference window.
///
/// The compact memory can summarize older entries, but the next turn needs the
/// recent tail verbatim for prompts like "implement the first item". Older raw
/// messages should not remain in transcript replay after compaction.
#[test]
fn runtime_agent_shell_compact_retains_bounded_recent_transcript_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-tail-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-tail-test"
compaction_raw_retention_percent = 2
[providers.openai]
kind = "openai"
models = ["gpt-compact-tail-test"]
default_model = "gpt-compact-tail-test"
[model_profiles.compact-tail-test]
provider = "openai"
model = "gpt-compact-tail-test"
context_window_tokens = 20000
max_output_tokens = 256
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-tail"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                mez_agent::transcript::TranscriptRole::User,
                format!(
                    "old raw marker should be summary only {}",
                    "old-word ".repeat(28)
                ),
            ),
            11 => (
                mez_agent::transcript::TranscriptRole::User,
                format!(
                    "Recent targets:\n1. Preserve raw tail after compaction.\n2. Keep memory summary. {}",
                    "recent-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                mez_agent::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "tail-user ".repeat(28)),
            ),
            _ => (
                mez_agent::transcript::TranscriptRole::User,
                format!(
                    "filler assistant turn {sequence} {}",
                    "tail-assistant ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "as-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-tail", 12)
        .unwrap();

    let mut external = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail","method":"agent/shell/command","params":{"idempotency_key":"compact-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains("state=queued"), "{compact}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.replay_source.is_some());
    let summarized = task.summarized_entries;
    assert!(summarized > 0 && summarized <= 10);
    // Archive counts fence freshness; the canonical candidate owns membership.
    assert_eq!(task.transcript_entries, 12);
    let original = transcript_store.inspect("as-tail").unwrap();
    let expected_tail = original[summarized..]
        .iter()
        .map(|row| row.content.clone())
        .collect::<Vec<_>>();
    // Another original writer appends after capture. Its typed occurrence and
    // actual append receipt, not a synthetic local counter, qualify the arrival.
    external
        .admit_instruction_task(
            vec![ContextBlock::user_event(
                "user prompt",
                "LATE_POST_PLAN_MANUAL_TRANSCRIPT_ENTRY",
            )],
            &[],
            &[],
            0,
        )
        .unwrap();
    let arrival = external.context().chronology().last().unwrap();
    let block = arrival.block();
    let late = mez_agent::transcript::TranscriptContextRecord::new(
        mez_agent::TranscriptContextEvent::task_admission(
            arrival.sequence().get(),
            external
                .task_admission_start(arrival.record().occurrence_id())
                .unwrap(),
            &block.label,
            &block.content,
        )
        .unwrap(),
        arrival.record(),
        None,
    )
    .unwrap();
    transcript_store
        .commit_accounted_transcript_batch(
            &crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
                mez_agent::transcript::TranscriptEntry {
                    conversation_id: "as-tail".to_string(),
                    sequence: 13,
                    created_at_unix_seconds: 13,
                    role: mez_agent::transcript::TranscriptRole::System,
                    turn_id: "turn-13".to_string(),
                    agent_id: "agent-%1".to_string(),
                    pane_id: "%1".to_string(),
                    content: late.to_transcript_content().unwrap(),
                },
            ])
            .unwrap(),
            1,
        )
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "old raw marker should be summary only");
    assert!(
        transcript_store
            .load_replay_epoch("as-tail", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let archive = transcript_store.inspect("as-tail").unwrap();
    assert!(archive.starts_with(&original));
    let receipt = replay_publication::execute_pending_publication(&mut service);
    assert!(service.agent_is_compacting("%1"));
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let (_, cold) = AgentTranscriptStore::new(transcript_store.root())
        .load_replay_epoch("as-tail", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().source,
        ContextSourceKind::CompactionPrefix
    );
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "old raw marker should be summary only"
    );
    let raw = cold.context().chronology()[1..]
        .iter()
        .map(|event| event.block().content.clone())
        .collect::<Vec<_>>();
    assert_eq!(raw, expected_tail);
    // The manifest loader returns its frozen members. A fresh ordinary history
    // read joins later accounted archive arrivals exactly once in source order.
    let mut fresh = test_runtime_service();
    fresh.set_agent_transcript_store(AgentTranscriptStore::new(transcript_store.root()));
    fresh.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    fresh
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-tail", 13)
        .unwrap();
    let replay = fresh
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert!(
        replay
            .context()
            .chronology()
            .starts_with(cold.context().chronology())
    );
    assert_eq!(
        replay
            .context()
            .chronology()
            .iter()
            .filter(|event| event.record() == arrival.record())
            .count(),
        1
    );
    assert_eq!(replay.context().chronology().last().unwrap(), arrival);
    assert_eq!(transcript_store.inspect("as-tail").unwrap(), archive);

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-tail-prompt","input":"Implement the first item"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let compact_memory = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::CompactionPrefix)
        .expect("typed summary head should be model-visible after /compact");
    assert_eq!(
        compact_memory.content,
        "old raw marker should be summary only"
    );
    let transcript_context = context
        .chronology()
        .iter()
        .filter(|event| event.block().source != ContextSourceKind::CompactionPrefix)
        .map(|event| event.block().content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        transcript_context.contains("1. Preserve raw tail after compaction."),
        "{transcript_context}"
    );
    assert!(
        transcript_context.contains("LATE_POST_PLAN_MANUAL_TRANSCRIPT_ENTRY"),
        "{transcript_context}"
    );
    assert!(
        !transcript_context.contains("old raw marker should be summary only"),
        "{transcript_context}"
    );
    assert_eq!(
        context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::CompactionPrefix)
            .count(),
        1
    );
    assert!(service.memory_records().iter().all(|record| {
        record.id != mez_agent::memory::canonical_memory_uuid("compact-as-tail")
    }));
}

/// Verifies manual compaction reports an oversized unfinished exact tail instead
/// of silently claiming it fits or summarizing a partial execution group.
#[test]
fn runtime_manual_compaction_reports_irreducible_oversized_unfinished_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "manual-unfinished-tail".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-unfinished-tail\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-unfinished-tail]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 10000\nmax_input_tokens = 2000\n".to_string(),
        }])
        .unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-unfinished-tail"));
    let mut original = mez_agent::ReplayEpochContext::new(
        mez_agent::ReplayConversationId::new("manual-unfinished-tail").unwrap(),
        mez_agent::AgentContext::empty(),
    )
    .unwrap();
    original
        .admit_instruction_task(
            vec![ContextBlock::user_event(
                "user prompt",
                format!(
                    "EXACT_UNFINISHED_USER_REQUEST {}",
                    "protected ".repeat(2_000)
                ),
            )],
            &[],
            &[],
            0,
        )
        .unwrap();
    let event = &original.context().chronology()[0];
    let record = mez_agent::transcript::TranscriptContextRecord::new(
        mez_agent::TranscriptContextEvent::task_admission(
            event.sequence().get(),
            original
                .task_admission_start(event.record().occurrence_id())
                .unwrap(),
            &event.block().label,
            &event.block().content,
        )
        .unwrap(),
        event.record(),
        None,
    )
    .unwrap();
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-unfinished-tail".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::System,
            turn_id: "unfinished-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: record.to_transcript_content().unwrap(),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-unfinished-tail", 1)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-unfinished-tail","method":"agent/shell/command","params":{"idempotency_key":"manual-unfinished-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(
        compact.contains("reason=irreducible-exact-retained-tail"),
        "{compact}"
    );
    assert!(compact.contains("summarized_entries=0"), "{compact}");
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert_eq!(
        store.compaction_epoch("manual-unfinished-tail").unwrap(),
        None
    );
    assert_eq!(store.inspect("manual-unfinished-tail").unwrap().len(), 1);
}

/// Verifies an oversized manual summary is rejected before the transcript
/// replay epoch or retained raw history is mutated.
#[test]
fn runtime_manual_compaction_rejects_oversized_post_summary_request() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-final-fit".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-final-fit\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-final-fit]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 10000\nmax_input_tokens = 2000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-final-fit"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-final-fit".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            // An inert historical reference, not an inferred closed execution.
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "compact this short durable history".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-final-fit", 1)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-final-fit","method":"agent/shell/command","params":{"idempotency_key":"manual-final-fit","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("manual compaction task");
    let source_words = queued
        .request
        .messages
        .last()
        .map(|message| mez_agent::model_context_text_word_count(&message.content))
        .unwrap_or_default()
        .max(1);
    assert!(queued.preserve_summary_output_budget);
    assert_eq!(queued.summary_correction.manual_words, Some(source_words));
    assert_eq!(
        queued.request.max_output_tokens,
        queued.model_profile.max_output_tokens()
    );
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        &"oversized summary ".repeat(source_words),
    );

    assert!(
        store
            .compaction_epoch("manual-final-fit")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-final-fit").unwrap().len(), 1);
    assert!(service.memory_records().iter().all(|record| {
        record.id != mez_agent::memory::canonical_memory_uuid("compact-manual-final-fit")
    }));
}

/// A manual candidate over its final-request cap retries the frozen source
/// with shorter advisory guidance and publishes only a complete-fit summary.
#[test]
fn runtime_manual_compaction_retries_oversized_final_request() {
    qualify_manual_final_fit_retry(false);
}

/// Repeating a frozen-valid but complete-request-oversized manual candidate
/// fails at the original fit/no-progress owner, without a new correction loop,
/// partial publication or a change to frozen word authority.
#[test]
fn runtime_manual_compaction_rejects_non_reducing_final_retry() {
    qualify_manual_final_fit_retry(true);
}

/// Seeds a real manual candidate and chooses final complete-fit progress versus
/// a repeated candidate; both cases preserve the original archive until fit.
fn qualify_manual_final_fit_retry(repeat: bool) {
    let mut service = test_runtime_service();
    // Derive the final-fit cap from real complete requests below. Static prompt
    // and schema growth must not turn this retry test into irreducible overhead.
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-final-retry".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-final-retry\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-final-retry]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 16000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-final-retry"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-final-retry".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            // Historical reference text needs no invented execution closure.
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "old-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "old decision ".repeat(800),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-final-retry", 1)
        .unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-retry","method":"agent/shell/command","params":{"idempotency_key":"manual-retry","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("state=queued"), "{queued}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("initial manual source")
        .clone();
    assert!(task.replay_source.is_some());
    let mut estimate = |summary| {
        let context = service
            .manual_compaction_candidate_context(&task, summary)
            .unwrap();
        service
            .manual_compaction_candidate_size_for_tests(&task, context)
            .unwrap()
            .0
    };
    let short_estimate = estimate("short manual summary");
    let long_estimate = estimate(&"long summary ".repeat(700));
    assert!(long_estimate > short_estimate);
    let desired_safe_limit = short_estimate + (long_estimate - short_estimate) / 2;
    let input_cap = (desired_safe_limit * 20).div_ceil(19);
    // Calibration does not authorize changing a frozen task's model. Admit a
    // new operation under the actual configured cap, retaining the unchanged
    // original archive. Both fit checks below use that newly captured profile.
    service.cancel_current_agent_compaction_task("%1");
    let mut layers = service.config_layers().to_vec();
    layers[0].text = layers[0].text.replace(
        "max_input_tokens = 16000",
        &format!("max_input_tokens = {input_cap}"),
    );
    service.replace_config_layers(layers).unwrap();
    let queued = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(queued.contains("state=queued"), "{queued}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert_eq!(task.model_profile.max_input_tokens(), Some(input_cap));
    let frozen_words = task.summary_correction.manual_words;
    assert!(frozen_words.is_some());
    let context = service
        .manual_compaction_candidate_context(&task, "short manual summary")
        .unwrap();
    let (_, safe_limit, _) = service
        .manual_compaction_candidate_size_for_tests(&task, context)
        .unwrap();
    assert!(short_estimate <= safe_limit && safe_limit < long_estimate);
    complete_runtime_test_compaction(&mut service, "%1", &"long summary ".repeat(700));
    assert!(
        store
            .compaction_epoch("manual-final-retry")
            .unwrap()
            .is_none()
    );
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| {
            panic!(
                "manual retry queued: {}",
                service
                    .pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\n")
            )
        });
    let (oversized_estimate, attempts) = retry.manual_final_retry.expect("actual final-fit retry");
    assert!(
        oversized_estimate > safe_limit,
        "the long candidate must exceed this fixture's safe cap"
    );
    assert_eq!(attempts, 1);
    assert_eq!(retry.summary_correction.manual_words, frozen_words);
    assert!(
        retry
            .summary_correction
            .effective_input_tokens
            .is_some_and(|tokens| tokens > 0)
    );
    assert_eq!(store.inspect("manual-final-retry").unwrap().len(), 1);
    let next_summary = if repeat {
        "long summary ".repeat(700)
    } else {
        "short manual summary".to_string()
    };
    complete_runtime_test_compaction(&mut service, "%1", &next_summary);
    assert!(
        store
            .load_replay_epoch("manual-final-retry", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    if repeat {
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        let raw = store.inspect("manual-final-retry").unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].content, "old decision ".repeat(800));
        let screen = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .iter()
            .map(|line| line.trim().trim_start_matches('│').trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            screen.contains("manual compaction candidate")
                && screen.contains("cannot fit its complete input allowance"),
            "{screen}"
        );
    } else {
        assert!(service.agent_is_compacting("%1"));
        let receipt = replay_publication::execute_pending_publication(&mut service);
        assert!(service.agent_is_compacting("%1"));
        service.apply_replay_epoch_completion(receipt).unwrap();
        let (_, cold) = AgentTranscriptStore::new(store.root())
            .load_replay_epoch("manual-final-retry", mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.context().chronology()[0].block().content, next_summary);
        assert_eq!(cold.revision().get(), 1);
        let raw = store.inspect("manual-final-retry").unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].content, "old decision ".repeat(800));
    }
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// A configured input cap splits the frozen manual source before dispatch,
/// without publishing a partial conversation summary.
#[test]
fn runtime_manual_compaction_splits_configured_input_cap() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-cap".to_string(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-cap\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-cap]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-cap-split"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-cap-split".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                // Packing arbitrary reference text needs no execution witness.
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!(
                    "SOURCE_{sequence} {} FINAL_SOURCE_SENTINEL",
                    "word ".repeat(if sequence == 3 { 10 } else { 25_000 })
                ),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&temp_root("manual-cap-auth")),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-cap-split", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-cap","method":"agent/shell/command","params":{"idempotency_key":"manual-cap","input":"/compact"}}"#, &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let original_source = original.request.messages.last().unwrap().content.clone();
    let estimate = mez_agent::provider_request_input_estimate(
        &original.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    let cap = 1_500;
    assert!(estimate.input_tokens > cap, "{estimate:?}");
    service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .unwrap()
        .model_profile
        .provider_options
        .insert("max_input_tokens".to_string(), cap.to_string());
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let dispatch = service
        .claim_agent_compaction_task("%1", generation)
        .unwrap()
        .unwrap();
    assert!(
        dispatch
            .task
            .conversation_chunks
            .as_ref()
            .is_some_and(|chunks| !chunks.pending.is_empty())
    );
    let reduced = mez_agent::provider_request_input_estimate(
        &dispatch.task.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    assert!(!reduced.exceeds_explicit_cap(cap));
    assert!(
        store
            .compaction_epoch("manual-cap-split")
            .unwrap()
            .is_none()
    );
    let mut fragments = Vec::new();
    let mut current = dispatch.task;
    // Prior completed responses are diagnostic, not an admission ceiling for
    // the finite frozen chunks still owned by this compaction epoch.
    current.conversation_chunks.as_mut().unwrap().completed = 32;
    let logical_epoch = current.compaction_epoch;
    let steering = service
        .execute_agent_shell_command(&primary, "keep steering across chunks")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );
    for attempt in 0..128 {
        assert_eq!(current.compaction_epoch, logical_epoch);
        let source = current.request.messages.last().unwrap().content.clone();
        let synthesis = source.contains("Chunk 1 summary:");
        if !synthesis {
            fragments.push(source);
        }
        let summary = if synthesis {
            "FINAL_CAP_SUMMARY"
        } else {
            "bounded chunk summary"
        };
        if synthesis {
            // The small cap exercised every source chunk; give the complete
            // post-compaction request its ordinary input allowance.
            current
                .model_profile
                .provider_options
                .insert("max_input_tokens".to_string(), "25000".to_string());
            service.claim_agent_compaction_task_state("%1", current.clone());
        }
        assert!(
            service
                .apply_agent_compaction_transition(
                    crate::runtime::AgentCompactionEvent::Completed {
                        pane_id: "%1".to_string(),
                        task_generation: current.task_generation,
                        response: Box::new(runtime_test_compaction_response(summary)),
                    }
                )
                .unwrap()
                .applied
        );
        if synthesis {
            break;
        }
        assert!(
            store
                .compaction_epoch("manual-cap-split")
                .unwrap()
                .is_none()
        );
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap_or_else(|| panic!("manual synthesis was not queued after {attempt} responses"));
        current = service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .expect("next bounded chunk or synthesis")
            .task;
    }
    assert!(
        fragments.len() > 32,
        "expected more than 32 finite source chunks"
    );
    assert_eq!(fragments.concat(), original_source);
    assert!(
        store
            .load_replay_epoch("manual-cap-split", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    let receipt = replay_publication::execute_pending_publication(&mut service);
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch("manual-cap-split", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "FINAL_CAP_SUMMARY"
    );
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "keep steering across chunks");
}

/// A rejected manual compactor request must queue smaller temporary input
/// without changing the selected conversation's durable replay boundary.
#[test]
fn runtime_manual_compaction_recovers_provider_context_limit() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-compact-context-limit".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-limit\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-limit]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-compact-context-limit"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-context-limit".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                // Provider rejection operates on literal historical references.
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("MANUAL_SOURCE_{sequence} {}", "history ".repeat(100)),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-context-limit", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-limit","method":"agent/shell/command","params":{"idempotency_key":"manual-limit","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    task.conversation_chunks = Some(
        crate::runtime::agent_state::RuntimeConversationCompactionChunks {
            current: task.request.messages.last().unwrap().content.clone(),
            pending: Vec::new(),
            summaries: Vec::new(),
            synthesis_source_bytes: None,
            failures: mez_agent::DEFAULT_PROVIDER_RETRY_POLICY.max_attempts,
            completed: 0,
        },
    );
    let logical_epoch = task.compaction_epoch;
    let task_generation = task.task_generation;
    let previous_bytes = mez_agent::openai_responses_request_body_with_stream(&task.request, false)
        .unwrap()
        .len();
    service.claim_agent_compaction_task_state("%1", task);
    let first_steering = service
        .execute_agent_shell_command(&primary, "steer before context retry")
        .unwrap();
    assert!(
        first_steering.contains("\"command\":\"compacting\""),
        "{first_steering}"
    );
    assert!(
        service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
                pane_id: "%1".to_string(),
                task_generation,
                kind: "invalid_state".to_string(),
                message: "provider context length exceeded".to_string(),
                usage: Default::default(),
                provider_failure_json: Some(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#.to_string(),
                ),
                provider_raw_text: None,
            })
            .unwrap()
            .applied
    );
    let retry_bytes = {
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .expect("smaller manual compactor request");
        assert_eq!(retry.compaction_epoch, logical_epoch);
        assert_eq!(
            retry.conversation_chunks.as_ref().unwrap().failures,
            mez_agent::DEFAULT_PROVIDER_RETRY_POLICY.max_attempts + 1
        );
        mez_agent::openai_responses_request_body_with_stream(&retry.request, false)
            .unwrap()
            .len()
    };
    assert!(
        retry_bytes < previous_bytes,
        "{retry_bytes} >= {previous_bytes}"
    );
    let retry_steering = service
        .execute_agent_shell_command(&primary, "steer between compaction chunks")
        .unwrap();
    assert!(
        retry_steering.contains("\"command\":\"compacting\""),
        "{retry_steering}"
    );
    assert_eq!(store.inspect("manual-context-limit").unwrap().len(), 3);
    assert!(
        store
            .compaction_epoch("manual-context-limit")
            .unwrap()
            .is_none()
    );
    let mut chunk_sources = Vec::new();
    let mut synthesized = false;
    for _ in 0..8 {
        let pending = service
            .take_pending_agent_compaction_task("%1")
            .expect("each temporary chunk and final synthesis is queued");
        assert_eq!(pending.compaction_epoch, logical_epoch);
        let task_generation = pending.task_generation;
        let source = pending.request.messages.last().unwrap().content.clone();
        let is_synthesis = source.contains("Chunk 1 summary:");
        chunk_sources.push(source);
        service.claim_agent_compaction_task_state("%1", pending);
        let summary = if is_synthesis {
            "FINAL_MANUAL_SUMMARY"
        } else {
            "temporary chunk summary"
        };
        assert!(
            service
                .apply_agent_compaction_transition(
                    crate::runtime::AgentCompactionEvent::Completed {
                        pane_id: "%1".to_string(),
                        task_generation,
                        response: Box::new(runtime_test_compaction_response(summary)),
                    }
                )
                .unwrap()
                .applied
        );
        if is_synthesis {
            synthesized = true;
            break;
        }
        assert!(
            store
                .compaction_epoch("manual-context-limit")
                .unwrap()
                .is_none()
        );
    }
    assert!(
        synthesized,
        "manual compaction never reached synthesis: {chunk_sources:?}"
    );
    assert!(
        chunk_sources
            .last()
            .unwrap()
            .contains("temporary chunk summary")
    );
    assert!(
        store
            .load_replay_epoch("manual-context-limit", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    let receipt = replay_publication::execute_pending_publication(&mut service);
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch("manual-context-limit", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "FINAL_MANUAL_SUMMARY"
    );
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].prompt,
        "steer before context retry\n\nsteer between compaction chunks"
    );
}

/// A failure after one temporary chunk has completed must not publish its
/// partial summary or shorten the exact transcript replay boundary.
#[test]
fn runtime_manual_compaction_partial_chunk_failure_preserves_history() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-partial-failure".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-limit\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-limit]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-partial-failure"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-partial-failure".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                // No fake completion authority is needed for reference history.
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("source {sequence} {}", "word ".repeat(100)),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-partial-failure", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-partial","method":"agent/shell/command","params":{"idempotency_key":"manual-partial","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", initial);
    service
        .apply_agent_compaction_failed_event(
            "%1",
            "invalid_state",
            "provider context length exceeded",
            Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
        )
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "partial model summary");
    assert!(
        store
            .compaction_epoch("manual-partial-failure")
            .unwrap()
            .is_none()
    );
    let next = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", next);
    assert!(service.apply_agent_compaction_failed_event(
        "%1", "forbidden", "authentication rejected", None,
    ).unwrap());
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(
        store
            .compaction_epoch("manual-partial-failure")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-partial-failure").unwrap().len(), 3);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        3
    );
}

/// The configured model token limit, rather than its word-scaled fallback,
/// determines which complete recent turn stays raw during manual compaction.
#[test]
fn runtime_manual_compaction_tail_uses_configured_token_allowance() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-token-tail".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-token-tail\"\ncompaction_raw_retention_percent = 10\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-token-tail]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 20000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-token-tail"));
    for (sequence, content) in [(1, "old work".to_string()), (2, "x".repeat(6_000))] {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-token-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                // Whole reference occurrences exercise the token reservation.
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-token-tail", 2)
        .unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-token-tail","method":"agent/shell/command","params":{"idempotency_key":"manual-token-tail","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("state=queued"), "{queued}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.replay_source.is_some());
    assert_eq!(task.summarized_entries, 1);
    let task = task.clone();
    let candidate = service
        .manual_compaction_candidate_context(&task, "old work summarized")
        .unwrap();
    assert_eq!(
        candidate
            .chronology()
            .iter()
            .filter(|event| event.block().content == "x".repeat(6_000))
            .count(),
        1
    );
    assert!(
        !candidate
            .chronology()
            .iter()
            .any(|event| event.block().content == "old work")
    );
}

/// Manual compaction may absorb a prior selective range only after summarizing
/// a complete prefix that contains it; its later raw tail stays available.
#[test]
fn runtime_manual_compaction_after_selective_epoch() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-after-selective".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"selective-manual\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.selective-manual]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-after-selective"));
    let group = mez_agent::ContextExecutionGroupId::new("manual-prior-group").unwrap();
    for (sequence, content, turn_id, role) in [
        (
            1,
            "display answer".to_string(),
            "old",
            mez_agent::transcript::TranscriptRole::Assistant,
        ),
        (
            2,
            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                ContextSourceKind::TranscriptAssistant,
                "answer",
                "old typed work",
                group,
                1,
                None,
            )
            .unwrap()
            .to_transcript_content(),
            "old",
            mez_agent::transcript::TranscriptRole::System,
        ),
        (
            3,
            "recent exact answer".to_string(),
            "recent",
            mez_agent::transcript::TranscriptRole::Assistant,
        ),
    ] {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-selective".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: turn_id.to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    store
        .install_legacy_compaction_ranges_fixture(
            "manual-selective",
            0,
            "",
            vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 2,
                through_sequence: 2,
                summary: "prior typed summary".to_string(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-selective", 3)
        .unwrap();
    let original = store.compaction_epoch("manual-selective").unwrap();
    let original_rows = store.inspect("manual-selective").unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-selective","method":"agent/shell/command","params":{"idempotency_key":"manual-selective","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("normalization"), "{queued}");
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        store.compaction_epoch("manual-selective").unwrap(),
        original
    );
    assert_eq!(store.inspect("manual-selective").unwrap(), original_rows);
}

/// Verifies model-generated compacted context survives runtime restoration.
#[test]
fn runtime_agent_compaction_summary_survives_runtime_restore() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-restore-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-restore-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-restore-test"]
default_model = "gpt-compact-restore-test"
[model_profiles.compact-restore-test]
provider = "openai"
model = "gpt-compact-restore-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-restore"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-restore", 0)
        .unwrap();
    let successor = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        "restore compact source from the original task",
        "original task completed",
        "retain the current task exactly",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &successor,
        "current task completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let original_archive = transcript_store.inspect("as-restore").unwrap();
    service.checkpoint_agent_session_metadata().unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-restore","method":"agent/shell/command","params":{"idempotency_key":"compact-restore","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let steering = service
        .execute_agent_shell_command(&primary, "continue after compaction")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );
    let task = service
        .take_pending_agent_compaction_task("%1")
        .expect("manual compaction task");
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
                pane_id: "%1".to_string(),
                task_generation: service
                    .claimed_agent_compaction_task_generation("%1")
                    .unwrap(),
                response: Box::new(runtime_test_compaction_response(
                    "COMPACTION_SUMMARY_RESTORED_AFTER_RESTART"
                )),
            })
            .unwrap()
            .applied
    );
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    assert!(
        transcript_store
            .compaction_epoch("as-restore")
            .unwrap()
            .is_none()
    );
    let receipt = replay_publication::execute_pending_publication(&mut service);
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let (_, expected) = AgentTranscriptStore::new(transcript_store.root())
        .load_replay_epoch("as-restore", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(expected.revision().get(), 1);
    assert_eq!(
        transcript_store.inspect("as-restore").unwrap(),
        original_archive
    );
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "continue after compaction");
    assert_eq!(
        resumed[0].transcript_entries,
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries
    );
    let session_snapshot = service.session().clone();

    let mut restored = RuntimeServiceFixture::new().build_with_session(session_snapshot);
    restored.set_agent_transcript_store(transcript_store);
    restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    let context = restored
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context.blocks().iter().any(|block| block
            .content
            .contains("COMPACTION_SUMMARY_RESTORED_AFTER_RESTART")),
        "restored history should include the durable compaction summary: {:#?}",
        context.blocks()
    );
    assert!(context.chronology().len() > expected.context().chronology().len());
    for (actual, original) in context
        .chronology()
        .iter()
        .zip(expected.context().chronology())
    {
        assert_eq!(actual.block(), original.block());
        assert_eq!(actual.record(), original.record());
        assert_eq!(actual.sequence(), original.sequence());
        assert_eq!(actual.execution_group_id(), original.execution_group_id());
        assert_eq!(actual.provider_owner(), original.provider_owner());
        // The newly admitted prompt supersedes the previous task's protected
        // instruction scope, not its original bytes, identity or accounting.
        if original.retention() == mez_agent::ContextRetention::Exact
            && original.block().source == ContextSourceKind::UserInstruction
        {
            assert_eq!(
                actual.retention(),
                mez_agent::ContextRetention::Summarizable
            );
        } else {
            assert_eq!(actual.retention(), original.retention());
        }
    }
    assert_eq!(
        context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::CompactionPrefix)
            .count(),
        1
    );
}

/// A lost committed epoch must fail prompt construction rather than replaying
/// the pane checkpoint's shortened suffix without the summarized prefix.
#[test]
fn runtime_compaction_missing_epoch_fails_restored_prompt() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-missing-compaction-epoch"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "missing-epoch".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("history {sequence}"),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch("missing-epoch", 1, "durable summary")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "missing-epoch", 2)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let snapshot = service.session().clone();
    let mut restored = RuntimeServiceFixture::new().build_with_session(snapshot);
    restored.set_agent_transcript_store(store.clone());
    restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    let path = store
        .transcript_path("missing-epoch")
        .unwrap()
        .parent()
        .unwrap()
        .join("compaction-epoch.json");
    std::fs::remove_file(path).unwrap();
    assert!(
        restored
            .agent_context_for_pane_prompt("%1", "continue", 0)
            .unwrap_err()
            .message()
            .contains("required compaction epoch")
    );
}

/// A committed summary remains visible with no raw replay rows, and later
/// appends appear exactly once even if the pane's saved count is stale.
#[test]
fn runtime_compaction_epoch_replays_empty_tail_and_later_append() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-epoch-empty-tail"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "epoch-empty".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "OLDER_ARCHIVED_ROW".to_string(),
        })
        .unwrap();
    store
        .save_compaction_epoch("epoch-empty", 1, "DURABLE_EMPTY_TAIL_SUMMARY")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "epoch-empty", 0)
        .unwrap();
    let before = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        before
            .blocks()
            .iter()
            .any(|block| block.content.contains("DURABLE_EMPTY_TAIL_SUMMARY"))
    );
    assert!(
        !before
            .blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_ARCHIVED_ROW"))
    );
    // Produce a genuine later canonical fact and commit its original ordinary
    // append proposal. Bare legacy rows cannot claim arrival custody after a
    // typed head has become authoritative.
    let (_, mut arrival_source) = store
        .load_replay_epoch("epoch-empty", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    arrival_source
        .append_fact("later", "LATER_EXACT_ROW")
        .unwrap();
    let original = arrival_source.context().chronology().last().unwrap();
    let mut later = store.inspect("epoch-empty").unwrap().pop().unwrap();
    later.sequence = 2;
    later.role = TranscriptRole::System;
    later.content = mez_agent::transcript::TranscriptContextRecord::new(
        mez_agent::TranscriptContextEvent::prompt_boundary_with_event_sequence(
            original.block().source,
            original.sequence().get(),
            original.block().label.clone(),
            original.block().content.clone(),
        )
        .unwrap(),
        original.record(),
        None,
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    store
        .commit_accounted_transcript_batch(
            &crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![later]).unwrap(),
            1,
        )
        .unwrap();
    let after = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert_eq!(
        after
            .blocks()
            .iter()
            .filter(|block| block.content.contains("LATER_EXACT_ROW"))
            .count(),
        1
    );
    assert!(
        after
            .blocks()
            .iter()
            .any(|block| block.content.contains("DURABLE_EMPTY_TAIL_SUMMARY"))
    );
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_ARCHIVED_ROW"))
    );
    let restored = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    for context in [&after, &restored] {
        let retained = context
            .chronology()
            .iter()
            .filter(|event| event.record().occurrence_id() == original.record().occurrence_id())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].record(), original.record());
        assert_eq!(retained[0].sequence(), original.sequence());
        assert_eq!(retained[0].block(), original.block());
    }
}

/// An empty pane replay count cannot conceal loss of the transcript that owns
/// the committed summary and its exact sequence boundary.
#[test]
fn runtime_compaction_epoch_missing_transcript_fails_empty_tail() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-epoch-missing-transcript"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "missing-transcript".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "archived history".to_string(),
        })
        .unwrap();
    store
        .save_compaction_epoch("missing-transcript", 1, "required summary")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "missing-transcript", 0)
        .unwrap();
    std::fs::remove_file(store.transcript_path("missing-transcript").unwrap()).unwrap();
    assert!(
        service
            .agent_context_for_pane_prompt("%1", "continue", 0)
            .is_err()
    );
}

/// Verifies explicit `/compact` is forced even when the entire transcript fits
/// inside the normal retained-tail budget.
///
/// The user command is a direct request to compact now, so it must summarize at
/// least one active durable entry instead of returning a budget-based no-op.
#[test]
fn runtime_agent_shell_compact_forces_summary_when_under_context_budget() {
    // Literal historical reference has no task or execution closure to infer.
    // Its tiny cost is well inside the configured 10% raw allowance, but the
    // explicit command still selects one eligible original occurrence.
    let (mut service, transcript_store) =
        manual_model::queued("as-forced", "forced compact marker 1".into());
    let archive = transcript_store.inspect("as-forced").unwrap();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(task.summarized_entries, 1);
    assert!(mez_agent::provider_text_input_token_estimate(&archive[0].content) < 2000);
    let memory_before = service.memory_records().len();
    complete_runtime_test_compaction(&mut service, "%1", "forced compact marker 1");
    assert!(
        transcript_store
            .compaction_epoch("as-forced")
            .unwrap()
            .is_none()
    );
    assert!(service.agent_is_compacting("%1"));
    let receipt = replay_publication::execute_pending_publication(&mut service);
    assert!(service.agent_is_compacting("%1"));
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let (_, compacted) = AgentTranscriptStore::new(transcript_store.root())
        .load_replay_epoch("as-forced", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(compacted.revision().get(), 1);
    assert_eq!(compacted.context().chronology().len(), 1);
    assert_eq!(
        compacted.context().chronology()[0].block().source,
        ContextSourceKind::CompactionPrefix
    );
    assert_eq!(
        compacted.context().chronology()[0].block().content,
        "forced compact marker 1"
    );
    assert_eq!(transcript_store.inspect("as-forced").unwrap(), archive);
    assert_eq!(service.memory_records().len(), memory_before);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Verifies a non-reducing configured-cap pass retries instead of failing.
///
/// The cap measures estimated wire bytes while the compaction planner budgets
/// words, so a code-heavy context can leave a pass that replaces fewer bytes than
/// its summary adds. That pass must tighten its budget and retry while the bounded
/// allowance remains, and only the exhausted allowance may end the turn - never an
/// internal "did not reduce" inconsistency error.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_retries_a_non_reducing_pass() {
    use crate::runtime::RuntimeSessionService;

    let first = RuntimeSessionService::plan_configured_input_cap_pass(0, None, 30_000, 20_000)
        .expect("the first pass of a turn always proceeds");
    assert_eq!(first.pass, 1);
    assert!(!first.non_reducing);

    let reduced =
        RuntimeSessionService::plan_configured_input_cap_pass(1, Some(30_000), 25_000, 20_000)
            .expect("a reducing pass proceeds");
    assert_eq!(reduced.pass, 2);
    assert!(!reduced.non_reducing);

    let non_reducing =
        RuntimeSessionService::plan_configured_input_cap_pass(1, Some(30_000), 31_000, 20_000)
            .expect("a non-reducing pass retries with a tightened budget");
    assert_eq!(non_reducing.pass, 2);
    assert!(non_reducing.non_reducing);

    let exhausted =
        RuntimeSessionService::plan_configured_input_cap_pass(4, Some(30_000), 31_000, 20_000)
            .expect_err("the exhausted allowance must end the turn");
    let message = exhausted.message().to_string();
    assert!(
        message.contains("configured input cap cannot be satisfied"),
        "the terminal outcome must be typed as an unsatisfiable cap: {message}"
    );
    assert!(
        !message.contains("did not reduce"),
        "an internal consistency message must never surface for this condition: {message}"
    );

    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(2_000, false),
        2_000,
        "an ordinary pass keeps the derived word budget"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(2_000, true),
        1_000,
        "a non-reducing pass retries against half the word budget"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(1, true),
        1,
        "a tightened budget never drops below one word"
    );
}

/// Verifies a durable block owned by another provider does not consume the
/// configured-cap summary budget when the recovery plans.
///
/// The configured-cap path plans through the active provider's rendered
/// projection, and request assembly never renders a block whose provider owner
/// does not match that provider. Without the projection, a foreign-owned native
/// block retained above the consumed boundary could bring the summary budget to
/// zero and fail the deferral with `no budget remains` instead of queueing the
/// compaction that makes the turn fit.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_plan_ignores_unrendered_provider_blocks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-projection".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-test"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
# The cap must clear the fixed provider request overhead this fixture observes -
# about 22.9k tokens of system prompt and tool schemas - or the pass lands in the
# "cap smaller than fixed overhead" terminal before the projection is exercised.
[model_profiles.configured-input-cap-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 200000
max_input_tokens = 60000
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-projection-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let transcript_store =
        AgentTranscriptStore::new(temp_root("configured-input-cap-projection-history"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "configured-input-cap-projection-history".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-history".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!(
                "configured-cap-projection-marker {}",
                "compactable ".repeat(20_000)
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "configured-input-cap-projection-history", 1)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-projection","input":"continue after proactive compaction"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();

    let foreign_content = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(
        (0..3000)
            .map(|index| {
                serde_json::json!({
                    "type": "reasoning",
                    "id": format!("foreign-{index}"),
                    "text": "foreign provider continuity replay segment",
                })
            })
            .collect(),
    )
    .unwrap()
    .to_transcript_content();
    let foreign_owner = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "configured-openai",
    )
    .unwrap();
    let foreign_group =
        mez_agent::ContextExecutionGroupId::new("foreign-native-execution").unwrap();
    let turn_context = service
        .agent_turn_contexts_mut()
        .get_mut(&task.turn_id)
        .expect("the pending turn owns a durable context");
    turn_context
        .append_assistant_event(
            "foreign native turn",
            "foreign call emitted ahead of its transcript",
            foreign_group.clone(),
        )
        .unwrap();
    turn_context
        .append_evidence_event(
            mez_agent::ContextSourceKind::TranscriptTool,
            "foreign native response",
            foreign_content,
            foreign_group,
            Some(foreign_owner),
            false,
        )
        .unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none(),
        "the configured cap must still defer into compaction"
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some(),
        "the unrendered provider block must not exhaust the summary budget"
    );
}

/// Verifies the configured-cap word budget is measured rather than assumed.
///
/// The planner budgets words while the cap measures estimated provider tokens, so
/// the budget converts the measured token allowance at the context's own
/// words-per-token ratio: code-heavy text with few words per token gets a smaller
/// word budget than prose with the same allowance, which is what makes the planned
/// reduction match the measured one.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_budget_uses_the_measured_word_ratio() {
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 250, 1_000),
        250,
        "code-heavy text plans one word per four available tokens"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 750, 1_000),
        750,
        "prose plans three words per four available tokens"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 0, 0),
        0,
        "an empty rendered projection leaves no word budget"
    );
}
