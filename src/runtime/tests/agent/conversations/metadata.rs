//! Agent conversation metadata tests.

use super::*;

/// Verifies `/resume` reloads saved provider token totals for the rebound
/// conversation.
///
/// Active-session metadata is the durable source for pane-level provider
/// accounting. A manual resume path must hydrate the same in-memory usage map
/// as daemon startup restore so `/status` does not reset token counts to zero.
#[test]
fn runtime_resume_restores_provider_token_usage_from_session_metadata() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-tokens"));
    let mut service = test_runtime_service();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved-tokens".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume with prior token totals".to_string(),
        })
        .unwrap();
    let saved_token_usage_key = mez_agent::ModelTokenUsageKey::new("openai", "gpt-saved");
    let saved_token_usage = mez_agent::ModelTokenUsage {
        input_tokens: 900,
        output_tokens: 80,
        reasoning_tokens: 33,
        cached_input_tokens: Some(450),
        cache_write_input_tokens: None,
    };
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[mez_agent::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "saved-tokens".to_string(),
                prompt_cache_lineage_id: "lineage-saved-tokens".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                directive: Some("Prefer focused tests.".to_string()),
                routing_enabled: Some(true),
                approval_policy: Some("full-access".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: Some("42%".to_string()),
                context_usage_snapshot: Some(mez_agent::AgentContextUsageSnapshot {
                    input_tokens: 420,
                    context_window_tokens: 1000,
                    cached_input_tokens: Some(450),
                }),
                token_usage: saved_token_usage,
                token_usage_by_model: std::collections::BTreeMap::from([(
                    saved_token_usage_key.clone(),
                    saved_token_usage,
                )]),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-tokens","method":"agent/shell/command","params":{"idempotency_key":"resume-tokens","input":"/resume saved-tokens"}}"#,
        &primary,
    );
    assert!(
        resumed.contains("conversation_id=saved-tokens"),
        "{resumed}"
    );
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service
            .agent_context_usage_by_conversation
            .get("saved-tokens")
            .map(String::as_str),
        Some("42%")
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status","input":"/status"}}"#,
        &primary,
    );

    assert!(
        status.contains(
            "| Pane agent tokens | gpt-saved via openai: input=450 cached_input=450 cache_hit=50.00% output=80 reasoning=33 total=980 |"
        ),
        "{status}"
    );
    assert!(
        status.contains("| openai | gpt-saved | 450 | 450 | 80 | 33 | 50.00% |"),
        "{status}"
    );
    let restored_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(restored_metadata.len(), 1, "{restored_metadata:#?}");
    let restored_metadata = &restored_metadata[0];
    assert_eq!(
        restored_metadata.conversation_id, "saved-tokens",
        "{restored_metadata:#?}"
    );
    assert_eq!(
        restored_metadata.token_usage_by_model,
        std::collections::BTreeMap::from([(saved_token_usage_key.clone(), saved_token_usage,)]),
        "{restored_metadata:#?}"
    );

    let (_, mut profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    profile.provider = "openai".to_string();
    profile.model = "gpt-saved".to_string();
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
            cache_write_input_tokens: None,
        },
        Some(&profile),
    );
    let resumed_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status-after-usage","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status-after-usage","input":"/status"}}"#,
        &primary,
    );
    assert!(
        resumed_status.contains("| openai | gpt-saved | 525 | 475 | 100 | 38 | 47.50% |"),
        "{resumed_status}"
    );
    let resumed_metadata = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(resumed_metadata.len(), 1, "{resumed_metadata:#?}");
    assert_eq!(
        resumed_metadata[0].token_usage_by_model,
        std::collections::BTreeMap::from([(
            saved_token_usage_key,
            mez_agent::ModelTokenUsage {
                input_tokens: 1000,
                output_tokens: 100,
                reasoning_tokens: 38,
                cached_input_tokens: Some(475),
                cache_write_input_tokens: None,
            },
        )]),
        "{resumed_metadata:#?}"
    );
}

/// Verifies that `/fork` returns a concrete runtime diagnostic when no
/// transcript store is attached instead of falling back to a generic
/// runtime-required placeholder.
#[test]
fn runtime_agent_shell_fork_reports_missing_transcript_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork-missing-store","method":"agent/shell/command","params":{"idempotency_key":"fork-missing-store","input":"/fork branch"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"fork""#), "{response}");
    assert!(
        response.contains("forked=false reason=transcript-store-unavailable"),
        "{response}"
    );
    assert!(response.contains("source=runtime-fork"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}
