//! Exact kill request bounds and closed revocation settlement evidence.
use super::*;

/// The public target is untyped: ID-looking text may be an exact session name.
/// A correlated host success must not be rejected after revocation merely
/// because that name differs from the generated lease or session identity.
#[test]
fn outbound_kill_identifier_looking_names_preserve_host_resolution() {
    let settled = serde_json::json!({"killed":true,"lease_id":"lease-one",
        "session_id":"$1","state":"revoked"});
    for target in ["lease-work", "$999"] {
        assert!(project_settlement(&settled, target).is_ok(), "{target}");
    }
}

/// Only revoked, well-formed settlement may cross IPC. Target resolution remains
/// host-owned; unknown metadata is discarded without granting destructive power.
#[test]
fn outbound_kill_settlement_is_bounded_and_target_scoped() {
    let original = serde_json::json!({"killed":true,"lease_id":"lease-one",
        "session_id":"$1","state":"revoked","device_credential":"discard"});
    for target in ["$1", "lease-one", "work"] {
        let settled = project_settlement(&original, target).unwrap();
        assert!(!serde_json::to_string(&settled).unwrap().contains("discard"));
    }
    for (field, value) in [
        ("killed", serde_json::json!(false)),
        ("state", serde_json::json!("active")),
        ("session_id", serde_json::json!("invalid")),
        ("lease_id", serde_json::json!("lease-")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(project_settlement(&changed, "$1").is_err());
    }
    validate_request("work 雪", "exact-key").unwrap();
    for (target, key) in [("", "key"), ("work", ""), ("\u{1b}", "key"), ("work", "\n")] {
        assert!(validate_request(target, key).is_err());
    }
}
