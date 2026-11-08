//! Manual frozen history, tail policy and genuine model-work recovery.

use super::*;

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

/// A usable long manual summary publishes once even when the next ordinary
/// request estimate exceeds the configured input cap.
#[test]
fn runtime_manual_compaction_accepts_long_final_without_fit_regeneration() {
    let (mut service, store) =
        manual_model::queued("manual-long-final", "old decision ".repeat(800));
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let summary = format!("{}END", "usable continuation ".repeat(5000));
    let candidate = service
        .manual_compaction_candidate_context(&task, &summary)
        .unwrap();
    let (estimated, limit, _) = service
        .manual_compaction_candidate_size_for_tests(&task, candidate)
        .unwrap();
    assert!(estimated > limit);
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&summary)),
    );
    assert_eq!(sent.len(), 1);
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(
        store
            .load_replay_epoch("manual-long-final", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let receipt = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    let (_, cold) = store
        .load_replay_epoch("manual-long-final", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology()[0].block().content, summary);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_ledger().turns().is_empty());
}

/// Packing uses the actual complete auxiliary input cap, preserves all source,
/// and exposes no intermediate head during bounded parallel waves.
#[test]
fn runtime_manual_compaction_splits_configured_input_cap() {
    const CID: &str = "manual-input-packing";
    let (mut service, store) = manual_model::queued(CID, "frozen manual source ".repeat(40000));
    let before = store.inspect(CID).unwrap();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let configured = service
        .provider_registry()
        .provider(&task.model_profile.provider)
        .unwrap();
    let api = mez_agent::resolve_provider_api(&configured.kind, configured.api.as_deref()).unwrap();
    let options =
        crate::runtime::config::runtime_effective_provider_options(configured, &task.model_profile);
    let mut calls = 0;
    for _ in 0..128 {
        if service.pending_agent_compaction_tasks().is_empty() {
            break;
        }
        let sent = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response("one final manual head")),
        );
        assert!(sent.len() <= 4);
        for request in &sent {
            let estimate =
                mez_agent::provider_request_input_estimate(request, api, &options, false).unwrap();
            assert!(estimate.input_tokens <= 20000);
        }
        calls += sent.len();
        assert!(
            store
                .load_replay_epoch(CID, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert_eq!(store.inspect(CID).unwrap(), before);
    }
    assert!(calls > 4, "the actual source must require multiple waves");
    let receipt = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    let (_, cold) = store
        .load_replay_epoch(CID, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "one final manual head"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// An actual input rejection repacks private input while retaining the same
/// source/epoch and original durable history until the final receipt is adopted.
#[test]
fn runtime_manual_compaction_recovers_provider_context_limit() {
    const CID: &str = "manual-context-rejection";
    let (mut service, store) = manual_model::queued(CID, "original manual source ".repeat(1000));
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let before = store.inspect(CID).unwrap();
    let rejected = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(r#"{"error":{"code":"context_length_exceeded"}}"#)),
    );
    assert_eq!(rejected.len(), 1);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(retry.compaction_epoch, original.compaction_epoch);
    assert!(std::sync::Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        original.replay_source.as_ref().unwrap()
    ));
    for _ in 0..32 {
        if service.pending_agent_compaction_tasks().is_empty() {
            break;
        }
        complete_runtime_test_compaction(
            &mut service,
            "%1",
            "accepted manual head after input rejection",
        );
        assert_eq!(store.inspect(CID).unwrap(), before);
        assert!(
            store
                .load_replay_epoch(CID, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
    let receipt = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_ledger().turns().is_empty());
}

/// A failure after accepted raw siblings cannot publish a partial summary or
/// shorten the exact original replay authority.
#[test]
fn runtime_manual_compaction_partial_chunk_failure_preserves_history() {
    const CID: &str = "manual-private-failure";
    let (mut service, store) = manual_model::queued(CID, "original manual source ".repeat(1000));
    let before = store.inspect(CID).unwrap();
    run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(r#"{"error":{"code":"context_length_exceeded"}}"#)),
    );
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(
            "private accepted material",
        )),
    );
    assert!(!sent.is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::new(
            crate::error::MezErrorKind::Forbidden,
            "final synthesis refused",
        )),
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(
        store
            .load_replay_epoch(CID, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect(CID).unwrap(), before);
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
