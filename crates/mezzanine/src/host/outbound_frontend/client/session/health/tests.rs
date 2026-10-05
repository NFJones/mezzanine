//! Closed coarse health facts require exact local and session ownership.
use super::*;

/// All quality categories retain their exact interpretation for a connected
/// sample. Foreign identity, unknown values and private metadata reject; down
/// samples cannot be represented as measured good/degraded/poor health.
#[test]
fn outbound_transport_health_client_is_closed_and_owner_scoped() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"connected":true,"quality":"unknown"});
    let reply: HealthReply = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(
        validate_reply(&reply, &handle, &reply.session).unwrap(),
        TerminalIrohStatusQuality::Unknown
    );
    for (word, expected) in [
        ("good", TerminalIrohStatusQuality::Good),
        ("degraded", TerminalIrohStatusQuality::Degraded),
        ("poor", TerminalIrohStatusQuality::Poor),
    ] {
        let mut value = original.clone();
        value["quality"] = serde_json::json!(word);
        let mut sample: HealthReply = serde_json::from_value(value).unwrap();
        assert_eq!(
            validate_reply(&sample, &handle, &reply.session).unwrap(),
            expected
        );
        sample.connected = false;
        assert!(validate_reply(&sample, &handle, &reply.session).is_err());
    }
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/quality", serde_json::json!("excellent")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let sample: HealthReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&sample, &handle, &reply.session).is_err());
    }
    let mut extra = original;
    extra["path"] = serde_json::json!("private");
    assert!(serde_json::from_value::<HealthReply>(extra).is_err());
}
