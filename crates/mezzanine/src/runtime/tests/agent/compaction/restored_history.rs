//! Durable compaction history across runtime restoration and later appends.

use super::*;

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
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "COMPACTION_SUMMARY_RESTORED_AFTER_RESTART",
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
