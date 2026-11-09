//! Long tasks compact completed source without retiring their exact obligations.

use super::*;

/// A late append to a retained group can enclose selected work even though the
/// selected group itself never changes. Both direct and prepared adoption must
/// reject the newly merged ownership span rather than split pending source.
#[test]
fn protected_selection_rejects_retained_group_extending_across_selected_work() {
    for prepared in [false, true] {
        let mut source = owner();
        let pending = ContextExecutionGroupId::new("retained spanning group").unwrap();
        source
            .append_execution_events([ContextConversationAppend::assistant(
                "pending",
                "original unfinished work",
                pending.clone(),
            )])
            .unwrap();
        source
            .append_fact("closed", "independent completed source")
            .unwrap();
        let frozen = plan(&source);
        let candidate = source
            .prepare_compaction_publication(
                frozen.clone(),
                ReplayHeadPublication::Summary("private head".into()),
            )
            .unwrap();
        source
            .append_execution_events([ContextConversationAppend::assistant(
                "late",
                "late unfinished source encloses selected work",
                pending,
            )])
            .unwrap();
        let before = source.clone();
        let result = if prepared {
            source.adopt_compaction_publication(&candidate)
        } else {
            source.publish_compaction(frozen, ReplayHeadPublication::Summary("must refuse".into()))
        };
        assert!(
            result.is_err(),
            "new ownership span must invalidate frozen selection"
        );
        assert_eq!(source, before);
    }
}

#[test]
fn protected_selection_compacts_completed_work_across_exact_instructions() {
    let mut source = owner();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(&task, ContextBlock::user_event("task", "exact active root"))
        .unwrap();
    let root = source.context().chronology()[0].clone();
    let first = ContextExecutionGroupId::new("first completed execution").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "first",
            "completed first evidence",
            first.clone(),
        )])
        .unwrap();
    finish_group(&mut source, &first).unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("steering", "exact steering"),
        )
        .unwrap();
    let steering = source.context().chronology()[2].clone();
    let second = ContextExecutionGroupId::new("second completed execution").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "second",
            "completed second evidence",
            second.clone(),
        )])
        .unwrap();
    finish_group(&mut source, &second).unwrap();
    let pending = ContextExecutionGroupId::new("unfinished execution").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "pending",
            "retain unfinished work",
            pending.clone(),
        )])
        .unwrap();
    let unfinished = source.context().chronology()[4].clone();
    let frozen = plan(&source);
    assert_eq!(frozen.summary_input().len(), 2);
    assert_eq!(frozen.removed_raw_members().count(), 2);
    assert_eq!(frozen.retained_raw_events().count(), 3);
    let prepared = source
        .prepare_compaction_publication(frozen, ReplayHeadPublication::Summary("first head".into()))
        .unwrap();
    let late = source.append_fact("late", "post-freeze arrival").unwrap();
    source.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(source.context().chronology()[1], root);
    assert_eq!(source.context().chronology()[2], steering);
    assert_eq!(source.context().chronology()[3], unfinished);
    assert_eq!(source.context().chronology()[4].sequence(), late);
    assert!(source.context().execution_group_is_pending(&pending));
    assert!(
        source
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_000, 0).unwrap())
            .unwrap()
    );
    let adopted = source.clone();
    source.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(source, adopted);
    let frozen = plan(&source);
    source
        .publish_compaction(frozen, ReplayHeadPublication::Summary("second head".into()))
        .unwrap();
    assert!(
        !source
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_000, 0).unwrap())
            .unwrap()
    );
    let mut restored = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        source.checkpoint().unwrap(),
        owner().context().clone(),
    )
    .unwrap();
    assert_eq!(
        restored.context().chronology(),
        source.context().chronology()
    );
    assert!(
        !restored
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_000, 0).unwrap())
            .unwrap()
    );
    finish_group(&mut restored, &pending).unwrap();
    assert!(
        restored
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_000, 0).unwrap())
            .unwrap()
    );
    assert_eq!(plan(&restored).removed_raw_members().count(), 1);
}

#[test]
fn protected_selection_rejects_reopened_or_extended_selected_groups_atomically() {
    for extend in [false, true] {
        let mut source = owner();
        let task = source.begin_instruction_task().unwrap();
        source
            .append_instruction(&task, ContextBlock::user_event("task", "exact root"))
            .unwrap();
        let group = ContextExecutionGroupId::new("selected execution").unwrap();
        source
            .append_execution_events([ContextConversationAppend::assistant(
                "first",
                "completed source",
                group.clone(),
            )])
            .unwrap();
        finish_group(&mut source, &group).unwrap();
        let frozen = plan(&source);
        if extend {
            source
                .append_execution_events([ContextConversationAppend::assistant(
                    "late",
                    "new source in selected group",
                    group.clone(),
                )])
                .unwrap();
        } else {
            let conversation = source.conversation_id().clone();
            let mut body = source.into_owned_context();
            body.retain_pending_execution_group(&group).unwrap();
            source = ReplayEpochContext::from_owned_context(conversation, body).unwrap();
        }
        let before = source.clone();
        assert!(
            source
                .publish_compaction(frozen, ReplayHeadPublication::Summary("must refuse".into()))
                .is_err()
        );
        assert_eq!(source, before);
    }
}
