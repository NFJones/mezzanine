//! Objective metadata round trips and legacy missing-field compatibility.

use super::*;

/// Snapshot round trips carry the objective, and a legacy snapshot payload
/// without the field restores to no objective instead of failing.
#[test]
fn snapshot_round_trip_carries_objective_and_legacy_payload_defaults_none() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Inspect the backlog"))
        .unwrap();
    let snapshot = service.snapshot_state();
    assert_eq!(
        snapshot.registered_agents[0].objective.as_deref(),
        Some("Inspect the backlog")
    );
    let restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(
        restored
            .registered_identity(&identity.agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Inspect the backlog")
    );
    let legacy_payload = format!(
        r#"{{"protocol":"{}","schema_version":1,"next_sequence":1,"retention_messages":1000,"retention_bytes":1048576,"registered_agents":[{{"agent_id":"{}","pane_id":null,"window_id":null,"role":"agent","capabilities":[]}}],"presence":[],"subscriptions":[],"retained_messages":[],"accepted_messages":[]}}"#,
        MMP_PROTOCOL,
        identity.agent_id.as_str()
    );
    let legacy = serde_json::from_str::<MessageServiceSnapshot>(&legacy_payload).unwrap();
    assert!(legacy.registered_agents[0].objective.is_none());
    let restored = MessageService::from_snapshot_state(&legacy).unwrap();
    assert!(
        restored
            .registered_identity(&identity.agent_id)
            .is_some_and(|identity| identity.objective.is_none())
    );
}
