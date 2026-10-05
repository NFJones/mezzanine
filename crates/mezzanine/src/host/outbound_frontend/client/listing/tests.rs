//! Exact management reply ownership without exporting remote proof.
use super::*;

/// A host-list reply must match the retained local handle and closed host-only
/// settlement. Unknown top-level fields cannot carry credentials across IPC.
#[test]
fn outbound_host_list_client_reply_requires_exact_owner() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"host":{
        "selected_version":3,"granted_role":"observer","host_only":true},"sessions":[]});
    let reply: ListReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle).unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/host/selected_version", serde_json::json!(2)),
        ("/host/granted_role", serde_json::json!("primary")),
        ("/host/host_only", serde_json::json!(false)),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let reply: ListReply = serde_json::from_value(invalid).unwrap();
        assert!(validate_reply(&reply, &handle).is_err());
    }
    let mut extra = original;
    extra["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<ListReply>(extra).is_err());
}
