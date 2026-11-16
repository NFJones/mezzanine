//! Covered closure evidence is bounded by existing original fence allocations,
//! not by a historical execution cache or descriptive checkpoint metadata.

use super::*;

fn closed() -> (
    ReplayEpochContext,
    ContextExecutionGroupId,
    ReplayExecutionOwner,
) {
    let mut live = owner();
    let group = ContextExecutionGroupId::new("completed-source").unwrap();
    let frontier = live
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "completed source",
            group.clone(),
        )])
        .unwrap()[0];
    let receipt = live.capture_execution_owner(&group, frontier).unwrap();
    live.finish_execution_group(&receipt).unwrap();
    (live, group, receipt)
}

fn compact(live: &mut ReplayEpochContext) {
    live.append_fact("next source", "new compactable evidence")
        .unwrap();
    let prepared = live
        .prepare_compaction_publication(
            plan(live),
            ReplayHeadPublication::Summary("accepted head".into()),
        )
        .unwrap();
    live.adopt_compaction_publication(&prepared).unwrap();
    live.adopt_compaction_publication(&prepared).unwrap();
}

#[test]
fn covered_completion_verifies_across_publications_and_releases_its_weak_proof() {
    let (mut live, group, receipt) = closed();
    for _ in 0..2 {
        compact(&mut live);
        assert!(live.execution_group_finished(&receipt));
        assert!(live.group_frontier(&group).is_none());
        assert!(!live.context().execution_group_is_pending(&group));
        assert!(live.execution_states().unwrap().is_empty());
    }
    let mut body = live.into_owned_context();
    body.update_replay_owner(|live| {
        assert!(live.execution_group_finished(&receipt));
        Ok(())
    })
    .unwrap();
    drop(receipt);
    body.update_replay_owner(|live| {
        assert!(live.source_lifecycle.closed_groups.is_empty());
        Ok(())
    })
    .unwrap();
}

#[test]
fn covered_completion_cannot_verify_a_recreated_owner_or_reopened_source() {
    let (mut live, group, receipt) = closed();
    compact(&mut live);
    let cold = ReplayEpochContext::from_checkpoint(
        live.conversation_id().clone(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!cold.execution_group_finished(&receipt));
    assert!(!owner().execution_group_finished(&receipt));
    let frontier = live
        .append_execution_events([ContextConversationAppend::assistant(
            "new assistant",
            "new same-group source",
            group.clone(),
        )])
        .unwrap()[0];
    assert!(!live.execution_group_finished(&receipt));
    assert!(live.finish_execution_group(&receipt).is_err());
    let renewed = live.capture_execution_owner(&group, frontier).unwrap();
    live.finish_execution_group(&renewed).unwrap();
    compact(&mut live);
    assert!(live.execution_group_finished(&renewed));
    assert!(!live.execution_group_finished(&receipt));
}

/// A pre-retirement snapshot holds the original allocation even after the
/// callback's receipt drops. Weak closure metadata must not keep that snapshot
/// alive, and disappears on reacquisition after the snapshot is also dropped.
#[test]
fn covered_completion_snapshot_lifetime_does_not_create_a_strong_cycle() {
    let mut live = owner();
    let group = ContextExecutionGroupId::new("snapshot-source").unwrap();
    let frontier = live
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "completed source",
            group.clone(),
        )])
        .unwrap()[0];
    let receipt = live.capture_execution_owner(&group, frontier).unwrap();
    let pending_snapshot = live.clone();
    live.finish_execution_group(&receipt).unwrap();
    drop(receipt);
    compact(&mut live);
    assert!(live.source_lifecycle.closed_groups.contains_key(&group));
    assert!(
        pending_snapshot
            .context()
            .execution_group_is_pending(&group)
    );
    drop(pending_snapshot);
    let body = live.into_owned_context();
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    assert!(live.source_lifecycle.closed_groups.is_empty());
}

#[test]
fn failed_compaction_never_manufactures_or_removes_completion_evidence() {
    let (mut live, group, receipt) = closed();
    let before = live.clone();
    assert!(
        live.publish_compaction(plan(&live), ReplayHeadPublication::Summary(String::new()),)
            .is_err()
    );
    assert_eq!(live, before);
    assert!(live.execution_group_finished(&receipt));
    assert_eq!(live.group_frontier(&group), Some(receipt.frontier()));
    drop(before);
    drop(receipt);
    compact(&mut live);
    assert!(live.source_lifecycle.closed_groups.is_empty());
}
