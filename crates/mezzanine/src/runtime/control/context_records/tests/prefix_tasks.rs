//! Persisted prefix migration preserves original task reservations and backing.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use mez_agent::{ReplayConversationId, ReplayEpochContext};

pub(super) const CID: &str = "prefix-recorded-tasks";

pub(super) enum Source {
    Tasks,
    RetiredCatalog,
    MixedLegacy { collision: bool },
}

/// Real task admissions supply the original boundaries. The historical prefix
/// can cover the first task's prelude while retaining its prompt and steering.
fn fixture(through: u64) -> (Directory, AgentTranscriptStore, ReplayEpochContext) {
    fixture_with_source(through, Source::Tasks)
}

pub(super) fn fixture_with_source(
    through: u64,
    source: Source,
) -> (Directory, AgentTranscriptStore, ReplayEpochContext) {
    let catalog = matches!(source, Source::RetiredCatalog);
    let mixed = matches!(source, Source::MixedLegacy { .. });
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new(CID).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    owner
        .append_fact("old source", "already summarized source")
        .unwrap();
    let mut turns = vec!["original-task-0".to_owned()];
    for task in 1..=2 {
        let first = owner.context().chronology().len();
        let prelude = if catalog && task == 1 {
            ContextBlock::task_prelude(
                ContextSourceKind::McpCatalogSnapshot,
                mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
                "original configured catalog",
            )
        } else {
            ContextBlock::task_prelude(
                ContextSourceKind::SkillInstruction,
                "task skill",
                format!("original skill {task}"),
            )
        };
        owner
            .admit_instruction_task(
                vec![
                    prelude,
                    ContextBlock::user_event("user prompt", format!("original task {task}")),
                    ContextBlock::user_event("user steering", format!("original steering {task}")),
                ],
                &[],
                &[],
                1,
            )
            .unwrap();
        if task == 1 && matches!(source, Source::MixedLegacy { collision: true }) {
            let group =
                mez_agent::ContextExecutionGroupId::new("compat-execution-group-2").unwrap();
            let frontier = owner
                .append_execution_events([mez_agent::ContextConversationAppend::assistant(
                    "original explicit assistant",
                    "original explicitly grouped answer",
                    group.clone(),
                )])
                .unwrap()[0];
            let completion = owner.capture_execution_owner(&group, frontier).unwrap();
            owner.finish_execution_group(&completion).unwrap();
        }
        turns.extend(std::iter::repeat_n(
            format!("original-task-{task}"),
            owner.context().chronology().len() - first,
        ));
    }
    let mut ordinals = std::collections::BTreeMap::<mez_agent::ContextExecutionGroupId, u64>::new();
    let mut rows = owner
        .context()
        .chronology()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let block = event.block();
            let semantic = if let Some(start) =
                owner.task_admission_start(event.record().occurrence_id())
            {
                TranscriptContextEvent::task_admission(
                    event.sequence().get(),
                    start,
                    &block.label,
                    &block.content,
                )
            } else if block.source == ContextSourceKind::UserInstruction {
                TranscriptContextEvent::user_event(
                    event.sequence().get(),
                    &block.label,
                    &block.content,
                )
            } else if let Some(group) = event.execution_group_id() {
                let ordinal = ordinals.entry(group.clone()).or_default();
                *ordinal += 1;
                TranscriptContextEvent::execution_block_with_metadata(
                    block.source,
                    &block.label,
                    &block.content,
                    group.clone(),
                    *ordinal,
                    event.provider_owner().cloned(),
                )
            } else if block.source == ContextSourceKind::McpCatalogSnapshot {
                TranscriptContextEvent::mcp_catalog_snapshot(&block.content, event.sequence().get())
            } else {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
                    &block.label,
                    &block.content,
                )
            }
            .unwrap();
            TranscriptEntry {
                conversation_id: CID.into(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 123,
                role: TranscriptRole::System,
                turn_id: turns[index].clone(),
                agent_id: "original-agent".into(),
                pane_id: "original-pane".into(),
                content: TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            }
        })
        .collect::<Vec<_>>();
    if mixed {
        rows.insert(
            1,
            TranscriptEntry {
                conversation_id: CID.into(),
                sequence: 2,
                created_at_unix_seconds: 100,
                role: TranscriptRole::Assistant,
                turn_id: "original-legacy-turn".into(),
                agent_id: "original-legacy-agent".into(),
                pane_id: "original-pane".into(),
                content: "mixed original legacy answer".into(),
            },
        );
        for (index, row) in rows.iter_mut().enumerate() {
            row.sequence = index as u64 + 1;
        }
    }
    if catalog {
        rows.push(TranscriptEntry {
            conversation_id: CID.into(),
            sequence: rows.len() as u64 + 1,
            created_at_unix_seconds: 123,
            role: TranscriptRole::System,
            turn_id: "old-compaction-turn".into(),
            agent_id: "original-agent".into(),
            pane_id: "original-pane".into(),
            content: TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
        });
    }
    store
        .commit_accounted_transcript_batch(
            &crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap(),
            1,
        )
        .unwrap();
    store
        .save_compaction_epoch(CID, through, "original persisted task head")
        .unwrap();
    (directory, store, owner)
}

fn history(
    store: AgentTranscriptStore,
) -> Result<crate::runtime::control::context::RuntimeAgentTranscriptContext> {
    execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
        store,
        pending_capture_error: None,
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "different-reader-pane".into(),
            conversation_id: CID.into(),
            ephemeral_source_entries: None,
            active_entries: None,
            pending_entries: Default::default(),
        },
    })
}

/// A covered task-start occurrence remains audit backing, never model history.
/// Original sequence reservations survive migration and repeated cold reads.
#[test]
fn legacy_prefix_tasks_preserve_reservations_and_covered_dependencies() {
    for through in [1, 2] {
        let (_directory, store, original) = fixture(through);
        let archive = store.inspect(CID).unwrap();
        let first = original.context().chronology()[1].record();
        let expected = &original.context().chronology()[through as usize..];
        let initial = history(store.clone()).unwrap();
        let context = initial.records.import_context(initial.blocks).unwrap();
        let cold = history(AgentTranscriptStore::new(store.root())).unwrap();
        let cold = cold.records.import_context(cold.blocks).unwrap();
        for actual in [context, cold] {
            assert_eq!(actual.chronology().len(), expected.len() + 1);
            for (actual, expected) in actual.chronology()[1..].iter().zip(expected) {
                assert_eq!(actual.record(), expected.record());
                assert_eq!(actual.sequence(), expected.sequence());
                assert_eq!(actual.block(), expected.block());
            }
            if through == 2 {
                assert!(
                    !actual
                        .chronology()
                        .iter()
                        .any(|event| event.record() == first)
                );
            }
        }
        let header: serde_json::Value = serde_json::from_slice(
            &std::fs::read(store.root().join(CID).join("compaction-epoch.json")).unwrap(),
        )
        .unwrap();
        let items = header["manifest"]["source_escrow"].as_array().unwrap();
        assert!(
            items
                .iter()
                .any(|item| item["record"] == serde_json::to_value(first).unwrap())
        );
        assert!(
            items
                .iter()
                .any(|item| item["task_start"]
                    == serde_json::to_value(first.occurrence_id()).unwrap())
        );
        assert_eq!(store.inspect(CID).unwrap(), archive);
    }
}

/// MCP retirement removes old catalog visibility, not original task custody.
/// The required catalog can back its task root without restoring model content
/// or tool grants through the migrated head.
#[test]
fn legacy_prefix_tasks_keep_retired_catalog_as_audit_backing() {
    let (_directory, store, original) = fixture_with_source(2, Source::RetiredCatalog);
    let catalog = original.context().chronology()[1].record();
    let replayed = history(store.clone()).unwrap();
    let replayed = replayed.records.import_context(replayed.blocks).unwrap();
    assert!(
        !replayed
            .chronology()
            .iter()
            .any(|event| event.record().occurrence_id() == catalog.occurrence_id())
    );
    assert!(
        !replayed
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::McpCatalogSnapshot)
    );
    let header: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.root().join(CID).join("compaction-epoch.json")).unwrap(),
    )
    .unwrap();
    assert!(
        header["manifest"]["source_escrow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["record"] == serde_json::to_value(catalog).unwrap())
    );
}

/// A genuine later admission can append above the preserved reservation and
/// Legacy replies keep their grouping independently of recorded task partitions.
/// An explicit producer group can use the same spelling as an inferred group;
/// it must remain distinct without granting either source new lifecycle rights.
#[test]
fn legacy_prefix_tasks_preserve_mixed_legacy_and_explicit_groups() {
    for collision in [false, true] {
        let (_directory, store, original) =
            fixture_with_source(1, Source::MixedLegacy { collision });
        let archive = store.inspect(CID).unwrap();
        let replayed = history(store.clone()).unwrap();
        let context = replayed.records.import_context(replayed.blocks).unwrap();
        let legacy = context
            .chronology()
            .iter()
            .filter(|event| event.block().content == "mixed original legacy answer")
            .collect::<Vec<_>>();
        assert_eq!(legacy.len(), 1);
        assert!(context.execution_group_is_pending(legacy[0].execution_group_id().unwrap()));
        for expected in &original.context().chronology()[1..] {
            let actual = context
                .chronology()
                .iter()
                .find(|event| event.record().occurrence_id() == expected.record().occurrence_id())
                .unwrap();
            assert_eq!(actual.block(), expected.block());
            assert_eq!(actual.sequence(), expected.sequence());
            assert_eq!(actual.record(), expected.record());
            assert_eq!(actual.execution_group_id(), expected.execution_group_id());
        }
        if collision {
            assert_ne!(
                legacy[0].execution_group_id().unwrap().as_str(),
                "compat-execution-group-2"
            );
        }
        assert_eq!(store.inspect(CID).unwrap(), archive);
    }
}

/// A genuine later admission can append above the preserved reservation and
/// retire the previous instruction scope once, without changing the head or
/// admitting its covered prelude back into model history.
#[test]
fn legacy_prefix_tasks_continue_through_later_accounted_admission() {
    let (_directory, store, _) = fixture(2);
    let migrated = history(store.clone()).unwrap();
    let body = migrated.records.import_context(migrated.blocks).unwrap();
    let mut owner =
        ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
            .unwrap();
    let head = owner.head().unwrap().occurrence_id();
    let start = owner.context().chronology().len();
    owner
        .admit_instruction_task(
            vec![
                ContextBlock::task_prelude(
                    ContextSourceKind::SkillInstruction,
                    "new skill",
                    "later exact prelude",
                ),
                ContextBlock::user_event("user prompt", "later actual task"),
            ],
            &[],
            &[],
            1,
        )
        .unwrap();
    let rows = owner.context().chronology()[start..]
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let block = event.block();
            let semantic =
                if let Some(first) = owner.task_admission_start(event.record().occurrence_id()) {
                    TranscriptContextEvent::task_admission(
                        event.sequence().get(),
                        first,
                        &block.label,
                        &block.content,
                    )
                } else {
                    TranscriptContextEvent::prompt_boundary_with_event_sequence(
                        block.source,
                        event.sequence().get(),
                        &block.label,
                        &block.content,
                    )
                }
                .unwrap();
            TranscriptEntry {
                conversation_id: CID.into(),
                sequence: index as u64 + 8,
                created_at_unix_seconds: 456,
                role: TranscriptRole::System,
                turn_id: "later-actual-task".into(),
                agent_id: "later-agent".into(),
                pane_id: "later-pane".into(),
                content: TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            }
        })
        .collect();
    store
        .commit_accounted_transcript_batch(
            &crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap(),
            2,
        )
        .unwrap();
    for _ in 0..2 {
        let replayed = history(AgentTranscriptStore::new(store.root())).unwrap();
        let replayed = replayed.records.import_context(replayed.blocks).unwrap();
        assert_eq!(replayed.chronology(), owner.context().chronology());
        assert_eq!(replayed.chronology()[0].record().occurrence_id(), head);
    }
}

/// A recorded root cannot borrow a missing or another task's prelude. The
/// existing metadata CAS changes only that source fact, leaving a structurally
/// valid authored row so migration must validate its actual dependency.
#[test]
fn legacy_prefix_tasks_refuse_missing_or_foreign_boundary_backing() {
    for missing in [false, true] {
        let (_directory, store, original) = fixture(2);
        let root = &original.context().chronology()[2];
        let first = if missing {
            mez_agent::ContextBlockOccurrenceId::allocate()
        } else {
            original.context().chronology()[4].record().occurrence_id()
        };
        let semantic = TranscriptContextEvent::task_admission(
            root.sequence().get(),
            first,
            &root.block().label,
            &root.block().content,
        )
        .unwrap();
        let changed = TranscriptContextRecord::new(semantic, root.record(), None)
            .unwrap()
            .to_transcript_content()
            .unwrap();
        let row = store.inspect(CID).unwrap()[2].clone();
        store
            .compare_and_swap_entry_content(
                CID,
                row.sequence,
                &row.pane_id,
                &store.transcript_entry_revision(&row).unwrap(),
                changed,
            )
            .unwrap();
        let path = store.root().join(CID).join("compaction-epoch.json");
        let before = std::fs::read(&path).unwrap();
        let archive = store.inspect(CID).unwrap();
        assert!(history(store.clone()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(store.inspect(CID).unwrap(), archive);
    }
}
