//! History import, retained identity, and provider continuity ownership tests.

use super::*;

/// Verifies legacy transcript import preserves a tool result that settles
/// after steering without moving it before the user event or inventing an
/// execution owner across the exact barrier.
#[test]
fn compatibility_import_keeps_straddling_evidence_as_exact_reference() {
    let context = AgentContext::new_durable(vec![
        ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "previous assistant action".to_string(),
            content: "started action-1".to_string(),
        },
        ContextBlock::user_event("user steering", "change the output format"),
        ContextBlock {
            source: ContextSourceKind::TranscriptTool,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "late action result action-1".to_string(),
            content: "action-1 completed".to_string(),
        },
    ])
    .unwrap();
    assert_eq!(
        context
            .chronology()
            .iter()
            .map(|event| event.block().label.as_str())
            .collect::<Vec<_>>(),
        [
            "previous assistant action",
            "user steering",
            "late action result action-1"
        ]
    );
    let late_evidence = &context.chronology()[2];
    assert_eq!(
        late_evidence.semantic_kind(),
        ContextSemanticKind::ReferenceEvent
    );
    assert_eq!(late_evidence.retention(), ContextRetention::Exact);
    assert!(late_evidence.execution_group_id().is_none());
}

/// Verifies a history refresh can change the imported prefix cardinality
/// without renumbering the active prompt or any later causal event.
#[test]
fn imported_history_prefix_replacement_preserves_retained_event_identities() {
    let mut context = AgentContext::new_durable(vec![
        ContextBlock::assistant_event("previous assistant", "older history"),
        ContextBlock::user_event("user prompt", "continue the task"),
    ])
    .unwrap();
    context
        .append_reference_event(
            ContextSourceKind::LocalMessage,
            "local message",
            "new constraint",
        )
        .unwrap();
    let group = ContextExecutionGroupId::new("current-execution").unwrap();
    context
        .append_assistant_event("current assistant", "run check", group.clone())
        .unwrap();
    context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "action result check",
            "passed",
            group,
            None,
            true,
        )
        .unwrap();
    let retained = context.chronology()[1..].to_vec();
    let replacement_count = context
        .replace_imported_history_prefix(
            |block| block.label == "previous assistant",
            vec![
                ContextBlock::reference_event(
                    ContextSourceKind::Memory,
                    "conversation compaction notice",
                    "older history was compacted",
                ),
                ContextBlock::reference_event(
                    ContextSourceKind::Memory,
                    "memory compact-session",
                    "semantic summary",
                ),
            ],
        )
        .unwrap();
    assert_eq!(replacement_count, 2);
    assert_eq!(&context.chronology()[2..], retained.as_slice());
    assert!(context.chronology()[1].sequence() < retained[0].sequence());
}

/// Verifies history refresh rejects fragmented ownership atomically instead
/// of gathering records across a local-message barrier.
#[test]
fn imported_history_prefix_replacement_rejects_fragmented_ownership_atomically() {
    let mut context = AgentContext::new_durable(vec![
        ContextBlock::reference_event(ContextSourceKind::Memory, "old history 1", "one"),
        ContextBlock::reference_event(ContextSourceKind::LocalMessage, "local message", "barrier"),
        ContextBlock::reference_event(ContextSourceKind::Memory, "old history 2", "two"),
        ContextBlock::user_event("user prompt", "continue"),
    ])
    .unwrap();
    let original = context.clone();
    let error = context
        .replace_imported_history_prefix(
            |block| block.source == ContextSourceKind::Memory,
            vec![ContextBlock::reference_event(
                ContextSourceKind::Memory,
                "replacement history",
                "summary",
            )],
        )
        .unwrap_err();
    assert!(error.message().contains("contiguous chronology prefix"));
    assert_eq!(context, original);
}

/// Verifies restoration retains an exact provider/API owner and rejects a
/// mismatched transcript family without mutating the imported context.
#[test]
fn imported_execution_restoration_preserves_exact_owner_and_fails_closed() {
    let content = ProviderTranscriptEvent::validated_openai_response_output(vec![
        serde_json::json!({"type":"reasoning","id":"reasoning-1"}),
    ])
    .unwrap()
    .to_transcript_content();
    let assistant = ContextBlock::assistant_event("assistant", "neutral fallback");
    let block = ContextBlock {
        source: ContextSourceKind::TranscriptTool,
        placement: ContextPlacement::ConversationAppend,
        label: "native response".to_string(),
        content,
    };
    let group = ContextExecutionGroupId::new("restored-execution").unwrap();
    let owner = ProviderContinuityOwner::new(
        ProviderApiCompatibility::OpenAiResponses,
        "configured-openai",
    )
    .unwrap();
    let assistant_record =
        ImportedExecutionEvent::new(assistant.clone(), group.clone(), 1, None).unwrap();
    let native_record =
        ImportedExecutionEvent::new(block.clone(), group.clone(), 2, Some(owner.clone())).unwrap();
    let mut context = AgentContext::import_durable_blocks(vec![assistant, block.clone()]).unwrap();
    context
        .restore_imported_execution_events(&[assistant_record.clone(), native_record.clone()])
        .unwrap();
    assert_eq!(context.chronology()[1].provider_owner(), Some(&owner));
    assert_eq!(context.chronology()[1].execution_group_id(), Some(&group));
    let original = context.clone();
    let reordered = [native_record, assistant_record];
    let error = context
        .restore_imported_execution_events(&reordered)
        .unwrap_err();
    assert!(error.message().contains("ordinals must be contiguous"));
    assert_eq!(context, original);
    let mismatched_owner = ProviderContinuityOwner::new(
        ProviderApiCompatibility::DeepSeekChatCompletions,
        "configured-openai",
    )
    .unwrap();
    assert!(ImportedExecutionEvent::new(block, group, 2, Some(mismatched_owner)).is_err());
}

/// Verifies initial durable import infers the generic Chat Completions API
/// and configured provider before exact execution metadata is restored.
#[test]
fn durable_import_accepts_generic_chat_continuity_owner() {
    let event = ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
        "local-openai-chat".to_string(), String::new(),
        vec![serde_json::json!({"id":"call-restored-1","type":"function","function":{"name":"submit_maap_action_batch","arguments":"{}"}})],
    ).unwrap();
    let block = ContextBlock {
        source: ContextSourceKind::TranscriptTool,
        placement: ContextPlacement::ConversationAppend,
        label: "generic native call".to_string(),
        content: event.to_transcript_content(),
    };
    let context = AgentContext::import_durable_blocks(vec![block]).unwrap();
    let owner = context.chronology()[0].provider_owner().unwrap();
    assert!(owner.matches_provider(
        ProviderApiCompatibility::OpenAiChatCompletions,
        "local-openai-chat"
    ));
}

/// Verifies exact continuity owners reject identities whose byte-exact
/// representation is empty, padded, whitespace-only, or control-bearing.
#[test]
fn provider_continuity_owner_rejects_invalid_provider_ids() {
    for provider_id in [
        "", " ", "\t", " padded", "padded ", "a\nb", "a\0b", "a\u{85}b",
    ] {
        assert!(
            ProviderContinuityOwner::new(ProviderApiCompatibility::OpenAiResponses, provider_id)
                .is_none(),
            "provider id should be rejected: {provider_id:?}"
        );
    }
    assert!(
        ProviderContinuityOwner::new(
            ProviderApiCompatibility::OpenAiResponses,
            "configured openai"
        )
        .is_some()
    );
}
