//! Shared redraw interpretation without transport or presentation authority.
use super::*;

/// Known and unknown event classifications and precedence preserve attach rules.
/// Event content is discarded; optional IDs remain distinct from authorization.
#[test]
fn wire_events_preserve_classification_and_precedence() {
    for (event, action) in [
        ("diagnostic", AttachRenderAction::None),
        ("snapshot_changed", AttachRenderAction::None),
        ("config_changed", AttachRenderAction::ImmediateView),
        ("pane_changed", AttachRenderAction::View),
        ("future_type", AttachRenderAction::View),
    ] {
        let body = serde_json::json!({"jsonrpc":"2.0","method":format!("event/{event}"),
            "params":{"event_type":event,"event_id":7,"content":"discard"}})
        .to_string();
        assert_eq!(strict_event_action(&body).unwrap(), (action, Some(7)));
    }
    let actions = [
        AttachRenderAction::None,
        AttachRenderAction::View,
        AttachRenderAction::ImmediateView,
        AttachRenderAction::InvalidateAndView,
        AttachRenderAction::Disconnect,
    ];
    for (left_index, left) in actions.iter().enumerate() {
        for (right_index, right) in actions.iter().enumerate() {
            assert_eq!(left.combine(*right), actions[left_index.max(right_index)]);
        }
    }
}

/// Malformed or unrelated notifications cannot become a redraw event. Generic
/// diagnostics never include untrusted payload values or credentials.
#[test]
fn wire_events_reject_mismatched_notifications_without_payload() {
    for body in [
        serde_json::json!({"jsonrpc":"1.0","method":"event/message","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"secret":"private-proof"}}),
        serde_json::json!({"jsonrpc":"2.0","method":"event/message","params":{"event_type":"pane_changed","secret":"private-proof"}}),
    ] {
        let error = strict_event_action(&body.to_string()).unwrap_err();
        assert!(!error.message().contains("private-proof"));
    }
}
