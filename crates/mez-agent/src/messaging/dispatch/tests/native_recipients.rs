//! Raw discovery-to-direct-address interoperability for registered opaque IDs.
//!
//! Hello establishes the generated sender binding. Project membership and the
//! native registration are trusted local fixture setup, never envelope claims.
//! Every send crosses the real raw body parser; no service-accept shortcut or
//! pane/display-label fallback can satisfy the accepted-delivery oracle.

use super::*;
use crate::messaging::{ProjectScopeId, Recipient, SenderIdentity};
use mez_core::ids::{AgentId, StableId};

mod boundaries;

/// Builds a generated raw sender and an independently registered native target
/// in one trusted project. Subscribe before acceptance so the initial cursor is
/// evidence rather than a post-send subscription that could skip the envelope.
fn native_raw_fixture() -> (MessageService, MessageConnection, SenderIdentity) {
    native_raw_fixture_with_service(MessageService::default())
}

/// Uses the same trusted setup with an explicitly bounded message service.
fn native_raw_fixture_with_service(
    mut service: MessageService,
) -> (MessageService, MessageConnection, SenderIdentity) {
    let mut connection = MessageConnection::default();
    let welcome: serde_json::Value = serde_json::from_str(&dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"raw-fixture"}"#,
        &mut service,
        &mut connection,
        10,
    ))
    .unwrap();
    assert_eq!(welcome["type"], "welcome");
    let sender = connection.agent_id.as_ref().unwrap();
    assert!(StableId::parse('a', sender.as_str()).is_some());
    assert_eq!(welcome["identity"]["agent_id"], sender.as_str());
    let project = ProjectScopeId::from_canonical_root_bytes(b"/fixture/raw-native-alpha");
    service
        .rebind_agent_project_scope(sender, Some(project.clone()))
        .unwrap();
    let native = service
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                project_scope: Some(project),
                pane_id: Some(StableId::new('%', 1)),
                window_id: Some(StableId::new('@', 1)),
                role: Some("native-fixture".into()),
                capabilities: vec!["agent-harness".into(), "fixture-review".into()],
                objective: None,
            },
            10,
        )
        .unwrap();
    service
        .subscribe_from_retained_start(&native.agent_id)
        .unwrap();
    (service, connection, native)
}

/// A raw caller must be able to use precisely the native ID advertised by its
/// project-scoped discovery result. Parser rejection after successful setup is
/// the causal failure; success requires one accepted envelope, canonical sender
/// and recipient, no invented identities/subscriptions, and explicit cursor
/// advancement rather than pretending a non-consuming receive acknowledges it.
#[test]
fn raw_discovery_native_recipient_round_trip() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let discovery: serde_json::Value = serde_json::from_str(&dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"discover","role":"native-fixture"}"#,
        &mut service,
        &mut connection,
        20,
    ))
    .unwrap();
    assert_eq!(discovery["type"], "discover_result");
    assert_eq!(discovery["scope"], "project");
    let agents = discovery["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    let discovered = agents[0]["agent_id"].as_str().unwrap();
    assert_eq!(discovered, native.agent_id.as_str());
    let sender = service
        .registered_identity(connection.agent_id.as_ref().unwrap())
        .unwrap()
        .clone();
    let request = serde_json::json!({
        "protocol": "mmp/1", "id": "raw-native-discovered", "type": "send", "time": "fixture:20",
        "sender": { "agent_id": sender.agent_id.as_str() },
        "recipient": { "agent_id": discovered },
        "correlation_id": null, "ttl_ms": null,
        "content_type": "text/plain; charset=utf-8", "payload": "isolated raw native evidence",
    })
    .to_string();
    let before = service.snapshot_state();
    let response: serde_json::Value = serde_json::from_str(&dispatch_mmp_body(
        &request,
        &mut service,
        &mut connection,
        21,
    ))
    .unwrap();
    assert_eq!(
        response["type"], "ack",
        "discovered exact ID must address the native recipient: {response}"
    );
    assert_eq!(response["status"], "accepted");
    assert_eq!(response["message_id"], "raw-native-discovered");
    assert_eq!(response["queued_recipients"], 1);
    let after = service.snapshot_state();
    assert_eq!(after.next_sequence, before.next_sequence + 1);
    assert_eq!(after.registered_agents, before.registered_agents);
    assert_eq!(after.subscriptions, before.subscriptions);
    let batch = service
        .receive_subscribed(&native.agent_id, 22, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(
        service
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let message = &batch.messages[0];
    assert_eq!(message.envelope.id, "raw-native-discovered");
    assert_eq!(message.envelope.sender, sender);
    assert_eq!(
        message.envelope.recipient,
        Recipient::Agent(native.agent_id.clone())
    );
    assert_eq!(message.envelope.payload, "isolated raw native evidence");
    let duplicate: serde_json::Value = serde_json::from_str(&dispatch_mmp_body(
        &request,
        &mut service,
        &mut connection,
        23,
    ))
    .unwrap();
    assert_eq!(duplicate["type"], "ack");
    assert_eq!(duplicate["status"], response["status"]);
    assert_eq!(duplicate["message_id"], response["message_id"]);
    assert_eq!(
        duplicate["queued_recipients"],
        response["queued_recipients"]
    );
    assert_eq!(service.snapshot_state().next_sequence, after.next_sequence);
    assert_eq!(
        service
            .receive_subscribed(&native.agent_id, 24, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    service
        .advance_subscription(&native.agent_id, message.sequence)
        .unwrap();
    assert!(
        service
            .receive_subscribed(&native.agent_id, 25, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}
