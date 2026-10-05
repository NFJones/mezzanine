//! Closed display projection tests; unknown peer metadata never crosses IPC.
//!
//! These probes do not establish terminal-input, presentation-acknowledgement,
//! styles, event-stream or complete attached-renderer support.

use super::*;

/// Correlation, role, client geometry and row count must match the retained
/// session. Unknown fields are discarded rather than forwarded as authority.
#[test]
fn outbound_view_projection_is_closed_and_geometry_bound() {
    let request = ViewRequest {
        handle: FrontendHandle {
            owner: "fixture".into(),
            generation: 1,
        },
        columns: 80,
        rows: 2,
    };
    let session = serde_json::json!({"granted_role":"primary"});
    let original = serde_json::json!({"jsonrpc":"2.0","id":VIEW_REQUEST_ID,
        "result":{"view":{"role":"primary","client_size":{"columns":80,"rows":2},
        "lines":["first 雪","second"],"private_metadata":"not forwarded"}}});
    assert_eq!(
        project_view_lines(&original.to_string(), &request, &session).unwrap(),
        vec!["first 雪", "second"]
    );
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/view/role", serde_json::json!("observer")),
        ("/result/view/client_size/columns", serde_json::json!(81)),
        ("/result/view/client_size/rows", serde_json::json!(3)),
        (
            "/result/view/lines",
            serde_json::json!(["one", "two", "three"]),
        ),
        ("/result/view/lines", serde_json::json!([7])),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(project_view_lines(&changed.to_string(), &request, &session).is_err());
    }
}
