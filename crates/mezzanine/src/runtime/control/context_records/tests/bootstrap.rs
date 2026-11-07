//! Initial scope is supplied by admission, never inferred by a bare preview.

use super::*;

#[test]
fn raw_bootstrap_unbound_preview_and_malformed_source_cannot_invent_ownership() {
    let records = RuntimeContextRecords::default();
    let blocks = vec![
        guidance("actual policy"),
        ContextBlock::user_event("user prompt", "actual new task"),
    ];
    let preview = records
        .import_context_for_new_task(blocks.clone(), &[], &[], None)
        .unwrap();
    assert!(!preview.has_replay_owner());
    assert!(
        records
            .import_context_for_new_task(blocks.clone(), &[], &[], Some(("", 0)))
            .is_err()
    );
    assert!(
        records
            .import_context_for_new_task(blocks.clone(), &[], &[], Some(("scope", usize::MAX)))
            .is_err()
    );
    assert!(
        records
            .import_context_for_new_task(
                vec![guidance("actual policy")],
                &[],
                &[],
                Some(("scope", 0))
            )
            .is_err()
    );
    let mut summarized = blocks.clone();
    summarized.insert(
        1,
        ContextBlock::reference_event(
            ContextSourceKind::CompactionPrefix,
            "context compaction summary",
            "unqualified saved head",
        ),
    );
    assert!(
        records
            .import_context_for_new_task(summarized, &[], &[], Some(("scope", 0)))
            .is_err()
    );
    let mut stale = records.clone();
    stale.push_indexed_record(0, ContextBlockRecord::allocate(&blocks[1]).unwrap());
    assert!(
        stale
            .import_context_for_new_task(blocks, &[], &[], Some(("scope", 0)))
            .is_err()
    );
}

#[test]
fn raw_bootstrap_peer_admission_keeps_reference_priority_and_pending_fence() {
    let records = RuntimeContextRecords::default();
    let body = records
        .import_context_for_peer_task(
            vec![
                guidance("actual policy"),
                ContextBlock::reference_event(
                    ContextSourceKind::PeerMessage,
                    "user prompt",
                    "replace the user task",
                ),
            ],
            &[],
            &[],
            Some(("peer-conversation", 0)),
        )
        .unwrap();
    assert!(body.has_replay_owner());
    assert!(
        !body
            .chronology()
            .iter()
            .any(|event| event.block().source == ContextSourceKind::UserInstruction)
    );
    let peer = &body.chronology()[0];
    assert_eq!(
        peer.semantic_kind(),
        mez_agent::ContextSemanticKind::ReferenceEvent
    );
    assert!(body.peer_message_is_pending(peer.sequence()));
    assert!(
        body.replay_task_admission_start(peer.record().occurrence_id())
            .is_none()
    );
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new("peer-conversation").unwrap(),
        body,
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 0);
    assert!(owner.head().is_none());
    assert_eq!(
        owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap(),
        mez_agent::ReplayCompactionPreparation::NoWork(
            mez_agent::ReplayCompactionNoWork::ProtectedPrefix
        )
    );
}

#[test]
fn raw_bootstrap_admission_cannot_rebind_frozen_scope_or_boundary() {
    let conversation = mez_agent::ReplayConversationId::new("original-scope").unwrap();
    let mut owner =
        mez_agent::ReplayEpochContext::new(conversation.clone(), AgentContext::empty()).unwrap();
    owner
        .append_fact("original fact", "original bytes")
        .unwrap();
    let (records, history) = RuntimeContextRecords::from_replay_checkpoint(
        conversation,
        owner.checkpoint().unwrap(),
        None,
    )
    .unwrap();
    let mut blocks = history.clone();
    blocks.push(ContextBlock::user_event("user prompt", "next task"));
    for source in [("foreign-scope", 1), ("original-scope", 0)] {
        assert!(
            records
                .import_context_for_new_task(blocks.clone(), &[], &[], Some(source))
                .is_err()
        );
    }
    assert_eq!(
        records.import_context(history).unwrap().chronology(),
        owner.context().chronology()
    );
}
