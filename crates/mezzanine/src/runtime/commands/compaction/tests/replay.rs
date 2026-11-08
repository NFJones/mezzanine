//! Canonical preview consumes whole selected groups and preserves exact tail.
//! Pending native bytes remain protected; no test-only transcript stripper or
//! paired-wire completion inference supplies source ownership.

use super::*;

#[test]
fn runtime_manual_compaction_projection_removes_complete_replay_groups() {
    let id = mez_agent::ReplayConversationId::new("group-preview").unwrap();
    let mut owner = mez_agent::ReplayEpochContext::new(id.clone(), AgentContext::empty()).unwrap();
    let selected = mez_agent::ContextExecutionGroupId::new("selected").unwrap();
    let tail = mez_agent::ContextExecutionGroupId::new("tail").unwrap();
    let mut frozen = None;
    for group in [&selected, &tail] {
        let mut events = vec![mez_agent::ContextConversationAppend::assistant(
            "assistant",
            "same text",
            group.clone(),
        )];
        for source in [
            ContextSourceKind::ActionResult,
            ContextSourceKind::CommittedEvidence,
            ContextSourceKind::McpServerReference,
            ContextSourceKind::McpServerSearchResult,
        ] {
            events.push(mez_agent::ContextConversationAppend::evidence(
                source,
                "evidence",
                "same text",
                group.clone(),
                None,
                true,
            ));
        }
        let frontier = owner
            .append_execution_events(events)
            .unwrap()
            .pop()
            .unwrap();
        if group == &selected {
            let receipt = owner.capture_execution_owner(group, frontier).unwrap();
            owner.finish_execution_group(&receipt).unwrap();
            frozen = Some(
                match owner
                    .prepare_compaction(mez_agent::ReplayTailPolicy::new(10_000, 100).unwrap())
                    .unwrap()
                {
                    mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
                    other => panic!("selected original source expected: {other:?}"),
                },
            );
        }
    }
    let provider = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "openai",
    )
    .unwrap();
    for event in [
        mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({"type":"function_call","id":"fc_preview","call_id":"call_preview",
                "name":"submit_maap_action_batch","arguments":"{}"}),
        ])
        .unwrap(),
        mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_preview".into(),
            output: "native result".into(),
        },
    ] {
        owner
            .append_execution_events([mez_agent::ContextConversationAppend::evidence(
                ContextSourceKind::TranscriptTool,
                "native",
                event.to_transcript_content(),
                tail.clone(),
                Some(provider.clone()),
                true,
            )])
            .unwrap();
    }
    let mut body = owner.into_owned_context();
    body.append_reference_event(
        ContextSourceKind::CommittedEvidence,
        "evidence",
        "same text",
    )
    .unwrap();
    body.append_reference_event(
        ContextSourceKind::Memory,
        "prior summary",
        "preserved summary",
    )
    .unwrap();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(id, body).unwrap();
    let original = owner.context().clone();
    let plan = frozen.unwrap();
    let expected = original.chronology()[plan.summary_input().len()..].to_vec();
    let prepared = owner
        .prepare_compaction_publication(
            plan,
            mez_agent::ReplayHeadPublication::Summary("one canonical summary".into()),
        )
        .unwrap();
    let projected = prepared.candidate().context();
    assert_eq!(&projected.chronology()[1..], expected);
    assert_eq!(
        projected.chronology()[0].block().content,
        "one canonical summary"
    );
    assert!(projected.execution_group_is_pending(&tail));
    assert!(
        !projected
            .chronology()
            .iter()
            .any(|event| event.execution_group_id() == Some(&selected))
    );
    assert!(
        projected
            .chronology()
            .iter()
            .any(
                |event| event.block().source == ContextSourceKind::CommittedEvidence
                    && event.execution_group_id().is_none()
            )
    );
    assert_eq!(owner.context(), &original);
}
