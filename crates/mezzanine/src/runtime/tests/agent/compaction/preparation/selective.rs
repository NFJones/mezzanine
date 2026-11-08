//! Manual compaction refuses historical selective state before model rendering.
//!
//! Real archives preserve the prior ordering, boundary, budget and open-barrier
//! layouts. None permits implicit normalization or covered-source resubmission.

use super::*;

/// Creates one valid range inside a closed turn, or after an independent turn.
/// Sharing the first turn makes its display row and covered execution rows one
/// indivisible manual group while preserving the range's canonical boundaries.
fn selective_fixture(shared_turn: bool) -> (RuntimeSessionService, ClientId, AgentTranscriptStore) {
    let (mut service, primary, _) = fixture("manual-selective-source");
    let store = AgentTranscriptStore::new(temp_root("manual-selective-range"));
    for sequence in 1..=4 {
        let covered = (2..=3).contains(&sequence);
        let content = if covered {
            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                ContextSourceKind::TranscriptAssistant,
                "assistant",
                "COVERED_RAW_MUST_NOT_RETURN",
                mez_agent::ContextExecutionGroupId::new("covered-group").unwrap(),
                sequence - 1,
                None,
            )
            .unwrap()
            .to_transcript_content()
        } else if sequence == 1 {
            "RAW_FIRST_GROUP".into()
        } else {
            "RAW_RETAINED_TAIL".into()
        };
        store
            .append(&TranscriptEntry {
                conversation_id: "selective-manual".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if covered {
                    TranscriptRole::System
                } else {
                    TranscriptRole::Assistant
                },
                turn_id: if covered || (shared_turn && sequence == 1) {
                    "covered-turn".into()
                } else {
                    format!("closed-{sequence}")
                },
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content,
            })
            .unwrap();
    }
    store
        .install_legacy_compaction_ranges_fixture(
            "selective-manual",
            0,
            "",
            vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 2,
                through_sequence: 3,
                summary: "SELECTIVE_SUMMARY_ONCE".into(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "selective-manual", 4)
        .unwrap();
    (service, primary, store)
}

/// Drives actual command/source completion and proves refusal precedes request
/// rendering, provider dispatch or durable source mutation.
fn assert_selective_manual_refused(
    service: &mut RuntimeSessionService,
    primary: &ClientId,
    store: &AgentTranscriptStore,
) {
    let paths = [
        store.transcript_path("selective-manual").unwrap(),
        store.root().join("selective-manual/compaction-epoch.json"),
    ];
    let before = paths.each_ref().map(|path| std::fs::read(path).unwrap());
    let queued = service
        .execute_agent_shell_command(primary, "/compact")
        .unwrap();
    assert!(queued.contains("state=preparing"), "{queued}");
    let source = service.take_manual_compaction_preparations().pop().unwrap();
    let result = source.execute_source();
    assert!(
        result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("normalization")
    );
    let error = service
        .complete_manual_compaction_preparation(&source, result)
        .unwrap_err();
    assert!(error.to_string().contains("normalization"), "{error}");
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.take_manual_compaction_requests().is_empty());
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    assert_eq!(
        paths.each_ref().map(|path| std::fs::read(path).unwrap()),
        before
    );
}

/// A range inside a complete turn still needs explicit normalization. It must
/// not be reopened as raw source or silently folded into another summary.
#[test]
fn runtime_manual_compaction_selective_range_replaces_covered_raw_input() {
    let (mut service, primary, store) = selective_fixture(true);
    assert_selective_manual_refused(&mut service, &primary, &store);
}

/// A later selective summary must not become eligible through an earlier raw
/// turn. The format gate runs before choosing a new prefix or rendering input.
#[test]
fn runtime_manual_compaction_selective_summary_after_boundary_stays_out_of_input() {
    let (mut service, primary, store) = selective_fixture(false);
    assert_selective_manual_refused(&mut service, &primary, &store);
}

/// Builds two early closed turns, a selective range, and a later exact tail.
/// Range endpoints can span turns so both grouping domains must remain intact.
fn extended_fixture(
    split_turns: bool,
    covered_words: usize,
) -> (RuntimeSessionService, ClientId, AgentTranscriptStore) {
    let (mut service, primary, _) = fixture("manual-selective-atoms");
    let store = AgentTranscriptStore::new(temp_root("manual-selective-atoms"));
    for sequence in 1..=5 {
        let covered = (3..=4).contains(&sequence);
        let content = if covered {
            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                ContextSourceKind::TranscriptAssistant,
                "assistant",
                "COVERED_BUDGET_RAW ".repeat(covered_words),
                mez_agent::ContextExecutionGroupId::new(if split_turns && sequence == 4 {
                    "second-covered"
                } else {
                    "first-covered"
                })
                .unwrap(),
                if split_turns { 1 } else { sequence - 2 },
                None,
            )
            .unwrap()
            .to_transcript_content()
        } else {
            format!("uncovered completed row {sequence}")
        };
        store
            .append(&TranscriptEntry {
                conversation_id: "selective-manual".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if covered {
                    TranscriptRole::System
                } else {
                    TranscriptRole::Assistant
                },
                turn_id: if (2..=4).contains(&sequence) && !(split_turns && sequence == 4) {
                    "first-covered-turn".into()
                } else {
                    format!("turn-{sequence}")
                },
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content,
            })
            .unwrap();
    }
    store
        .install_legacy_compaction_ranges_fixture(
            "selective-manual",
            0,
            "",
            vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 3,
                through_sequence: 4,
                summary: "small authoritative summary".into(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "selective-manual", 5)
        .unwrap();
    (service, primary, store)
}

/// Large covered raw data cannot bypass admission merely because its stored
/// summary is small. Refusal must not expand that raw source into model input.
#[test]
fn runtime_manual_compaction_selective_budget_uses_summary_cost() {
    let (mut service, primary, store) = extended_fixture(false, 20000);
    assert_selective_manual_refused(&mut service, &primary, &store);
}

/// A tight budget cannot split a historical range across turns into a purported
/// new prefix. The complete old projection remains intact on format refusal.
#[test]
fn runtime_manual_compaction_selective_range_is_atomic_across_turns() {
    let (mut service, primary, store) = extended_fixture(true, 2);
    let mut layers = service.integration.config_layers().to_vec();
    for layer in &mut layers {
        layer.text = layer
            .text
            .replace("context_window_tokens=128000", "context_window_tokens=100");
    }
    service.replace_config_layers(layers).unwrap();
    assert_selective_manual_refused(&mut service, &primary, &store);
}

/// Covered closed work after an uncovered open user turn cannot authorize
/// skipping that turn. The original archive and range remain byte-for-byte
/// unchanged, with neither a rendered request nor provider task queued.
#[test]
fn runtime_manual_compaction_selective_summary_does_not_bypass_open_barrier() {
    let (mut service, primary, original) = selective_fixture(false);
    let store = AgentTranscriptStore::new(temp_root("manual-selective-open-barrier"));
    for mut row in original.inspect("selective-manual").unwrap() {
        if row.sequence == 1 {
            row.role = TranscriptRole::User;
        }
        store.append(&row).unwrap();
    }
    let epoch = original
        .compaction_epoch("selective-manual")
        .unwrap()
        .unwrap();
    store
        .install_legacy_compaction_ranges_fixture("selective-manual", 0, "", epoch.ranges)
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    assert_selective_manual_refused(&mut service, &primary, &store);
    assert!(
        store
            .compaction_epoch("selective-manual")
            .unwrap()
            .unwrap()
            .summary
            .is_empty()
    );
}

/// An independent compactor changes the head without rewriting audit rows.
/// Completion from the earlier capture must fail the actual publisher's parent
/// CAS, retaining the newer authority and protected raw groups exactly.
#[test]
fn runtime_manual_compaction_selective_epoch_edit_survives_old_completion() {
    let (mut service, primary, old) = selective_fixture(true);
    let store = AgentTranscriptStore::new(temp_root("manual-supported-prefix-source"));
    for mut row in old.inspect("selective-manual").unwrap() {
        // The race needs eligible historical input, not inferred closure for
        // an assistant role. The explicit grouped source remains protected.
        if row.role == TranscriptRole::Assistant {
            row.role = TranscriptRole::User;
        }
        store.append(&row).unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    // Capture a real request after the supported prefix is normalized. The
    // historical selective layout remains a separate refusal control above.
    store
        .install_legacy_compaction_ranges_fixture(
            "selective-manual",
            0,
            "SUPPORTED_PREFIX",
            Vec::new(),
        )
        .unwrap();
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert!(
        service
            .complete_manual_compaction_request(&work, Ok(task))
            .unwrap()
    );
    let before = store.inspect("selective-manual").unwrap();
    let (parent, current) = store
        .load_replay_epoch("selective-manual", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    let plan = match current
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(128_000, 1).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("independent original source expected: {other:?}"),
    };
    let proposal = store
        .prepare_replay_epoch(
            crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                "independent-compactor",
                "agent-%1",
                "%1",
            )
            .unwrap()
            .capture(None),
            "selective-manual",
            Some(parent),
            &current,
            plan,
            mez_agent::ReplayHeadPublication::Summary("NEW_AUTHORITATIVE_RANGE_SUMMARY".into()),
        )
        .unwrap();
    let replacement = store.publish_replay_epoch(&proposal).unwrap();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", task);
    service
        .apply_agent_compaction_completed_event(
            "%1",
            runtime_test_compaction_response("OLD_CAPTURED_RESULT"),
        )
        .unwrap();
    let completion = service
        .take_replay_epoch_attempt_for_tests()
        .unwrap()
        .execute();
    assert!(completion.error().is_some());
    service.apply_replay_epoch_completion(completion).unwrap();
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    let (identity, cold) = store
        .load_replay_epoch("selective-manual", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(&identity, replacement.identity());
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "NEW_AUTHORITATIVE_RANGE_SUMMARY"
    );
    assert_eq!(store.inspect("selective-manual").unwrap(), before);
}
