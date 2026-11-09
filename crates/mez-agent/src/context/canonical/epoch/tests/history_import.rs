//! Initial historical allocation must not weaken live arrival or task fences.

use super::*;
use crate::{ContextBlockRecord, ImportedExecutionEvent};

/// Descriptive runtime observations are readable source, not pending execution.
/// Initial ownership must leave an unrelated real assistant group protected.
#[test]
fn history_import_observation_does_not_acquire_execution_protection() {
    let pending = ContextExecutionGroupId::new("pending-assistant").unwrap();
    let observation = ContextExecutionGroupId::new("runtime-observation").unwrap();
    let blocks = vec![
        ContextBlock::assistant_event("assistant", "unfinished work"),
        ContextBlock::evidence_event(ContextSourceKind::CommittedEvidence, "counters", "one call"),
    ];
    let records = records(&blocks);
    let execution = [pending.clone(), observation.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, group)| {
            ImportedExecutionEvent::new(blocks[index].clone(), group, 1, None)
                .unwrap()
                .with_record(records[index].1)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let owner = ReplayEpochContext::from_imported_history(
        ReplayConversationId::new("observed-history").unwrap(),
        blocks,
        &records,
        &execution,
        &[],
        &[],
    )
    .unwrap();
    assert!(owner.context().execution_group_is_pending(&pending));
    assert!(!owner.context().execution_group_is_pending(&observation));
    assert!(
        owner
            .capture_execution_owner(&observation, owner.context().chronology()[1].sequence())
            .is_err()
    );
    assert!(owner.execution_states().unwrap().is_empty());
    assert_eq!(owner.context().chronology()[1].record(), records[1].1);
}

fn records(blocks: &[ContextBlock]) -> Vec<(usize, ContextBlockRecord)> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (index, ContextBlockRecord::allocate(block).unwrap()))
        .collect()
}

#[test]
fn history_import_dense_tasks_preserve_records_protection_and_next_live_frontier() {
    let group = ContextExecutionGroupId::new("imported-pending").unwrap();
    let blocks = vec![
        ContextBlock::task_prelude(ContextSourceKind::SkillInstruction, "skill", "first skill"),
        ContextBlock::user_event("user prompt", "first task"),
        ContextBlock::assistant_event("assistant", "unfinished execution"),
        ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "unconsumed peer"),
        ContextBlock::task_prelude(ContextSourceKind::SkillInstruction, "skill", "next skill"),
        ContextBlock::user_event("user prompt", "next task"),
    ];
    let records = records(&blocks);
    let tasks = [
        (records[1].1.occurrence_id(), records[0].1.occurrence_id()),
        (records[5].1.occurrence_id(), records[4].1.occurrence_id()),
    ];
    let execution = [
        ImportedExecutionEvent::new(blocks[2].clone(), group.clone(), 1, None)
            .unwrap()
            .with_record(records[2].1)
            .unwrap(),
    ];
    let mut owner = ReplayEpochContext::from_imported_history(
        ReplayConversationId::new("dense-history").unwrap(),
        blocks.clone(),
        &records,
        &execution,
        &[
            (
                records[1].1.occurrence_id(),
                ContextEventSequence::new(8).unwrap(),
            ),
            (
                records[5].1.occurrence_id(),
                ContextEventSequence::new(16).unwrap(),
            ),
        ],
        &tasks,
    )
    .unwrap();
    assert_eq!(
        owner
            .context()
            .chronology()
            .iter()
            .map(|e| e.sequence().get())
            .collect::<Vec<_>>(),
        [4, 8, 10, 12, 14, 16]
    );
    for (index, event) in owner.context().chronology().iter().enumerate() {
        assert_eq!(event.record(), records[index].1);
        assert_eq!(event.block(), &blocks[index]);
        assert_eq!(owner.members()[index].origin(), event.sequence());
    }
    for index in [0, 1] {
        assert_eq!(
            owner.context().chronology()[index].retention(),
            ContextRetention::Summarizable
        );
    }
    for index in [4, 5] {
        assert_eq!(
            owner.context().chronology()[index].retention(),
            ContextRetention::Exact
        );
    }
    assert!(owner.context().execution_group_is_pending(&group));
    assert!(
        owner
            .context()
            .peer_message_is_pending(ContextEventSequence::new(12).unwrap())
    );
    assert!(
        owner.execution_states().unwrap().is_empty(),
        "import is not producer execution evidence"
    );
    for (root, first) in tasks {
        assert_eq!(owner.task_admission_start(root), Some(first));
    }
    let before = owner.clone();
    let block = ContextBlock::user_event("user prompt", "invalid earlier arrival");
    let record = ContextBlockRecord::allocate(&block).unwrap();
    assert!(
        owner
            .restore_instruction_task(
                vec![block],
                &[(0, record)],
                &[],
                0,
                &[(0, ContextEventSequence::new(17).unwrap())]
            )
            .is_err()
    );
    assert_eq!(
        owner, before,
        "rejected replay cannot retire the active task"
    );
    owner
        .admit_instruction_task(
            vec![ContextBlock::user_event("user prompt", "new live task")],
            &[],
            &[],
            0,
        )
        .unwrap();
    assert_eq!(owner.context().chronology()[6].sequence().get(), 1040);
    owner.validate_source_extension(&before).unwrap();
    assert!(owner.context().execution_group_is_pending(&group));
}

#[test]
fn history_import_rejects_conflicting_exhausted_and_unresolved_anchors() {
    let blocks = vec![
        ContextBlock::reference_event(ContextSourceKind::TranscriptUser, "a", "a"),
        ContextBlock::reference_event(ContextSourceKind::TranscriptUser, "b", "b"),
    ];
    let records = records(&blocks);
    let a = records[0].1.occurrence_id();
    let b = records[1].1.occurrence_id();
    let seq = |value| ContextEventSequence::new(value).unwrap();
    for anchors in [
        vec![(b, seq(1))],
        vec![(a, seq(8)), (b, seq(8))],
        vec![(a, seq(9)), (b, seq(8))],
        vec![(a, seq(8)), (a, seq(8))],
        vec![(b, seq(u64::MAX))],
        vec![(ContextBlockOccurrenceId::allocate(), seq(8))],
    ] {
        assert!(
            ReplayEpochContext::from_imported_history(
                ReplayConversationId::new("invalid-history").unwrap(),
                blocks.clone(),
                &records,
                &[],
                &anchors,
                &[]
            )
            .is_err()
        );
    }
    let original = blocks.clone();
    let mut duplicate = records.clone();
    duplicate[1].1 = duplicate[0].1;
    for invalid in [
        duplicate,
        vec![records[0]],
        vec![(1, records[0].1), (0, records[1].1)],
    ] {
        assert!(
            ReplayEpochContext::from_imported_history(
                ReplayConversationId::new("invalid-history").unwrap(),
                blocks.clone(),
                &invalid,
                &[],
                &[],
                &[]
            )
            .is_err()
        );
    }
    assert_eq!(blocks, original);
}

#[test]
fn history_import_requires_explicit_nonoverlapping_task_boundaries() {
    let blocks = vec![
        ContextBlock::user_event("user prompt", "one"),
        ContextBlock::user_event("user prompt", "two"),
    ];
    let records = records(&blocks);
    let a = records[0].1.occurrence_id();
    let b = records[1].1.occurrence_id();
    for tasks in [
        vec![],
        vec![(a, b)],
        vec![(a, a), (b, a)],
        vec![(a, a), (a, a)],
        vec![(a, ContextBlockOccurrenceId::allocate())],
    ] {
        assert!(
            ReplayEpochContext::from_imported_history(
                ReplayConversationId::new("invalid-tasks").unwrap(),
                blocks.clone(),
                &records,
                &[],
                &[],
                &tasks
            )
            .is_err()
        );
    }
    let empty = ReplayEpochContext::from_imported_history(
        ReplayConversationId::new("empty").unwrap(),
        vec![],
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap();
    assert!(empty.members().is_empty());
    let no_anchors = ReplayEpochContext::from_imported_history(
        ReplayConversationId::new("unanchored").unwrap(),
        blocks,
        &records,
        &[],
        &[],
        &[(a, a), (b, b)],
    )
    .unwrap();
    assert_eq!(no_anchors.context().chronology()[0].sequence().get(), 1024);
    assert_eq!(no_anchors.context().chronology()[1].sequence().get(), 2048);
}
