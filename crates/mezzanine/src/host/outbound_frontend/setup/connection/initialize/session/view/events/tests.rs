//! Bounded event request and burst classification without remote mutation.
use super::*;

/// Only the retained local handle and finite wait may consume event bytes.
/// Unknown fields cannot retarget the reader or request a remote method.
#[test]
fn outbound_event_poll_request_is_exact_and_bounded() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"events","handle":handle,"wait_ms":25});
    let request: EventRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle).unwrap();
    for (pointer, value) in [
        ("/operation", serde_json::json!("step")),
        ("/handle/generation", serde_json::json!(2)),
        ("/wait_ms", serde_json::json!(0)),
        ("/wait_ms", serde_json::json!(251)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: EventRequest = serde_json::from_value(changed).unwrap();
        assert!(validate_request(&request, &handle).is_err());
    }
    let mut extra = original;
    extra["session_id"] = serde_json::json!("$2");
    assert!(serde_json::from_value::<EventRequest>(extra).is_err());
}

/// An unidentified event makes the entire burst cutoff unknown, even when
/// later events have IDs. Stronger redraw requirements remain monotonic.
#[test]
fn outbound_event_poll_coalescing_preserves_unknown_cutoff() {
    let mut action = AttachRenderAction::View;
    let mut id = Some(7);
    combine(
        &mut action,
        &mut id,
        (AttachRenderAction::ImmediateView, None),
    );
    combine(&mut action, &mut id, (AttachRenderAction::View, Some(9)));
    assert_eq!(action, AttachRenderAction::ImmediateView);
    assert_eq!(id, None);
    let mut id = Some(7);
    combine(&mut action, &mut id, (AttachRenderAction::View, Some(9)));
    assert_eq!(id, Some(9));
}
