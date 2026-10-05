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
        "lease_id":"lease-one","client_id":"c1"},"lines":["one 雪","two"],"line_style_spans":[[],[]],
        "cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"presentation_ids":[]});
    let snapshot: Snapshot = serde_json::from_value(original.clone()).unwrap();
    validate_snapshot(&snapshot, &handle, 2).unwrap();
    assert_eq!(snapshot.render_rate_limit_fps, None);
    assert_eq!(
        (snapshot.view_identity, snapshot.event_cutoff),
        (None, None)
    );
    let mut revision = original.clone();
    revision["view_identity"] = serde_json::json!("b".repeat(64));
    revision["event_cutoff"] = serde_json::json!(u64::MAX);
    let reported: Snapshot = serde_json::from_value(revision.clone()).unwrap();
    validate_snapshot(&reported, &handle, 2).unwrap();
    assert_eq!(reported.event_cutoff, Some(u64::MAX));
    for identity in ["B".repeat(64), "b".repeat(63), "z".repeat(64)] {
        revision["view_identity"] = serde_json::json!(identity);
        let invalid: Snapshot = serde_json::from_value(revision.clone()).unwrap();
        assert!(validate_snapshot(&invalid, &handle, 2).is_err());
    }
    revision["view_identity"] = serde_json::json!("b".repeat(64));
    revision["event_cutoff"] = serde_json::json!("7");
    assert!(serde_json::from_value::<Snapshot>(revision).is_err());
    for fps in [0_u64, 30, u64::MAX] {
        let mut reported = original.clone();
        reported["render_rate_limit_fps"] = serde_json::json!(fps);
        let reported: Snapshot = serde_json::from_value(reported).unwrap();
        assert_eq!(reported.render_rate_limit_fps, Some(fps));
    }
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!("30"),
        serde_json::json!(false),
    ] {
        let mut malformed = original.clone();
        malformed["render_rate_limit_fps"] = invalid;
        assert!(serde_json::from_value::<Snapshot>(malformed).is_err());
    }
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
