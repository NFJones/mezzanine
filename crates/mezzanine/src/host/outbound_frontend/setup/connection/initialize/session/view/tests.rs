//! Closed display projection tests; unknown peer metadata never crosses IPC.
//!
//! These probes do not establish terminal-input, presentation-acknowledgement,
//! styles, event-stream or complete attached-renderer support.

use super::*;

/// Missing cadence remains unavailable, while zero and reported rates retain
/// their exact values. Malformed explicit metadata cannot become a scheduling
/// policy or cross IPC as an invented default.
#[test]
fn outbound_view_render_rate_preserves_optional_policy() {
    assert_eq!(project_render_rate(&serde_json::json!({})).unwrap(), None);
    for fps in [0_u64, 30, u64::MAX] {
        assert_eq!(
            project_render_rate(&serde_json::json!({"result":{"render_rate_limit_fps":fps}}))
                .unwrap(),
            Some(fps)
        );
    }
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!("30"),
        serde_json::json!(false),
        serde_json::Value::Null,
    ] {
        assert!(
            project_render_rate(&serde_json::json!({"result":{"render_rate_limit_fps":invalid}}))
                .is_err()
        );
    }
}

/// Broker snapshots project only decoded cursor/output modes after checking
/// viewport bounds. Unknown peer metadata cannot become local terminal policy.
#[test]
fn outbound_view_modes_are_geometry_bound_and_metadata_free() {
    let original = serde_json::json!({"result":{"view":{
        "cursor":{"row":1,"column":7,"visible":true,"style":"underline","private":"discard"},
        "output_modes":{"bracketed_paste":true,"private":"discard"}
    }}});
    let projected = project_view_modes(&original.to_string(), 8, 2).unwrap();
    assert_eq!(projected["cursor"]["style"], "underline");
    assert_eq!(projected["output_modes"]["bracketed_paste"], true);
    assert!(!projected.to_string().contains("discard"));
    assert!(project_view_modes(&original.to_string(), 7, 2).is_err());
    assert!(project_view_modes(&original.to_string(), 8, 1).is_err());
}

/// Style projection preserves layered cell coordinates and decoded colors, but
/// removes unknown metadata before forwarding. Misaligned or out-of-range rows
/// reject rather than expose unvalidated styling to a frontend renderer.
#[test]
fn outbound_view_styles_are_bounded_and_metadata_free() {
    let original = serde_json::json!({"result":{"view":{"line_style_spans":[[
        {"start":0,"length":8,"rendition":{"bold":true,"private":"discard"}},
        {"start":2,"length":2,"rendition":{"foreground":{"kind":"indexed","index":7}}}
    ]]}}});
    let projected = project_view_styles(&original.to_string(), 1, 8).unwrap();
    assert_eq!(projected[0][0]["start"], 0);
    assert_eq!(projected[0][0]["length"], 8);
    assert_eq!(projected[0][1]["start"], 2);
    assert_eq!(projected[0][1]["rendition"]["foreground"]["index"], 7);
    assert!(!projected.to_string().contains("discard"));
    assert!(project_view_styles(&original.to_string(), 2, 8).is_err());
    assert!(project_view_styles(&original.to_string(), 1, 7).is_err());
}

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
