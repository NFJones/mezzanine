//! Typed request intent and dependency selection in provider-neutral MAAP.
//!
//! Parsing preserves ordinary actions and refuses to infer lifecycle authority
//! from free-form correlation text, deadlines, or omitted request intent.

use super::*;

/// Omitted intent stays false, explicit true is preserved, and strings cannot
/// masquerade as a boolean. Correlation remains independent untrusted text.
#[test]
fn request_bound_send_parser_preserves_explicit_intent_and_default() {
    for (extra, expected) in [
        ("", false),
        (",\"expect_reply\":true", true),
        (",\"expect_reply\":false", false),
    ] {
        let action = parse_maap_action_json(&format!(r#"{{"type":"send_message","recipient":"agent:worker","content_type":"text/plain; charset=utf-8","payload":"question?","correlation_id":"not-authority"{extra}}}"#)).unwrap();
        assert!(
            matches!(action.payload, AgentActionPayload::SendMessage { expect_reply, correlation_id: Some(_), .. } if expect_reply == expected)
        );
    }
    assert!(parse_maap_action_json(r#"{"type":"send_message","recipient":"agent:worker","content_type":"text/plain; charset=utf-8","payload":"question","expect_reply":"true"}"#).is_err());
}

/// Plain wait remains valid while targeted selection survives exact parsing.
/// Deadlines and forged authority fields reject before runtime admission, and
/// from_agent without a message selector is a deterministic contract error.
#[test]
fn request_bound_wait_parser_and_contract_reject_unsupported_authority() {
    let plain = parse_maap_action_json(r#"{"type":"wait"}"#).unwrap();
    assert!(matches!(
        plain.payload,
        AgentActionPayload::Wait {
            for_message: None,
            from_agent: None
        }
    ));
    let targeted = parse_maap_action_json(
        r#"{"type":"wait","for_message":"actual-accepted-id","from_agent":"agent-worker"}"#,
    )
    .unwrap();
    assert!(
        matches!(&targeted.payload, AgentActionPayload::Wait { for_message: Some(message), from_agent: Some(agent) } if message == "actual-accepted-id" && agent == "agent-worker")
    );
    for value in [
        r#"{"type":"wait","timeout_ms":1}"#,
        r#"{"type":"wait","owned_task":true}"#,
    ] {
        assert!(parse_maap_action_json(value).is_err());
    }
    if let Ok(action) = parse_maap_action_json(r#"{"type":"wait","from_agent":"agent-worker"}"#) {
        assert!(
            MaapBatch {
                rationale: "await a peer".into(),
                actions: vec![action]
            }
            .validate_harness_contract(&[], &[])
            .is_err()
        );
    }
}

/// Canonical schema and its strict native-provider projection expose the same
/// fields; normalization must retain request intent and dependency selectors.
#[test]
fn request_bound_schema_survives_native_strict_projection() {
    let schema = maap_action_batch_schema(&AllowedActionSet::all_enabled(), &[]);
    let strict = crate::normalize_openai_strict_schema(schema.clone());
    for schema in [schema, strict] {
        let rendered = schema.to_string();
        for field in ["expect_reply", "for_message", "from_agent"] {
            assert!(rendered.contains(field), "{field}");
        }
        assert!(rendered.contains("Tracking capacity rejection applies before sending"));
    }
}
