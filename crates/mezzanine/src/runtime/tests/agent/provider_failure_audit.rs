//! Runtime provider-failure audit and transcript secrecy tests.

use super::*;

/// Builds a running turn identity for provider-failure audit tests.
fn provider_failure_turn() -> AgentTurnRecord {
    AgentTurnRecord {
        turn_id: "provider-failure-turn".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    }
}

/// Verifies the provider-failure audit record and the transcript-facing error
/// text never retain credential-shaped provider text handed over by an
/// unsanitized caller, while the typed error category and structured failure
/// payload stay available for correlation.
///
/// The provider adapter sanitizes provider-authored text at the shared
/// boundary and the product conversion re-applies it defensively. This
/// regression documents that the persisted audit record and the transcript
/// error text observe only the sanitized display message, so a bearer token or
/// API-key shape cannot survive the durable surfaces.
#[test]
fn runtime_provider_failure_audit_sanitizes_unsanitized_provider_text() {
    const SENTINEL: &str = "sk-ant-api03-AUDITSENTINEL000000000";
    let root = temp_root("runtime-provider-failure-audit");
    let audit_path = root.join("audit.jsonl");
    let mut service = test_runtime_service();
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let turn = provider_failure_turn();
    let profile = ModelProfile {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        model_capabilities: Default::default(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let error = MezError::from(
        mez_agent::ProviderResponseError::invalid_state(format!(
            "invalid api key: Bearer {SENTINEL}"
        ))
        .with_provider_failure_json(
            serde_json::json!({
                "status_code": 401,
                "request_id": "req_audit_safe",
                "error": {
                    "type": "authentication_error",
                    "message": format!("Bearer {SENTINEL}")
                }
            })
            .to_string(),
        ),
    );

    service
        .append_provider_request_failure_audit(&turn, &profile, "anthropic", &error)
        .unwrap();

    let audit_output = fs::read_to_string(&audit_path).unwrap();
    assert!(!audit_output.contains(SENTINEL), "{audit_output}");
    let record: serde_json::Value = serde_json::from_str(audit_output.trim()).unwrap();
    assert_eq!(record["metadata"]["error_kind"], "invalid_state");
    assert_eq!(record["metadata"]["error_message"], "[REDACTED]");
    assert!(
        record["metadata"]["provider_failure_json"].is_string(),
        "structured failure payload must stay available for correlation"
    );

    // The pane trace projection renders the same product error, so its
    // structured provider-error record must inherit the sanitized message while
    // keeping the safe request id and error type available for correlation.
    service
        .append_agent_trace_provider_error(&turn, "anthropic", &profile, &error)
        .unwrap();
    let trace_text = service
        .agent_pane_trace_log_text(&turn.pane_id)
        .expect("provider error trace text");
    assert!(!trace_text.contains(SENTINEL), "{trace_text}");
    let trace_record: serde_json::Value = serde_json::from_str(
        trace_text
            .split("MAAP provider_error\n")
            .nth(1)
            .expect("provider error trace payload"),
    )
    .unwrap();
    assert_eq!(trace_record["error"]["kind"], "invalid_state");
    assert_eq!(trace_record["error"]["message"], "[REDACTED]");
    assert_eq!(
        trace_record["provider_failure_json"]["request_id"],
        "req_audit_safe"
    );
    assert_eq!(
        trace_record["provider_failure_json"]["error"]["type"],
        "authentication_error"
    );
    fs::remove_dir_all(root).unwrap();
}
