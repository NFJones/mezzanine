//! Explicit prefix conversion preserves source and grants no controller rights.

use super::*;
use crate::ContextBlockRecord;

/// Uses an actual prefix-kind event and its original record, independently of
/// its historical Memory-shaped label. The suffix is current raw source only.
fn legacy() -> (AgentContext, ContextBlockRecord) {
    let context = AgentContext::new(vec![
        ContextBlock::stable_instruction(ContextSourceKind::System, "policy", "current policy"),
        ContextBlock::reference_event(
            ContextSourceKind::CompactionPrefix,
            "memory old-prefix-uuid (conversation)",
            "original model summary",
        ),
    ])
    .unwrap();
    let head = context.chronology()[0].record();
    (context, head)
}

/// Exact source and accounting survive import and checkpoint reopen. Unknown
/// groups, instructions and peer messages retain protection rather than gaining
/// completion/ACK evidence from their presence in a saved transcript.
#[test]
fn prefix_import_preserves_head_tail_and_conservative_lifecycle() {
    let (mut context, head) = legacy();
    let group = ContextExecutionGroupId::new("legacy-group").unwrap();
    context
        .append_assistant_event("answer", "retained native-neutral answer", group.clone())
        .unwrap();
    context
        .append_user_event("prompt", "keep this instruction exact")
        .unwrap();
    let peer = context
        .append_conversation_events([ContextConversationAppend::peer_message(
            "peer",
            "original peer evidence",
        )])
        .unwrap()[0];
    let events = context.chronology().to_vec();
    let owner = ReplayEpochContext::from_legacy_prefix(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
        head,
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 1);
    assert_eq!(owner.head().unwrap().occurrence_id(), head.occurrence_id());
    assert_eq!(owner.context().chronology(), events);
    assert_eq!(owner.members().len(), events.len() - 1);
    for (member, event) in owner.members().iter().zip(&events[1..]) {
        assert_eq!(member.occurrence_id(), event.record().occurrence_id());
        assert_eq!(member.origin(), event.sequence());
        assert_eq!(member.born_in().get(), 0);
    }
    assert!(owner.context().execution_group_is_pending(&group));
    assert!(owner.execution_states().unwrap().is_empty());
    assert!(owner.context().peer_message_is_pending(peer));
    let cold = ReplayEpochContext::from_checkpoint(
        owner.conversation_id().clone(),
        owner.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(cold.context().chronology(), events);
    assert!(cold.context().execution_group_is_pending(&group));
    assert!(cold.execution_states().unwrap().is_empty());
    assert!(cold.context().peer_message_is_pending(peer));
    assert!(cold.instruction_owners.is_empty());
}

/// Recorded admissions retain their instruction lifecycle before the old head
/// is attached; flattening these tasks would place a prelude after an active
/// prompt. The head's new anchor cannot change any original raw reservation.
#[test]
fn prefix_import_attaches_head_to_restored_task_source() {
    let (header, head) = legacy();
    let block = header.chronology()[0].block().clone();
    let conversation = ReplayConversationId::new("conversation-a").unwrap();
    let mut source = ReplayEpochContext::new(conversation.clone(), AgentContext::empty()).unwrap();
    for task in 1..=2 {
        source
            .restore_instruction_task(
                vec![
                    ContextBlock::task_prelude(
                        ContextSourceKind::SkillInstruction,
                        "skill",
                        format!("original skill {task}"),
                    ),
                    ContextBlock::user_event("user prompt", format!("original task {task}")),
                ],
                &[],
                &[],
                1,
                &[],
            )
            .unwrap_err();
        // Actual admissions supply records before the descriptive restore port
        // can be used; this fixture owns those admissions directly.
        source
            .admit_instruction_task(
                vec![
                    ContextBlock::task_prelude(
                        ContextSourceKind::SkillInstruction,
                        "skill",
                        format!("original skill {task}"),
                    ),
                    ContextBlock::user_event("user prompt", format!("original task {task}")),
                ],
                &[],
                &[],
                1,
            )
            .unwrap();
    }
    let group = ContextExecutionGroupId::new("original-source-group").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "original completed answer",
            group.clone(),
        )])
        .unwrap()[0];
    let completion = source.capture_execution_owner(&group, frontier).unwrap();
    source.finish_execution_group(&completion).unwrap();
    let expected = source.context().chronology().to_vec();
    let explicit = expected
        .iter()
        .filter(|event| event.execution_group_id().is_some())
        .map(|event| {
            crate::ImportedExecutionEvent::new(
                event.block().clone(),
                event.execution_group_id().unwrap().clone(),
                1,
                event.provider_owner().cloned(),
            )
            .unwrap()
            .with_record(event.record())
            .unwrap()
        })
        .collect::<Vec<_>>();
    let ids = explicit
        .iter()
        .map(|event| event.record().unwrap().occurrence_id())
        .collect();
    let imported = source
        .import_legacy_prefix_head(block, head, &explicit)
        .unwrap();
    assert_eq!(&imported.context().chronology()[1..], expected);
    assert!(imported.context().execution_group_is_pending(&group));
    assert!(!imported.execution_group_finished(&completion));
    assert!(imported.execution_states().unwrap().is_empty());
    let before = imported.context().clone();
    let framing = imported.context().legacy_import_framing(&ids).unwrap();
    assert_eq!(imported.context(), &before);
    for (metadata, event) in framing.iter().zip(imported.context().chronology()) {
        assert_eq!(metadata.record(), event.record());
        assert_eq!(metadata.retention(), event.retention());
    }
    assert!(imported.context().chronology()[0].sequence() < expected[0].sequence());
    assert!(
        imported
            .members()
            .iter()
            .all(|member| member.born_in().get() == 0)
    );
    let cold = ReplayEpochContext::from_checkpoint(
        conversation,
        imported.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(&cold.context().chronology()[1..], expected);
    assert!(cold.execution_states().unwrap().is_empty());
}

/// A legitimately head-only prefix needs no invented raw occurrence. Later
/// arrivals allocate above the original reservation and the next rewrite uses
/// that head once, never the historical source it already replaced.
#[test]
fn prefix_import_head_only_continues_through_real_publication() {
    let (context, record) = legacy();
    let first_sequence = context.chronology()[0].sequence();
    let mut imported = ReplayEpochContext::from_legacy_prefix(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
        record,
    )
    .unwrap();
    assert!(imported.members().is_empty());
    let arrival = imported.append_fact("new", "later actual source").unwrap();
    assert!(arrival > first_sequence);
    let selected = plan(&imported);
    assert_eq!(selected.summary_input().len(), 2);
    assert_eq!(selected.summary_input()[0].record(), record);
    imported
        .publish_compaction(
            selected,
            ReplayHeadPublication::Summary("new accepted head".into()),
        )
        .unwrap();
    assert_eq!(imported.revision().get(), 2);
    assert_ne!(
        imported.head().unwrap().occurrence_id(),
        record.occurrence_id()
    );
    assert_eq!(imported.context().chronology().len(), 1);
    assert!(imported.members().is_empty());
}

/// Ordinary labels, selective/multiple summaries and changed expected records
/// cannot be promoted into a typed head by the migration adapter.
#[test]
fn prefix_import_rejects_ambiguous_or_unqualified_head() {
    let (original, record) = legacy();
    let unrelated =
        ContextBlockRecord::allocate(&ContextBlock::assistant_event("other", "other")).unwrap();
    assert!(
        ReplayEpochContext::from_legacy_prefix(
            ReplayConversationId::new("conversation-a").unwrap(),
            original.clone(),
            unrelated,
        )
        .is_err()
    );
    for source in [
        ContextSourceKind::Memory,
        ContextSourceKind::CompactionSelective,
    ] {
        let context = AgentContext::new(vec![ContextBlock::reference_event(
            source,
            "context compaction summary",
            "looks like a summary",
        )])
        .unwrap();
        let record = context.chronology()[0].record();
        assert!(
            ReplayEpochContext::from_legacy_prefix(
                ReplayConversationId::new("conversation-a").unwrap(),
                context,
                record,
            )
            .is_err()
        );
    }
    for source in [
        ContextSourceKind::CompactionPrefix,
        ContextSourceKind::CompactionSelective,
    ] {
        let mut multiple = original.clone();
        multiple
            .append_reference_event(source, "second summary", "distinct summary")
            .unwrap();
        assert!(
            ReplayEpochContext::from_legacy_prefix(
                ReplayConversationId::new("conversation-a").unwrap(),
                multiple,
                record,
            )
            .is_err()
        );
    }
    let mut malformed = original.clone();
    malformed.chronology[0].retention = ContextRetention::Exact;
    assert!(
        ReplayEpochContext::from_legacy_prefix(
            ReplayConversationId::new("conversation-a").unwrap(),
            malformed,
            record,
        )
        .is_err()
    );
}
