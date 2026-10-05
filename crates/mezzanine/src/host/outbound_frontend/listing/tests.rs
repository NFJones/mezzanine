//! Allowlisted lease summaries with finite identity and lifecycle checks.
use super::*;

/// Projection preserves public optional facts and source order while excluding
/// private peer fields. Invalid or duplicate rows cannot become valid listings.
#[test]
fn outbound_host_list_projection_is_closed_and_bounded() {
    let original = serde_json::json!([{"lease_id":"lease-one","session_id":"$1",
        "name":"work 雪","state":"active","created_at_unix_seconds":0,
        "expires_at_unix_seconds":null,"device_credential":"discard"}]);
    let rows = project_sessions(&original).unwrap();
    let projected = serde_json::to_value(&rows).unwrap();
    assert_eq!(projected[0]["name"], "work 雪");
    assert_eq!(projected[0]["created_at_unix_seconds"], 0);
    assert!(!projected.to_string().contains("discard"));
    for (pointer, value) in [
        ("/0/session_id", serde_json::json!("invalid")),
        ("/0/lease_id", serde_json::json!("lease-")),
        ("/0/state", serde_json::json!("unknown")),
        ("/0/name", serde_json::json!("\u{1b}")),
        ("/0/expires_at_unix_seconds", serde_json::json!(0)),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(project_sessions(&invalid).is_err());
    }
    let duplicate = serde_json::json!([original[0], original[0]]);
    assert!(project_sessions(&duplicate).is_err());
    assert!(
        project_sessions(&serde_json::json!(vec![
            original[0].clone();
            MAX_LISTED_SESSIONS + 1
        ]))
        .is_err()
    );
    assert!(project_sessions(&serde_json::json!([])).unwrap().is_empty());
}
