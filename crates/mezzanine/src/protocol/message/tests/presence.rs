//! Messaging presence tests.

use super::*;

/// Verifies presence updates are reported in stable order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn presence_updates_are_reported_in_stable_order() {
    let mut service = MessageService::default();
    let first = service.register_agent(None, None, "default", Vec::new());
    let _second = service.register_agent(None, None, "worker", Vec::new());

    service
        .update_presence(&first.agent_id, AgentPresenceStatus::Blocked, 50)
        .unwrap();

    let presence = service.presence();

    assert_eq!(presence[0].status, AgentPresenceStatus::Blocked);
    assert_eq!(presence[0].updated_at_ms, 50);
}

/// Verifies that discovery applies every identity, presence, and capability
/// filter together. This covers the service-level behavior used by MMP
/// `discover` requests before response serialization.
#[test]
fn discovery_filters_by_identity_presence_and_capability() {
    let mut service = MessageService::default();
    let reviewer = SenderIdentity {
        agent_id: AgentId::parse('a', "a42").unwrap(),
        pane_id: Some(PaneId::parse('%', "%7").unwrap()),
        window_id: Some(WindowId::parse('@', "@3").unwrap()),
        role: Some("reviewer".to_string()),
        capabilities: vec!["rust".to_string(), "tests".to_string()],
    };
    let writer = SenderIdentity {
        agent_id: AgentId::parse('a', "a43").unwrap(),
        pane_id: Some(PaneId::parse('%', "%8").unwrap()),
        window_id: Some(WindowId::parse('@', "@3").unwrap()),
        role: Some("writer".to_string()),
        capabilities: vec!["docs".to_string()],
    };
    service.ensure_agent_identity(reviewer.clone(), 10).unwrap();
    service.ensure_agent_identity(writer, 10).unwrap();
    service
        .update_presence(&reviewer.agent_id, AgentPresenceStatus::Blocked, 20)
        .unwrap();

    let matches = service.discover_agents_filtered(
        Some("a42"),
        Some("%7"),
        Some("@3"),
        Some("reviewer"),
        Some(AgentPresenceStatus::Blocked),
        &["rust".to_string(), "tests".to_string()],
    );
    let misses = service.discover_agents_filtered(
        Some("a42"),
        Some("%7"),
        Some("@3"),
        Some("reviewer"),
        Some(AgentPresenceStatus::Blocked),
        &["network".to_string()],
    );

    assert_eq!(matches, vec![reviewer]);
    assert!(misses.is_empty());
}

/// Verifies that heartbeat frames prove an agent is still live without
/// overwriting the agent's last declared presence state. This keeps `busy` and
/// `blocked` statuses meaningful while still advancing liveness time.
#[test]
fn heartbeat_updates_presence_timestamp_without_changing_status() {
    let mut service = MessageService::default();
    let agent = service.register_agent(None, None, "default", Vec::new());

    service
        .update_presence(&agent.agent_id, AgentPresenceStatus::Busy, 20)
        .unwrap();
    service.record_heartbeat(&agent.agent_id, 35).unwrap();

    let presence = service.presence();

    assert_eq!(presence[0].status, AgentPresenceStatus::Busy);
    assert_eq!(presence[0].updated_at_ms, 35);
}

/// Verifies the transport path for MMP heartbeat liveness. A registered agent
/// can report a non-default presence state and later heartbeat without another
/// request type resetting that state back to available.
#[test]
fn mmp_transport_heartbeat_updates_presence_timestamp() {
    let mut service = MessageService::default();
    let mut connection = MessageConnection::default();
    let hello =
        r#"{"protocol":"mmp/1","type":"hello","id":"h1","role":"worker","capabilities":[]}"#;
    let presence = r#"{"protocol":"mmp/1","type":"presence","id":"p1","status":"busy"}"#;
    let heartbeat = r#"{"protocol":"mmp/1","type":"heartbeat","id":"hb1"}"#;

    dispatch_mmp_body(hello, &mut service, &mut connection, 10);
    dispatch_mmp_body(presence, &mut service, &mut connection, 20);
    let response = dispatch_mmp_body(heartbeat, &mut service, &mut connection, 35);
    let records = service.presence();

    assert!(response.contains(r#""type":"ack""#), "{response}");
    assert!(response.contains(r#""message_id":"hb1""#), "{response}");
    assert_eq!(records[0].status, AgentPresenceStatus::Busy);
    assert_eq!(records[0].updated_at_ms, 35);
}

/// Verifies that MMP discover requests expose filtered discovery rather than
/// always returning every registered agent. The transport path parses role,
/// status, and capability filters before serializing matching identities.
#[test]
fn mmp_transport_discover_applies_presence_and_capability_filters() {
    let mut service = MessageService::default();
    let mut worker_connection = MessageConnection::default();
    let mut reviewer_connection = MessageConnection::default();
    let worker_hello = r#"{"protocol":"mmp/1","type":"hello","id":"h1","role":"worker","capabilities":["rust","tests"]}"#;
    let reviewer_hello = r#"{"protocol":"mmp/1","type":"hello","id":"h2","role":"reviewer","capabilities":["docs"]}"#;
    let presence = r#"{"protocol":"mmp/1","type":"presence","id":"p1","status":"busy"}"#;
    let discover = r#"{"protocol":"mmp/1","type":"discover","role":"worker","status":"busy","capabilities":["rust"]}"#;

    dispatch_mmp_body(worker_hello, &mut service, &mut worker_connection, 10);
    dispatch_mmp_body(reviewer_hello, &mut service, &mut reviewer_connection, 10);
    dispatch_mmp_body(presence, &mut service, &mut worker_connection, 20);
    let response = dispatch_mmp_body(discover, &mut service, &mut reviewer_connection, 30);

    assert!(
        response.contains(r#""type":"discover_result""#),
        "{response}"
    );
    assert!(response.contains(r#""role":"worker""#), "{response}");
    assert!(!response.contains(r#""role":"reviewer""#), "{response}");
}
