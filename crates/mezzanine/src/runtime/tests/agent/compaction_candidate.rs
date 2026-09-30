//! Manual compaction candidate replay parity for retained native executions.
//!
//! Candidate sizing must retain the same execution ownership as published
//! history, including rows accepted while the compactor is running.

use super::*;
use crate::integrations::agent::context::assemble_model_request;

/// A post-plan native call/result group in the pending append lane must keep
/// its provider ownership and ordinals in the prospective candidate context.
#[test]
fn runtime_manual_compaction_candidate_retains_native_execution_ownership() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "candidate-native".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"candidate-native\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.candidate-native]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("candidate-native"));
    let mut row = TranscriptEntry {
        conversation_id: "candidate-native".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::Assistant,
        turn_id: "old-turn".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "old work to summarize".to_string(),
    };
    store.append(&row).unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "candidate-native", 1)
        .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"candidate-native","method":"agent/shell/command","params":{"idempotency_key":"candidate-native","input":"/compact"}}"#,
        &primary,
    );
    assert!(response.contains("state=queued"), "{response}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let group = mez_agent::ContextExecutionGroupId::new("retained-native").unwrap();
    let owner = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "openai",
    )
    .unwrap();
    let call = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
        serde_json::json!({
            "type":"function_call", "id":"fc_retained", "call_id":"call_retained",
            "name":"submit_maap_action_batch", "arguments":"{}"
        }),
    ])
    .unwrap()
    .to_transcript_content();
    let result = mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
        call_id: "call_retained".to_string(),
        output: "exact native result".to_string(),
    }
    .to_transcript_content();
    let mut rows = Vec::new();
    for (index, (source, label, content, provider_owner)) in [
        (
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            "retained assistant".to_string(),
            None,
        ),
        (
            ContextSourceKind::TranscriptTool,
            "native call",
            call,
            Some(owner.clone()),
        ),
        (
            ContextSourceKind::TranscriptTool,
            "native result",
            result,
            Some(owner.clone()),
        ),
        (
            ContextSourceKind::ActionResult,
            "result",
            "exact action result".to_string(),
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        row.sequence = index as u64 + 2;
        row.role = TranscriptRole::System;
        row.turn_id = "retained-turn".to_string();
        row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            source,
            label,
            content,
            group.clone(),
            index as u64 + 1,
            provider_owner,
        )
        .unwrap()
        .to_transcript_content();
        rows.push(row.clone());
    }
    service.persistence.enable_transcript_adapter();
    service
        .persistence
        .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path("candidate-native").unwrap(),
            store: store.clone(),
            entries: rows,
        });
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 4)
        .unwrap();
    let candidate = service
        .manual_compaction_candidate_context(&task, "short summary")
        .unwrap();
    let events = candidate
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id() == Some(&group))
        .collect::<Vec<_>>();
    assert_eq!(
        events.len(),
        4,
        "candidate must retain canonical execution ownership"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.provider_owner() == Some(&owner))
            .count(),
        2
    );
    candidate.validate_durable().unwrap();
    let mut invalid_task = task.clone();
    invalid_task.request.max_output_tokens = Some(1);
    assert!(
        service
            .manual_compaction_candidate_context(&invalid_task, "oversized summary")
            .is_err()
    );
    assert!(
        store
            .compaction_epoch("candidate-native")
            .unwrap()
            .is_none(),
        "candidate must not publish an epoch"
    );
    complete_runtime_test_compaction(&mut service, "%1", "short summary");
    let complete_published = service
        .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
        .unwrap()
        .with_metadata(candidate.metadata().clone());
    assert_eq!(
        candidate.blocks(),
        complete_published.blocks(),
        "complete candidate context must match ordinary post-publication assembly"
    );
    let turn = AgentTurnRecord {
        turn_id: "parity-turn".to_string(),
        conversation_id: "candidate-native".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".to_string(),
        model_profile: task.model_profile_name.clone(),
        parent_turn_id: None,
        state: AgentTurnState::Queued,
        cooperation_mode: None,
        initial_capability: None,
    };
    for provider in ["openai", "switched-openai"] {
        let mut profile = task.model_profile.clone();
        profile.provider = provider.to_string();
        let api = mez_agent::ProviderApiCompatibility::OpenAiResponses;
        let before = assemble_model_request(&profile, api, &turn, &candidate).unwrap();
        let after = assemble_model_request(&profile, api, &turn, &complete_published).unwrap();
        let before_body = mez_agent::openai_responses_request_body(&before).unwrap();
        let after_body = mez_agent::openai_responses_request_body(&after).unwrap();
        assert_eq!(before_body, after_body, "replay wire parity for {provider}");
        assert_eq!(
            mez_agent::provider_request_input_estimate_from_body(&before_body),
            mez_agent::provider_request_input_estimate_from_body(&after_body)
        );
        assert_eq!(before_body.contains("call_retained"), provider == "openai");
    }
}

/// A virtual compaction boundary must invalidate complete manifest-bearing
/// groups exactly as a published MCP epoch does, preserving exact barriers and
/// excluding malformed groups rather than restoring partial native ownership.
#[test]
fn runtime_compaction_candidate_mcp_epoch_matches_published_projection() {
    let group = mez_agent::ContextExecutionGroupId::new("manifest-group").unwrap();
    let malformed = mez_agent::ContextExecutionGroupId::new("malformed-group").unwrap();
    let contents = [
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            "retrieve manifest",
            group.clone(),
            1,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::McpRetrievedManifest,
            "manifest",
            "obsolete manifest evidence",
            group,
            2,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::ActionResult,
            "orphan",
            "malformed evidence",
            malformed,
            2,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::user_event(
            9,
            "exact steering",
            "Keep this exact barrier.",
        )
        .unwrap()
        .to_transcript_content(),
    ];
    let mut rows = contents
        .into_iter()
        .enumerate()
        .map(|(index, content)| TranscriptEntry {
            conversation_id: "projection-parity".to_string(),
            sequence: index as u64 + 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: "retained-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content,
        })
        .collect::<Vec<_>>();
    let prospective =
        crate::runtime::control::runtime_agent_compaction_replay_context("%1", &rows, &[]);
    rows.push(TranscriptEntry {
        conversation_id: "projection-parity".to_string(),
        sequence: 5,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "compact-turn".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
    });
    let published = crate::runtime::control::runtime_agent_transcript_context("%1", &rows);
    assert_eq!(prospective.blocks, published.blocks);
    assert_eq!(prospective.execution_events, published.execution_events);
    assert!(prospective.execution_events.is_empty());
    assert_eq!(prospective.blocks.len(), 1);
    assert_eq!(prospective.blocks[0].content, "Keep this exact barrier.");
}
