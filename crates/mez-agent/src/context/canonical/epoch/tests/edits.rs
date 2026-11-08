//! Raw content revision at the descriptive checkpoint boundary. These tests
//! separate editable source from original live task/native/peer authority.

use super::*;

/// A retained occurrence changes content and estimates while its head and all
/// other source/accounting/lifecycle fields remain byte-identical. Equal text
/// in another occurrence does not participate in the edit.
#[test]
fn checkpoint_raw_edit_preserves_head_identity_membership_and_unrelated_source() {
    let mut source = owner();
    source
        .append_fact("covered", "original covered source")
        .unwrap();
    let frozen = plan(&source);
    source.append_fact("tail", "same text").unwrap();
    source.append_fact("tail", "same text").unwrap();
    source
        .publish_compaction(
            frozen,
            ReplayHeadPublication::Summary("original head".into()),
        )
        .unwrap();
    let original = source.checkpoint().unwrap();
    let edited = source.context().chronology()[1].record();
    let content = "longer revised payload with Unicode 界 and newline\n".repeat(5);
    let revised = original
        .clone()
        .revise_raw_member(edited.occurrence_id(), content.clone())
        .unwrap();
    let mut expected = serde_json::to_value(&original).unwrap();
    let actual = serde_json::to_value(&revised).unwrap();
    expected["events"][1]["block"]["content"] = serde_json::json!(content);
    expected["events"][1]["record"] = actual["events"][1]["record"].clone();
    assert_eq!(actual, expected);
    let cold = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        revised,
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(cold.head(), source.head());
    assert_eq!(cold.revision(), source.revision());
    assert_eq!(cold.members(), source.members());
    assert_eq!(
        cold.context().chronology()[0],
        source.context().chronology()[0]
    );
    assert_eq!(
        cold.context().chronology()[2],
        source.context().chronology()[2]
    );
    let new = cold.context().chronology()[1].record();
    assert_eq!(new.occurrence_id(), edited.occurrence_id());
    assert_ne!(new, edited);
    assert_ne!(
        cold.input_estimates().raw_input_tokens(),
        source.input_estimates().raw_input_tokens()
    );
}

/// Editing cannot restore covered source, substitute a head, repair an invalid
/// original membership.
#[test]
fn checkpoint_raw_edit_refuses_covered_head_missing_and_invalid_source() {
    let mut source = owner();
    source.append_fact("covered", "original covered").unwrap();
    let covered = source.context().chronology()[0].record().occurrence_id();
    let frozen = plan(&source);
    source.append_fact("tail", "original raw").unwrap();
    source
        .publish_compaction(frozen, ReplayHeadPublication::Summary("head".into()))
        .unwrap();
    let checkpoint = source.checkpoint().unwrap();
    for occurrence in [
        covered,
        source.head().unwrap().occurrence_id(),
        ContextBlockOccurrenceId::allocate(),
    ] {
        assert!(
            checkpoint
                .clone()
                .revise_raw_member(occurrence, "changed".into())
                .is_err()
        );
    }
    let raw = source.members()[0].occurrence_id();
    let mut invalid = serde_json::to_value(&checkpoint).unwrap();
    invalid["members"] = serde_json::json!([]);
    let invalid: ReplayEpochCheckpoint = serde_json::from_value(invalid).unwrap();
    assert!(invalid.revise_raw_member(raw, "changed".into()).is_err());
    assert_eq!(source.checkpoint().unwrap(), checkpoint);
}

/// Raw source has no compactor-summary byte target. Keep all edited UTF-8 bytes;
/// durable source/manifest limits belong to the separate storage transaction.
#[test]
fn checkpoint_raw_edit_keeps_large_raw_payload_without_summary_size_limit() {
    let mut source = owner();
    source.append_fact("raw", "original").unwrap();
    let content = "界".repeat(128 * 1024 / 3 + 1);
    let revised = source
        .checkpoint()
        .unwrap()
        .revise_raw_member(source.members()[0].occurrence_id(), content.clone())
        .unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        revised,
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(cold.context().chronology()[0].block().content, content);
    assert_eq!(cold.members(), source.members());
    assert_eq!(cold.head(), source.head());
    assert_eq!(cold.revision(), source.revision());
}

/// Native and peer fences are retained source, not editable model text. An edit
/// to an unrelated fact cannot revive controller handles from the old owner.
#[test]
fn checkpoint_raw_edit_keeps_pending_protection_and_old_handles_invalid() {
    let mut source = owner();
    source.append_fact("editable", "ordinary raw").unwrap();
    let raw = source.context().chronology()[0].record().occurrence_id();
    let group = ContextExecutionGroupId::new("pending-original").unwrap();
    let sequences = source
        .append_execution_events([ContextConversationAppend::assistant(
            "pending",
            "original pending source",
            group.clone(),
        )])
        .unwrap();
    let fence = source
        .capture_execution_owner(&group, sequences[0])
        .unwrap();
    let peer = source
        .append_pending_peer("peer", "original peer source")
        .unwrap();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("user prompt", "original exact root"),
        )
        .unwrap();
    let checkpoint = source.checkpoint().unwrap();
    for selected in [sequences[0], peer] {
        let occurrence = source
            .context()
            .chronology()
            .iter()
            .find(|event| event.sequence() == selected)
            .unwrap()
            .record()
            .occurrence_id();
        assert!(
            checkpoint
                .clone()
                .revise_raw_member(occurrence, "replacement".into())
                .is_err()
        );
    }
    let revised = checkpoint
        .clone()
        .revise_raw_member(raw, "new descriptive raw".into())
        .unwrap();
    assert_eq!(revised.stable_records(), checkpoint.stable_records());
    let mut cold = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        revised,
        AgentContext::empty(),
    )
    .unwrap();
    assert!(cold.context().execution_group_is_pending(&group));
    assert!(cold.context().peer_message_is_pending(peer));
    let before = cold.clone();
    assert!(cold.finish_execution_group(&fence).is_err());
    assert!(cold.complete_instruction_task(&task).is_err());
    assert_eq!(cold, before);
}

/// Closed native source may revise result prose while preserving original call
/// identity. Repointing a result to another call must fail whole-group validation.
#[test]
fn checkpoint_raw_edit_validates_closed_native_pairing() {
    use crate::{ProviderApiCompatibility, ProviderContinuityOwner, ProviderTranscriptEvent};
    let mut source = owner();
    let group = ContextExecutionGroupId::new("closed-native").unwrap();
    let provider = ProviderContinuityOwner::new(
        ProviderApiCompatibility::OpenAiChatCompletions,
        "offline-provider",
    )
    .unwrap();
    let call = ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
        "offline-provider".into(), "declaration".into(), vec![serde_json::json!({"id":"original-call", "type":"function", "function":{"name":crate::MAAP_ACTION_BATCH_TOOL_NAME,"arguments":"{}"}})],
    ).unwrap();
    source
        .append_execution_events([ContextConversationAppend::provider_assistant(
            "call",
            call.to_transcript_content(),
            group.clone(),
            provider.clone(),
        )])
        .unwrap();
    let result = |id: &str, text: &str| {
        ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
            provider_id: "offline-provider".into(),
            tool_call_id: id.into(),
            content: text.into(),
        }
        .to_transcript_content()
    };
    source
        .append_execution_events([ContextConversationAppend::evidence(
            ContextSourceKind::TranscriptTool,
            "result",
            result("original-call", "original prose"),
            group.clone(),
            Some(provider),
            true,
        )])
        .unwrap();
    finish_group(&mut source, &group).unwrap();
    let target = source
        .context()
        .chronology()
        .last()
        .unwrap()
        .record()
        .occurrence_id();
    let checkpoint = source.checkpoint().unwrap();
    let revised = checkpoint
        .clone()
        .revise_raw_member(target, result("original-call", "revised prose"))
        .unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        revised,
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!cold.context().execution_group_is_pending(&group));
    assert_eq!(cold.members(), source.members());
    assert!(
        checkpoint
            .revise_raw_member(target, result("foreign-call", "revised prose"))
            .is_err()
    );
}
