//! Item requests cannot acquire clipboard authority or retarget another owner.
use super::*;

/// Exact admission precedes item consumption. Foreign handles, unnegotiated
/// sessions, arbitrary fields and unbounded waits reject before stream reads.
#[test]
fn outbound_clipboard_item_request_is_closed_and_capability_scoped() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"items","handle":handle,"wait_ms":25});
    let request: ItemRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle, true).unwrap();
    assert!(validate_request(&request, &handle, false).is_err());
    for (pointer, value) in [
        ("/operation", serde_json::json!("events")),
        ("/handle/generation", serde_json::json!(2)),
        ("/wait_ms", serde_json::json!(0)),
        ("/wait_ms", serde_json::json!(251)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: ItemRequest = serde_json::from_value(changed).unwrap();
        assert!(validate_request(&request, &handle, true).is_err());
    }
    let mut extra = original;
    extra["client_id"] = serde_json::json!("c2");
    assert!(serde_json::from_value::<ItemRequest>(extra).is_err());
}
