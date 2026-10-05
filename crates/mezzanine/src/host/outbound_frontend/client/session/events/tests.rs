//! Closed event reply validation without exposing payload or acquiring authority.
use super::*;

/// Event facts require exact stream/session ownership and a closed redraw
/// vocabulary. Unknown fields cannot export raw event content or credentials.
#[test]
fn outbound_event_poll_client_reply_is_owner_scoped() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"action":"view","event_id":7});
    let reply: EventReply = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(
        validate_reply(&reply, &handle, &reply.session).unwrap(),
        AttachRenderAction::View
    );
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/action", serde_json::json!("execute")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let changed: EventReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&changed, &handle, &reply.session).is_err());
    }
    let mut extra = original;
    extra["payload"] = serde_json::json!("must not cross IPC");
    assert!(serde_json::from_value::<EventReply>(extra).is_err());
}
