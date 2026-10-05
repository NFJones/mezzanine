//! Closed snapshot validation at the local client boundary.
//!
//! Snapshot identities remain inert and pinned to the retained stream. These
//! tests do not establish full terminal rendering or ordinary CLI activation.

use super::*;

/// Handle, role, numeric IDs, lease shape and row limits reject invalid replies
/// before exposing line content. Unknown fields cannot smuggle device proof.
#[test]
fn outbound_client_snapshot_validation_is_closed_and_identity_scoped() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"lines":["one 雪","two"]});
    let snapshot: Snapshot = serde_json::from_value(original.clone()).unwrap();
    validate_snapshot(&snapshot, &handle, 2).unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/selected_version", serde_json::json!(2)),
        ("/session/granted_role", serde_json::json!("automation")),
        ("/session/session_id", serde_json::json!("invalid")),
        ("/session/client_id", serde_json::json!("agent-%1")),
        ("/session/lease_id", serde_json::json!("lease-")),
        ("/lines", serde_json::json!(["one", "two", "three"])),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let snapshot: Snapshot = serde_json::from_value(changed).unwrap();
        assert!(
            validate_snapshot(&snapshot, &handle, 2).is_err(),
            "{pointer}"
        );
    }
    let mut extra = original;
    extra["session"]["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<Snapshot>(extra).is_err());
}
