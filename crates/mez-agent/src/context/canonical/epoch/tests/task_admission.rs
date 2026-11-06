//! Live and qualified recorded task boundaries share exact lifecycle semantics.

use super::*;

#[test]
fn recorded_task_admission_preserves_original_source_and_native_protection() {
    let mut body = owner().into_owned_context();
    body.append_user_event("user prompt", "original task")
        .unwrap();
    let mut base = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    let group = ContextExecutionGroupId::new("still-pending-execution").unwrap();
    base.append_execution_events([ContextConversationAppend::assistant(
        "assistant",
        "actual pending execution",
        group.clone(),
    )])
    .unwrap();
    let suffix = vec![
        ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "new skill",
            "exact skill",
        ),
        ContextBlock::user_event("user prompt", "new task"),
    ];
    let start = base.context().chronology().len();
    let mut live = base.clone();
    live.admit_instruction_task(suffix.clone(), &[], &[], 1)
        .unwrap();
    live.validate_source_extension(&base).unwrap();
    let events = &live.context().chronology()[start..];
    let records = events
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.record()))
        .collect::<Vec<_>>();
    let sequences = events
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.sequence()))
        .collect::<Vec<_>>();
    let mut restored = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        base.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    restored
        .restore_instruction_task(suffix, &records, &[], 1, &sequences)
        .unwrap();
    restored.validate_source_extension(&base).unwrap();
    // A checked cold source is recoverable for publication; ordinary live
    // admission alone does not yet establish that physical backing.
    for (live, restored) in live
        .context()
        .chronology()
        .iter()
        .zip(restored.context().chronology())
    {
        assert_eq!(live.block(), restored.block());
        assert_eq!(live.record(), restored.record());
        assert_eq!(live.sequence(), restored.sequence());
        assert_eq!(live.retention(), restored.retention());
        assert_eq!(live.execution_group_id(), restored.execution_group_id());
        assert_eq!(live.provider_owner(), restored.provider_owner());
    }
    assert_eq!(
        restored.context().chronology()[0].retention(),
        ContextRetention::Summarizable
    );
    assert_eq!(
        restored.context().chronology()[start].retention(),
        ContextRetention::Exact
    );
    assert!(restored.context().execution_group_is_pending(&group));
    let root = events[1].record().occurrence_id();
    let first = events[0].record().occurrence_id();
    assert_eq!(restored.task_admission_start(root), Some(first));
    let moved = restored.clone().into_owned_context();
    assert_eq!(moved.replay_task_admission_start(root), Some(first));
    let reacquired = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        moved,
    )
    .unwrap();
    reacquired.validate_source_extension(&base).unwrap();
    let descriptive = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        restored.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(descriptive.task_admission_start(root), None);
    assert!(descriptive.validate_source_extension(&base).is_err());
}

#[test]
fn recorded_task_admission_rejects_invalid_source_atomically_and_peer_cannot_retire() {
    let mut body = owner().into_owned_context();
    body.append_user_event("user prompt", "original task")
        .unwrap();
    let mut base = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    let before = base.clone();
    assert!(
        base.restore_instruction_task(
            vec![ContextBlock::user_event("user prompt", "replacement")],
            &[],
            &[],
            0,
            &[],
        )
        .is_err()
    );
    assert_eq!(base, before);
    assert!(
        base.admit_instruction_task(
            vec![ContextBlock::reference_event(
                ContextSourceKind::PeerMessage,
                "user prompt",
                "replacement"
            )],
            &[],
            &[],
            0,
        )
        .is_err()
    );
    assert_eq!(base, before);
    base.append_pending_peer("user prompt", "replacement")
        .unwrap();
    assert_eq!(
        base.context().chronology()[0].retention(),
        ContextRetention::Exact
    );
    let mut body = base.clone().into_owned_context();
    assert!(
        body.append_user_event("user prompt", "replacement")
            .is_err()
    );
    assert_eq!(body.chronology()[0].retention(), ContextRetention::Exact);
}
