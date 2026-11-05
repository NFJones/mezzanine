//! Exact committed-prefix qualification and nonserializable lifecycle proofs.
//! All transitions use real owner methods, without provider or tool execution.

use super::*;
use serde_json::json;

/// Creates one published head and one inherited older-born raw occurrence.
fn parent() -> ReplayEpochContext {
    let mut owner = owner();
    owner
        .append_fact("covered", "covered original source")
        .unwrap();
    let frozen = plan(&owner);
    owner.append_fact("retained", "original raw bytes").unwrap();
    owner
        .publish_compaction(
            frozen,
            ReplayHeadPublication::Summary("original head".into()),
        )
        .unwrap();
    owner
}

/// Genuine arrivals retain the exact parent prefix and may use refreshed current
/// instructions/provider views. Neither refresh rewrites historical source.
#[test]
fn source_extension_preserves_committed_prefix_and_allows_live_refresh() {
    let parent = parent();
    let mut extended = parent.clone();
    extended
        .append_fact("arrival", "new original bytes")
        .unwrap();
    let current = AgentContext::new(vec![ContextBlock::stable_instruction(
        ContextSourceKind::System,
        "policy",
        "refreshed current policy",
    )])
    .unwrap();
    extended
        .replace_stable_slots(current.stable_slots().to_vec())
        .unwrap();
    extended
        .select_provider_projection(
            ReplayProviderProjection::new(
                crate::ProviderApiCompatibility::OpenAiResponses,
                "current-provider",
                "current-model",
            )
            .unwrap(),
        )
        .unwrap();
    extended.validate_source_extension(&parent).unwrap();
    assert_eq!(extended.members()[0], parent.members()[0]);
    assert_eq!(
        extended.context().chronology()[..2],
        parent.context().chronology()[..]
    );
}

/// New source cannot reuse a covered ordering reservation or claim an earlier
/// birth revision even when its standalone canonical checkpoint is valid.
#[test]
fn source_extension_rejects_reused_reservations_and_forged_arrival_births() {
    let parent = parent();
    let mut extended = parent.clone();
    extended.append_fact("arrival", "new source").unwrap();
    for alteration in 0..2 {
        let mut value = serde_json::to_value(extended.checkpoint().unwrap()).unwrap();
        if alteration == 0 {
            value["events"][2]["sequence"] = json!(2560);
            value["members"][1]["origin"] = json!(2560);
        } else {
            value["members"][1]["born_in"] = json!(0);
        }
        let altered = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            serde_json::from_value(value).unwrap(),
            owner().context().clone(),
        )
        .unwrap();
        assert!(altered.validate_source_extension(&parent).is_err());
    }
}

/// Real task completion, local peer handoff and exact group closure each permit
/// a source transition. Deserializing the same resulting state cannot mint those
/// transition rights against an older pending parent. Once committed, restored
/// identical state remains a valid parent extension. Covered proofs are pruned.
#[test]
fn source_lifecycle_transitions_require_actual_controller_witnesses() {
    let mut pending = owner();
    let task = pending.begin_instruction_task().unwrap();
    pending
        .append_instruction(
            &task,
            ContextBlock::user_event("user prompt", "active original instruction"),
        )
        .unwrap();
    let peer = pending
        .append_pending_peer("peer", "untrusted original reference")
        .unwrap();
    let group = ContextExecutionGroupId::new("pending-group").unwrap();
    pending
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "actual completed output",
            group.clone(),
        )])
        .unwrap();
    for transition in 0..3 {
        let mut current = pending.clone();
        match transition {
            0 => current.complete_instruction_task(&task).unwrap(),
            1 => assert!(current.release_peer(peer).unwrap()),
            2 => finish_group(&mut current, &group).unwrap(),
            _ => unreachable!(),
        }
        current.validate_source_extension(&pending).unwrap();
        let restored = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            current.checkpoint().unwrap(),
            owner().context().clone(),
        )
        .unwrap();
        assert!(restored.validate_source_extension(&pending).is_err());
        restored.validate_source_extension(&current).unwrap();
    }
    pending.complete_instruction_task(&task).unwrap();
    pending.release_peer(peer).unwrap();
    finish_group(&mut pending, &group).unwrap();
    pending
        .publish_compaction(
            plan(&pending),
            ReplayHeadPublication::Summary("completed source head".into()),
        )
        .unwrap();
    assert!(pending.source_lifecycle.completed_instructions.is_empty());
    assert!(pending.source_lifecycle.handed_off_peers.is_empty());
    assert!(pending.source_lifecycle.closed_groups.is_empty());
}
