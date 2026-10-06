//! Exact-owner item polling preserves complete-content and no-replay boundaries.
use super::*;

mod wire;

/// Closed redraw replies cannot borrow another frontend/session or export private
/// metadata. Disconnect is terminal, not a reusable redraw result.
#[test]
fn outbound_client_items_validate_closed_redraw_ownership() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"kind":"redraw","handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"action":"view","event_id":7});
    let reply: RedrawReply = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(
        validate_redraw(&reply, &handle, &reply.session).unwrap(),
        AttachRenderAction::View
    );
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/kind", serde_json::json!("other")),
        ("/action", serde_json::json!("disconnect")),
        ("/action", serde_json::json!("execute")),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let invalid: RedrawReply = serde_json::from_value(invalid).unwrap();
        assert!(validate_redraw(&invalid, &handle, &reply.session).is_err());
    }
    let mut extra = original;
    extra["token"] = serde_json::json!("private");
    assert!(serde_json::from_value::<RedrawReply>(extra).is_err());
}
