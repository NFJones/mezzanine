//! Manual compaction consumes selective summaries without covered raw replay.
//!
//! Real archives and command preparation qualify source ordering, future-summary
//! exclusion, physical counts and protected leading groups without model calls.

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
        .save_compaction_ranges(
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

/// Applies a controlled test summary through the actual task completion path,
/// reopens storage, and resumes the conversation before reading provider context.
/// This qualifies publication/replay, not external model quality or billing.
fn complete_and_resume(
    service: &mut RuntimeSessionService,
    primary: &ClientId,
    store: &AgentTranscriptStore,
    work: &crate::runtime::RuntimeManualCompactionRequestWork,
    task: crate::runtime::RuntimeAgentCompactionTask,
    summary: &str,
) -> mez_agent::AgentContext {
    assert!(
        service
            .complete_manual_compaction_request(work, Ok(task))
            .unwrap()
    );
    complete_runtime_test_compaction(service, "%1", summary);
    assert!(!service.agent_is_compacting("%1"));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root().to_path_buf()));
    let response = service
        .execute_agent_shell_command(primary, "/resume selective-manual")
        .unwrap();
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap_or(response);
    assert!(settled.contains("resumed=true"), "{settled}");
    service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap()
}

/// A range inside the selected complete turn enters the compactor exactly once
/// as its authoritative summary. Its covered raw bytes must not return. Frozen
/// publication proof and retained counts still refer to all three physical
/// source rows, independently of the reduced model representation.
#[test]
fn runtime_manual_compaction_selective_range_replaces_covered_raw_input() {
    let (mut service, primary, store) = selective_fixture(true);
    let original = store.inspect("selective-manual").unwrap();
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert_eq!(task.compacted_through_sequence, Some(3));
    assert_eq!(task.retained_transcript_entries, 1);
    assert_eq!(
        task.frozen_compaction_rows
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let source = &task.request.messages.last().unwrap().content;
    assert!(!source.contains("COVERED_RAW_MUST_NOT_RETURN"), "{source}");
    assert_eq!(
        source.matches("SELECTIVE_SUMMARY_ONCE").count(),
        1,
        "{source}"
    );
    assert!(
        source.find("RAW_FIRST_GROUP").unwrap() < source.find("SELECTIVE_SUMMARY_ONCE").unwrap()
    );
    assert!(!source.contains("RAW_RETAINED_TAIL"));
    let context = complete_and_resume(
        &mut service,
        &primary,
        &store,
        &work,
        task,
        "COMPACTED_PREFIX_RESULT",
    );
    let epoch = store.compaction_epoch("selective-manual").unwrap().unwrap();
    assert_eq!(epoch.through_sequence, 3);
    assert!(epoch.ranges.is_empty());
    assert_eq!(
        &store.inspect("selective-manual").unwrap()[..4],
        original.as_slice()
    );
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content.contains("COMPACTED_PREFIX_RESULT"))
            .count(),
        1
    );
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("RAW_RETAINED_TAIL"))
    );
    assert!(!context.blocks().iter().any(|block| {
        block.content.contains("COVERED_RAW_MUST_NOT_RETURN")
            || block.content.contains("SELECTIVE_SUMMARY_ONCE")
            || block.content.contains("RAW_FIRST_GROUP")
    }));
}

/// A fitting forced prefix selects only the earlier independent closed turn.
/// A later range's summary must stay in the retained projection rather than
/// enter the new prefix summary and then appear a second time after publication.
#[test]
fn runtime_manual_compaction_selective_summary_after_boundary_stays_out_of_input() {
    let (mut service, primary, store) = selective_fixture(false);
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert_eq!(task.compacted_through_sequence, Some(1));
    assert_eq!(task.retained_transcript_entries, 3);
    let source = &task.request.messages.last().unwrap().content;
    assert!(source.contains("RAW_FIRST_GROUP"));
    assert!(!source.contains("SELECTIVE_SUMMARY_ONCE"), "{source}");
    assert!(!source.contains("COVERED_RAW_MUST_NOT_RETURN"));
    assert!(!source.contains("RAW_RETAINED_TAIL"));
    let context = complete_and_resume(
        &mut service,
        &primary,
        &store,
        &work,
        task,
        "EARLY_PREFIX_RESULT",
    );
    let epoch = store.compaction_epoch("selective-manual").unwrap().unwrap();
    assert_eq!(epoch.through_sequence, 1);
    assert_eq!(epoch.ranges.len(), 1);
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "SELECTIVE_SUMMARY_ONCE")
            .count(),
        1
    );
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content.contains("EARLY_PREFIX_RESULT"))
            .count(),
        1
    );
    assert!(!context.blocks().iter().any(|block| {
        block.content.contains("COVERED_RAW_MUST_NOT_RETURN")
            || block.content.contains("RAW_FIRST_GROUP")
    }));
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
        .save_compaction_ranges(
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

/// A large archived range has the cost of its small authoritative summary.
/// Charging its covered raw bytes would unnecessarily summarize the second
/// turn; correct budgeting retains that turn and selects only the first one.
#[test]
fn runtime_manual_compaction_selective_budget_uses_summary_cost() {
    let (mut service, primary, _) = extended_fixture(false, 20000);
    let task = request_work(&mut service, &primary)
        .execute_request()
        .unwrap();
    assert_eq!(task.compacted_through_sequence, Some(1));
    assert_eq!(task.retained_transcript_entries, 4);
    assert!(
        !task
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("COVERED_BUDGET_RAW")
    );
    service.cancel_current_agent_compaction_task("%1");
}

/// A range may cover complete execution groups in multiple turns. A tight
/// budget cannot cut between those turns and publish a prefix through the
/// middle of their shared range. The full physical prefix remains the proof.
#[test]
fn runtime_manual_compaction_selective_range_is_atomic_across_turns() {
    let (mut service, primary, _) = extended_fixture(true, 2);
    let mut layers = service.integration.config_layers().to_vec();
    for layer in &mut layers {
        layer.text = layer
            .text
            .replace("context_window_tokens=128000", "context_window_tokens=100");
    }
    service.replace_config_layers(layers).unwrap();
    let task = request_work(&mut service, &primary)
        .execute_request()
        .unwrap();
    assert_eq!(task.compacted_through_sequence, Some(5));
    assert_eq!(task.retained_transcript_entries, 0);
    assert_eq!(task.frozen_compaction_rows.len(), 5);
    assert_eq!(
        task.request
            .messages
            .last()
            .unwrap()
            .content
            .matches("small authoritative summary")
            .count(),
        1
    );
    service.cancel_current_agent_compaction_task("%1");
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
        .save_compaction_ranges("selective-manual", 0, "", epoch.ranges)
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let before = store.inspect("selective-manual").unwrap();
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let work = service.take_manual_compaction_preparations().pop().unwrap();
    assert!(
        service
            .complete_manual_compaction_preparation(&work, work.execute_source())
            .unwrap()
    );
    assert!(service.take_manual_compaction_requests().is_empty());
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(store.inspect("selective-manual").unwrap(), before);
    assert!(
        store
            .compaction_epoch("selective-manual")
            .unwrap()
            .unwrap()
            .summary
            .is_empty()
    );
}

/// A cooperating publisher may replace a summary without changing any physical
/// row. Completion from an earlier immutable source must not absorb and discard
/// that newer authority merely because its frozen raw rows still match. This
/// exercises actual task adoption, epoch publication and controlled completion;
/// it makes no claim about external provider execution or summary quality.
#[test]
fn runtime_manual_compaction_selective_epoch_edit_survives_old_completion() {
    let (mut service, primary, store) = selective_fixture(true);
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert!(
        service
            .complete_manual_compaction_request(&work, Ok(task))
            .unwrap()
    );
    let mut replacement = store.compaction_epoch("selective-manual").unwrap().unwrap();
    replacement.ranges[0].summary = "NEW_AUTHORITATIVE_RANGE_SUMMARY".into();
    store
        .save_compaction_ranges(
            "selective-manual",
            replacement.through_sequence,
            &replacement.summary,
            replacement.ranges.clone(),
        )
        .unwrap();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", task);
    let _completion = service.apply_agent_compaction_completed_event(
        "%1",
        runtime_test_compaction_response("OLD_CAPTURED_RESULT"),
    );
    assert_eq!(
        store.compaction_epoch("selective-manual").unwrap(),
        Some(replacement),
        "an old compactor cannot replace a later authoritative range summary"
    );
}
