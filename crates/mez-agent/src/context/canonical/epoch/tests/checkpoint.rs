//! Serialized checkpoint adoption through the actual canonical epoch owner.
//! Fixtures never dispatch providers, execute native tools or send peer ACKs.

use super::*;
use serde_json::{Value, json};

/// Uses a new stable-only owner as the live caller, then restores serialized
/// source. This exercises serde as well as the production adoption boundary.
fn restore(source: &ReplayEpochContext) -> ReplayEpochContext {
    let bytes = serde_json::to_vec(&source.checkpoint().unwrap()).unwrap();
    ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        serde_json::from_slice(&bytes).unwrap(),
        owner().context().clone(),
    )
    .unwrap()
}

/// Deserialization alone is insufficient: invalid structurally decoded source
/// must be refused by the typed owner before any state can be adopted.
fn assert_rejected(value: Value) {
    let checkpoint = serde_json::from_value::<ReplayEpochCheckpoint>(value.clone()).unwrap();
    assert!(
        ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            checkpoint,
            owner().context().clone(),
        )
        .is_err(),
        "unexpectedly accepted checkpoint: {value}"
    );
}

/// Cold restoration after each of three publications preserves the sole head,
/// inherited tail, original IDs/costs and monotonic arrivals. Covered ancestors
/// stay absent from the next plan instead of being expanded from audit history.
#[test]
fn checkpoint_round_trip_preserves_three_epochs_and_arrivals() {
    let mut current = owner();
    current.append_fact("covered", "covered sentinel").unwrap();
    for revision in 1..=3 {
        let frozen = plan(&current);
        let arrival = current.append_fact("tail", "equal arrival bytes").unwrap();
        current
            .publish_compaction(
                frozen,
                ReplayHeadPublication::Summary(format!("head {revision}")),
            )
            .unwrap();
        let mut restored = restore(&current);
        assert_eq!(restored.context(), current.context());
        assert_eq!(restored.head(), current.head());
        assert_eq!(restored.members(), current.members());
        assert_eq!(restored.input_estimates(), current.input_estimates());
        assert_eq!(restored.members()[0].origin(), arrival);
        assert_eq!(restored.members()[0].born_in().get(), revision - 1);
        let next = restored.append_fact("new", "new source").unwrap();
        assert!(next > arrival);
        assert!(
            plan(&restored)
                .summary_input()
                .iter()
                .all(|event| !event.block().content.contains("covered sentinel"))
        );
        current = restored;
    }
}

/// Current trusted policy and request identity win over saved metadata, while
/// the same producer's stable occurrence survives a changed instruction body.
/// No saved instruction bytes or session/cache IDs are serialized.
#[test]
fn checkpoint_uses_current_instructions_and_metadata() {
    let mut source = owner();
    source.append_fact("fact", "historical source").unwrap();
    source
        .select_provider_projection(
            ReplayProviderProjection::new(
                crate::ProviderApiCompatibility::OpenAiResponses,
                "saved-provider",
                "saved-model",
            )
            .unwrap(),
        )
        .unwrap();
    let checkpoint = source.checkpoint().unwrap();
    let json = serde_json::to_string(&checkpoint).unwrap();
    for private in [
        "exact controlling policy",
        "actual-session",
        "stable-cache-lineage",
        "saved-provider",
        "saved-model",
    ] {
        assert!(!json.contains(private));
    }
    let prefix = AgentContext::new(vec![ContextBlock::stable_instruction(
        ContextSourceKind::System,
        "policy",
        "current trusted policy",
    )])
    .unwrap()
    .with_metadata(crate::ModelContextMetadata::new(
        Some("current-session"),
        Some("current-cache"),
    ));
    let expected_metadata = prefix.metadata().clone();
    let restored = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        checkpoint,
        prefix,
    )
    .unwrap();
    assert_eq!(restored.context().metadata(), &expected_metadata);
    assert_eq!(
        restored.context().stable_slots()[0].block().content,
        "current trusted policy"
    );
    assert_eq!(
        restored.context().stable_slots()[0]
            .record()
            .occurrence_id(),
        source.context().stable_slots()[0].record().occurrence_id()
    );
    assert!(restored.provider_projection().is_none());
    assert_eq!(
        restored.context().chronology(),
        source.context().chronology()
    );
    assert_eq!(
        restored.input_estimates().stable_input_tokens(),
        restored.context().stable_slots()[0]
            .record()
            .estimated_input_tokens()
    );
    let removed = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        source.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(removed.context().stable_slots().is_empty());
    assert_eq!(
        removed.context().chronology(),
        source.context().chronology()
    );
}

/// Historical instruction completion survives explicit typed restoration.
/// Active handles and frozen plans from the old incarnation cannot act on the
/// restored owner, even with equal conversation spelling and event positions.
#[test]
fn checkpoint_restores_lifecycle_without_resurrecting_handles() {
    let mut source = owner();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("user prompt", "completed instruction"),
        )
        .unwrap();
    source.complete_instruction_task(&task).unwrap();
    let frozen = plan(&source);
    let active = source.begin_instruction_task().unwrap();
    let sequence = source
        .append_instruction(
            &active,
            ContextBlock::user_event("user prompt", "current instruction"),
        )
        .unwrap();
    let mut restored = restore(&source);
    assert_eq!(restored.context(), source.context());
    let unchanged = restored.clone();
    assert!(restored.complete_instruction_task(&active).is_err());
    assert!(
        restored
            .publish_compaction(
                frozen,
                ReplayHeadPublication::Summary("stale summary".into())
            )
            .is_err()
    );
    assert_eq!(restored, unchanged);
    let fresh = restored.begin_instruction_task().unwrap();
    restored
        .bind_instruction_occurrences(&fresh, &[sequence])
        .unwrap();
    restored.complete_instruction_task(&fresh).unwrap();
    assert_eq!(plan(&restored).summary_input().len(), 2);
}

/// Native declarations remain protected across restart, and the old closure
/// fence is invalid. Removing protection from an unanswered declaration is
/// rejected. A real matching result permits closure and remains exact source.
#[test]
fn checkpoint_preserves_native_source_and_requires_actual_closure() {
    use crate::{ProviderApiCompatibility, ProviderContinuityOwner, ProviderTranscriptEvent};
    let mut source = owner();
    let group = ContextExecutionGroupId::new("pending-native").unwrap();
    let provider = ProviderContinuityOwner::new(
        ProviderApiCompatibility::OpenAiChatCompletions,
        "offline-provider",
    )
    .unwrap();
    let call = ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
        "offline-provider".into(), "declaration".into(),
        vec![json!({"id":"original-call", "type":"function", "function":{"name":crate::MAAP_ACTION_BATCH_TOOL_NAME, "arguments":"{}"}})],
    ).unwrap();
    let ids = source
        .append_execution_events([ContextConversationAppend::provider_assistant(
            "call",
            call.to_transcript_content(),
            group.clone(),
            provider.clone(),
        )])
        .unwrap();
    let old_fence = source.capture_execution_owner(&group, ids[0]).unwrap();
    let mut restored = restore(&source);
    assert_eq!(restored.context(), source.context());
    assert!(restored.finish_execution_group(&old_fence).is_err());
    assert_eq!(
        restored
            .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap(),
        ReplayCompactionPreparation::NoWork(ReplayCompactionNoWork::ProtectedPrefix)
    );
    let mut forged = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    forged["pending_execution_groups"] = json!([]);
    assert_rejected(forged);
    let result = ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
        provider_id: "offline-provider".into(),
        tool_call_id: "original-call".into(),
        content: "original output".into(),
    };
    restored
        .append_execution_events([ContextConversationAppend::evidence(
            ContextSourceKind::TranscriptTool,
            "result",
            result.to_transcript_content(),
            group.clone(),
            Some(provider),
            true,
        )])
        .unwrap();
    finish_group(&mut restored, &group).unwrap();
    let reopened = restore(&restored);
    assert_eq!(reopened.context(), restored.context());
    assert_eq!(reopened.input_estimates(), restored.input_estimates());
    assert_eq!(
        plan(&reopened).summary_input(),
        restored.context().chronology()
    );
}

/// Pending peer source keeps its exact identity and protection, independently
/// of transport ACKs. Releasing that local protection survives another restart.
#[test]
fn checkpoint_preserves_peer_protection_without_transport_rights() {
    let mut source = owner();
    let peer = source
        .append_pending_peer("peer", "untrusted original bytes")
        .unwrap();
    let mut restored = restore(&source);
    assert!(restored.context().peer_message_is_pending(peer));
    assert_eq!(
        restored.context().chronology(),
        source.context().chronology()
    );
    assert!(restored.release_peer(peer).unwrap());
    let released = restore(&restored);
    assert!(!released.context().peer_message_is_pending(peer));
    assert_eq!(plan(&released).summary_input().len(), 1);
}

/// Typed restoration rejects missing/duplicate/foreign membership, malformed
/// head policy, sequence high-water and lifecycle witnesses, and unsupported
/// record formats. Unknown fields fail structural decoding as well.
#[test]
fn checkpoint_rejects_corrupt_membership_and_lifecycle() {
    let mut source = owner();
    source.append_fact("source", "covered").unwrap();
    source
        .publish_compaction(
            plan(&source),
            ReplayHeadPublication::Summary("valid head".into()),
        )
        .unwrap();
    source.append_fact("tail", "retained raw").unwrap();
    let value = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    let cases = [
        ("/version", json!(99)),
        ("/conversation", json!("foreign")),
        ("/revision", json!(0)),
        ("/policy", json!("intentional_omission")),
        ("/head", Value::Null),
        ("/events/0/block/content", json!("")),
        ("/events/0/retention", json!("exact")),
        ("/events/1/sequence", json!(0)),
        ("/events/1/block/placement", json!("stable_prefix")),
        ("/events/1/record/format_version", json!(99)),
        ("/members", json!([])),
        ("/members/0/born_in", json!(2)),
        ("/members/0/origin", json!(1)),
        ("/next_event_sequence", json!(0)),
        ("/historical_instruction_events", json!([1024])),
        ("/pending_peer_messages", json!([2048, 2048])),
        ("/pending_execution_groups", json!(["absent"])),
    ];
    for (path, replacement) in cases {
        let mut corrupt = value.clone();
        *corrupt.pointer_mut(path).unwrap() = replacement;
        assert_rejected(corrupt);
    }
    let mut duplicate = value.clone();
    duplicate["members"][0]["occurrence_id"] = duplicate["head"].clone();
    assert_rejected(duplicate);
    let mut unknown = value;
    unknown["unknown_authority"] = json!(true);
    assert!(serde_json::from_value::<ReplayEpochCheckpoint>(unknown).is_err());
}

/// Cached token counts are descriptive. Authored or stale estimates are repaired
/// against the supplied exact source without replacing original occurrence IDs.
#[test]
fn checkpoint_repairs_stale_estimates_and_rejects_prefix_history() {
    let mut source = owner();
    source.append_fact("source", "actual exact bytes").unwrap();
    let mut value = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    value["events"][0]["record"]["framed_input_tokens"] = json!(usize::MAX);
    let restored = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        serde_json::from_value(value).unwrap(),
        owner().context().clone(),
    )
    .unwrap();
    assert_eq!(restored.context(), source.context());
    assert_eq!(restored.input_estimates(), source.input_estimates());
    let checkpoint = source.checkpoint().unwrap();
    assert!(
        ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            checkpoint,
            source.context().clone(),
        )
        .is_err()
    );
}

/// Empty initial and explicit omission epochs retain their sequence reservation
/// across restart. A new arrival never reuses a covered ordering slot or ID.
#[test]
fn checkpoint_round_trips_empty_and_explicit_omission_epochs() {
    let initial = owner();
    assert_eq!(restore(&initial).context(), initial.context());
    let mut omission = ReplayEpochContext::new_for_head_policy(
        ReplayConversationId::new("conversation-a").unwrap(),
        initial.context().clone(),
        ReplayHeadPolicy::IntentionalOmission,
    )
    .unwrap();
    let covered = omission.append_fact("covered", "original source").unwrap();
    omission
        .publish_compaction(plan(&omission), ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    let mut restored = restore(&omission);
    assert!(restored.head().is_none());
    assert!(restored.members().is_empty());
    assert_eq!(restored.revision().get(), 1);
    assert!(restored.append_fact("new", "post-restart arrival").unwrap() > covered);
}

/// Storage may externalize only payload bytes. A serialized reference roundtrip
/// preserves every original ID, framing, lifecycle and accounting field while
/// equal text remains two distinct occurrences. Callback errors stop extraction.
#[test]
fn checkpoint_external_payload_mapping_preserves_canonical_state() {
    let mut source = owner();
    source.append_fact("first", "equal source bytes").unwrap();
    source.append_fact("second", "equal source bytes").unwrap();
    let checkpoint = source.checkpoint().unwrap();
    let mut payloads = Vec::new();
    let referenced = checkpoint
        .clone()
        .try_map_payloads(|id, bytes| {
            let index = payloads.len();
            payloads.push((id, bytes));
            Ok::<_, ()>(index)
        })
        .unwrap();
    assert_ne!(payloads[0].0, payloads[1].0);
    assert_eq!(payloads[0].1, payloads[1].1);
    let encoded = serde_json::to_vec(&referenced).unwrap();
    assert!(!String::from_utf8_lossy(&encoded).contains("equal source bytes"));
    let referenced: ReplayEpochCheckpoint<usize> = serde_json::from_slice(&encoded).unwrap();
    let resolved = referenced
        .try_map_payloads(|id, index| {
            assert_eq!(id, payloads[index].0);
            Ok::<_, ()>(payloads[index].1.clone())
        })
        .unwrap();
    assert_eq!(resolved, checkpoint);
    let restored = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        resolved,
        owner().context().clone(),
    )
    .unwrap();
    assert_eq!(restored.context(), source.context());
    assert_eq!(restored.input_estimates(), source.input_estimates());
    let unchanged = source.clone();
    let mut calls = 0;
    let failed = checkpoint.try_map_payloads(|_, bytes| {
        calls += 1;
        if calls == 2 {
            Err("storage failed")
        } else {
            Ok(bytes)
        }
    });
    assert_eq!(failed.unwrap_err(), "storage failed");
    assert_eq!(calls, 2);
    assert_eq!(source, unchanged);
}
