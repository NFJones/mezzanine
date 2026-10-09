//! Manual compaction's replay-free projection retains causal ownership.
//!
//! Synthetic valid groups exercise the actual shared projection without private
//! conversation data or a provider, including identical exact reference bytes.

use super::*;

/// Removing imported raw history must remove every member of its execution
/// group, not just transcript-labelled owners. Exact references with identical
/// visible bytes remain distinct, with their original metadata and ordering.
#[test]
fn runtime_manual_compaction_projection_removes_complete_replay_groups() {
    let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "current input",
    )])
    .unwrap();
    for name in ["selected", "tail"] {
        let group = mez_agent::ContextExecutionGroupId::new(name).unwrap();
        context
            .append_assistant_event("assistant", "same text", group.clone())
            .unwrap();
        for source in [
            ContextSourceKind::ActionResult,
            ContextSourceKind::CommittedEvidence,
            ContextSourceKind::McpServerReference,
            ContextSourceKind::McpServerSearchResult,
            ContextSourceKind::McpRetrievedManifest,
        ] {
            context
                .append_evidence_event(source, "evidence", "same text", group.clone(), None, true)
                .unwrap();
        }
        let owner = mez_agent::ProviderContinuityOwner::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "openai",
        )
        .unwrap();
        for event in [
            mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
                serde_json::json!({
                    "type":"function_call", "id":"fc_projection", "call_id":"call_projection",
                    "name":"submit_maap_action_batch", "arguments":"{}"
                }),
            ])
            .unwrap(),
            mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
                call_id: "call_projection".into(),
                output: "native result".into(),
            },
        ] {
            context
                .append_evidence_event(
                    ContextSourceKind::TranscriptTool,
                    "native",
                    event.to_transcript_content(),
                    group.clone(),
                    Some(owner.clone()),
                    true,
                )
                .unwrap();
        }
    }
    context
        .append_reference_event(
            ContextSourceKind::CommittedEvidence,
            "evidence",
            "same text",
        )
        .unwrap();
    context
        .append_reference_event(
            ContextSourceKind::Memory,
            "prior summary",
            "preserved summary",
        )
        .unwrap();
    context.validate_durable().unwrap();
    let original = context.clone();
    let expected = context
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id().is_none())
        .cloned()
        .collect::<Vec<_>>();
    let projected = runtime_compaction_context_without_transcript_blocks(context.clone()).unwrap();
    projected.validate_durable().unwrap();
    assert_eq!(projected.chronology(), expected);
    assert_eq!(context, original);
    assert!(
        projected
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::CommittedEvidence)
    );
}
