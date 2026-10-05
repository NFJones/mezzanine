//! Closed local sampling requests cannot retarget a retained connection.
use super::*;

/// Handle and operation validation precede sampling; unknown fields cannot
/// request counters, path identifiers or arbitrary remote control methods.
#[test]
fn outbound_transport_health_request_is_closed_and_exact() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"health","handle":handle});
    let request: HealthRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle).unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/operation", serde_json::json!("step")),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let request: HealthRequest = serde_json::from_value(invalid).unwrap();
        assert!(validate_request(&request, &handle).is_err());
    }
    let mut extra = original;
    extra["path"] = serde_json::json!(true);
    assert!(serde_json::from_value::<HealthRequest>(extra).is_err());
}
