//! Fixed capsule privacy and public receipt validation without vendor execution.
use super::*;

/// The fixed helper can project only an explicit public predecessor for one
/// same-creator/session handoff. It cannot infer newest state, accept null/unsafe
/// selectors or turn a capsule into general control or a bearer credential.
#[test]
fn harness_source_capsule_projects_only_explicit_safe_predecessor() {
    let capsule = serde_json::json!({"external_session_id":"session-a","observer_instance":"module-b","session_boundary":"resume","predecessor_generation":7});
    let parsed = Capsule::parse(capsule.to_string().as_bytes()).unwrap();
    let request: serde_json::Value = serde_json::from_str(&parsed.request("%1").unwrap()).unwrap();
    assert_eq!(request["method"], "agent/external/curated-enroll");
    assert_eq!(request["params"]["predecessor_generation"], 7);
    assert_eq!(request["params"]["harness"], "claude");
    assert!(request["params"].get("launch_token").is_none());
    let receipt = serde_json::json!({"protocol":"external-agent/1","registered":true,"controls":[],"agent_id":"external-a","generation":8,"observer_witness":"a".repeat(64),"run_id":1,"observer_epoch":2,"observer_instance":"module-b","external_session_id":"session-a","usage":"unavailable-source-continuity","observer_transport":"unavailable-curated-freshness","expires_at_unix_seconds":123,"lease_seconds":60});
    assert!(parsed.receipt(&receipt).is_some());
    for (key, value) in [("generation", 7), ("generation", 6), ("observer_epoch", 1)] {
        let mut stale = receipt.clone();
        stale[key] = value.into();
        assert!(parsed.receipt(&stale).is_none());
    }
    for value in [
        serde_json::json!(0),
        serde_json::json!(null),
        serde_json::json!(-1),
        serde_json::json!(9_007_199_254_740_992u64),
        serde_json::json!("7"),
        serde_json::json!(true),
        serde_json::json!(7.0),
    ] {
        let mut invalid = capsule.clone();
        invalid["predecessor_generation"] = value;
        assert!(Capsule::parse(invalid.to_string().as_bytes()).is_err());
    }
    assert!(Capsule::parse(br#"{"external_session_id":"a","observer_instance":"b","session_boundary":"resume","predecessor_generation":7,"predecessor_generation":8}"#).is_err());
    assert!(Capsule::parse(br#"{"external_session_id":"a","observer_instance":"b","session_boundary":"resume","predecessor_generation":7,"\u0070redecessor_generation":8}"#).is_err());
    let initial = Capsule::parse(
        br#"{"external_session_id":"a","observer_instance":"b","session_boundary":"startup"}"#,
    )
    .unwrap();
    let request: serde_json::Value = serde_json::from_str(&initial.request("%1").unwrap()).unwrap();
    assert!(request["params"].get("predecessor_generation").is_none());
}

/// Public epoch proof capsule and receipt cannot choose TTL/role/method or leak
/// raw responses. Exact sequence equality is required before exposure, including
/// an inert duplicate receipt which must never be mistaken for lease renewal.
#[test]
fn harness_source_heartbeat_capsule_and_receipt_are_fixed_public_proof() {
    let capsule = serde_json::json!({"operation":"curated-heartbeat","external_session_id":"session-a","generation":1,"observer_witness":"a".repeat(64),"sequence":1});
    let proof = heartbeat::Heartbeat::parse(capsule.to_string().as_bytes()).unwrap();
    assert!(proof.request().contains("agent/external/curated-heartbeat"));
    let valid = serde_json::json!({"observed":true,"sequence":1,"changed":false});
    assert_eq!(proof.receipt(&valid), Some(valid.clone()));
    let mut extra = valid;
    extra["launch_token"] = "private".into();
    assert!(proof.receipt(&extra).is_none());
    for (key, value) in [
        ("sequence", serde_json::json!(0)),
        ("observer_witness", serde_json::json!("bad")),
        ("ttl", serde_json::json!(999)),
        ("method", serde_json::json!("control/initialize")),
    ] {
        let mut changed = capsule.clone();
        changed[key] = value;
        assert!(heartbeat::Heartbeat::parse(changed.to_string().as_bytes()).is_err());
    }
}

/// Raw ambiguity, extra selectors/credentials, content-shaped IDs, unsupported
/// boundaries and oversized capsules cannot become a fixed source request.
#[test]
fn harness_source_capsule_rejects_ambiguity_credentials_and_content() {
    for value in [
        r#"{"external_session_id":"a","external_session_id":"b","observer_instance":"m","session_boundary":"startup"}"#,
        r#"{"external_session_id":"a","observer_instance":"m","session_boundary":"startup","launch_token":"private"}"#,
        r#"{"external_session_id":"prompt with spaces","observer_instance":"m","session_boundary":"startup"}"#,
        r#"{"external_session_id":"a","observer_instance":"m","session_boundary":"compact"}"#,
        r#"{"external_session_id":"a","observer_instance":"m","session_boundary":"startup","method":"control/initialize"}"#,
        r#"{"external_session_id":"a","observer_instance":"m","session_boundary":"startup","pid":42}"#,
        r#"{"external_session_id":"a","observer_instance":{"prompt":"private"},"session_boundary":"startup"}"#,
    ] {
        assert!(Capsule::parse(value.as_bytes()).is_err());
    }
    assert!(Capsule::parse(&vec![b' '; 4097]).is_err());
    let capsule = Capsule::parse(br#"{"external_session_id":"session-a","observer_instance":"module-a","session_boundary":"startup"}"#).unwrap();
    let request = capsule.request("%1").unwrap();
    assert!(request.contains("agent/external/curated-enroll"));
    assert!(!request.contains("launch_token"));
    assert!(capsule.request("%1\nprivate").is_err());
}

/// Only public matching source receipts escape. Unknown/private fields, invalid
/// selectors and fabricated counter/coverage shapes are rejected, never stripped
/// and echoed. A projected receipt is registration evidence, not usage commitment.
#[test]
fn harness_source_receipt_projects_only_matching_public_fields() {
    let capsule = Capsule::parse(br#"{"external_session_id":"session-a","observer_instance":"module-a","session_boundary":"startup"}"#).unwrap();
    let valid = serde_json::json!({"protocol":"external-agent/1","registered":true,"controls":[],"agent_id":"external-a","generation":1,"observer_witness":"a".repeat(64),"run_id":1,"observer_epoch":1,"observer_instance":"module-a","external_session_id":"session-a","usage":"unavailable-source-continuity","observer_transport":"unavailable-curated-freshness","expires_at_unix_seconds":123,"lease_seconds":60});
    assert_eq!(capsule.receipt(&valid), Some(valid.clone()));
    for (key, value) in [
        ("launch_token", serde_json::json!("private")),
        ("prompt", serde_json::json!("private")),
        ("generation", serde_json::json!(0)),
        ("observer_witness", serde_json::json!("bad")),
        ("external_session_id", serde_json::json!("foreign")),
        ("controls", serde_json::json!(["primary"])),
        ("observer_instance", serde_json::json!("foreign")),
        ("generation", serde_json::json!(9_007_199_254_740_992u64)),
        (
            "agent_id",
            serde_json::json!("private response with spaces"),
        ),
        ("usage", serde_json::json!({"prompt":"private"})),
    ] {
        let mut changed = valid.clone();
        changed[key] = value;
        assert!(capsule.receipt(&changed).is_none());
    }
}
