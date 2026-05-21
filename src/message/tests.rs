//! Unit tests for message service delivery, MMP dispatch, and fanout behavior.

use super::{
    AgentPresenceStatus, DeliveryStatus, Envelope, MessageConnection, MessageFanoutSink,
    MessageService, Recipient, SenderIdentity, TaskResultPayload, TaskState, TaskStatusPayload,
    decode_mmp_frame, dispatch_mmp_body, encode_mmp_body, flush_message_fanout,
    flush_message_fanout_for, handle_mmp_frame, mmp_error_code, validate_message_type,
};
use crate::MezError;
use crate::error::Result;
use crate::ids::IdFactory;
use crate::ids::{AgentId, PaneId, WindowId};

/// Carries Collecting Fanout Sink state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
struct CollectingFanoutSink {
    /// Stores the frames value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    frames: Vec<(AgentId, Vec<u8>)>,
}

impl MessageFanoutSink for CollectingFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, recipient: &AgentId, frame: &[u8]) -> Result<()> {
        self.frames.push((recipient.clone(), frame.to_vec()));
        Ok(())
    }
}

/// Carries Failing Fanout Sink state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct FailingFanoutSink;

impl MessageFanoutSink for FailingFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, _recipient: &AgentId, _frame: &[u8]) -> Result<()> {
        Err(MezError::new(
            crate::error::MezErrorKind::Io,
            "fixture write failed",
        ))
    }
}

/// Runs the envelope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope(sender: SenderIdentity) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: "m1".to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender,
        recipient: Recipient::Group("session".to_string()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".to_string(),
        payload: "hello".to_string(),
        extension_fields: Vec::new(),
    }
}

/// Verifies accepts message from registered sender.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn accepts_message_from_registered_sender() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());

    let delivery = service
        .accept(&sender.agent_id.clone(), envelope(sender))
        .unwrap();

    assert!(delivery.accepted);
    assert_eq!(delivery.message_id, "m1");
    assert_eq!(delivery.sequence, 1);
    assert_eq!(delivery.queued_recipients, 1);
    assert_eq!(delivery.status, DeliveryStatus::Accepted);
}

/// Verifies that the snapshot projection retains MMP identities, presence,
/// subscriptions, queued envelopes, and accepted-message idempotency state.
#[test]
fn message_service_snapshot_round_trips_delivery_state() {
    let mut service = MessageService::default();
    let sender = service.register_agent(
        Some(PaneId::opaque("%1").unwrap()),
        Some(WindowId::opaque("@1").unwrap()),
        "writer",
        vec!["code".to_string()],
    );
    let target = service.register_agent(None, None, "reviewer", vec!["review".to_string()]);
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Busy, 5)
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.id = "snapshot-message".to_string();
    message.recipient = Recipient::Agent(target.agent_id.clone());
    message.ttl_ms = Some(1_000);
    message.extension_fields = vec![("trace".to_string(), r#"{"span":"one"}"#.to_string())];

    let delivery = service
        .accept_at(&sender.agent_id, message.clone(), 10)
        .unwrap();
    let snapshot = service.snapshot_state();
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();

    assert_eq!(snapshot.protocol, "mmp/1");
    assert_eq!(snapshot.registered_agents.len(), 2);
    assert_eq!(snapshot.retained_messages.len(), 1);
    assert_eq!(snapshot.accepted_messages.len(), 1);
    assert_eq!(
        restored
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == target.agent_id)
            .unwrap()
            .status,
        AgentPresenceStatus::Busy
    );
    let batch = restored
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].envelope.payload, "hello");
    assert_eq!(
        restored.accept_at(&sender.agent_id, message, 20).unwrap(),
        delivery
    );
}

/// Verifies that accepted message ids behave as idempotency keys at the service
/// layer. Retrying the exact same envelope returns the original delivery result
/// and does not enqueue a second copy for the recipient.
#[test]
fn duplicate_message_ids_are_idempotent_for_matching_envelopes() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());

    let first = service
        .accept_at(&sender.agent_id, message.clone(), 10)
        .unwrap();
    let second = service.accept_at(&sender.agent_id, message, 11).unwrap();
    let received = service.receive_for(&target.agent_id, 12);

    assert_eq!(second, first);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].id, "m1");
}

/// Verifies that reusing a message id for different envelope content is
/// rejected instead of being delivered as a second logical message. This keeps
/// the session-global message identity invariant enforceable at the MMP
/// boundary.
#[test]
fn conflicting_duplicate_message_ids_are_rejected() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut first = envelope(sender.clone());
    first.recipient = Recipient::Agent(target.agent_id.clone());
    let mut second = first.clone();
    second.payload = "different".to_string();

    service.accept_at(&sender.agent_id, first, 10).unwrap();
    let error = service.accept_at(&sender.agent_id, second, 11).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert_eq!(mmp_error_code(&error), "invalid_envelope");
    assert_eq!(service.receive_for(&target.agent_id, 12).len(), 1);
}

/// Verifies rejects sender spoofing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_sender_spoofing() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut spoofed = sender.clone();
    spoofed.role = Some("other".to_string());

    let error = service
        .accept(&sender.agent_id, envelope(spoofed))
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies mmp frame round trips raw json body.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_frame_round_trips_raw_json_body() {
    let encoded = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello"}"#);

    let (body, consumed) = decode_mmp_frame(&encoded, 4096).unwrap();

    assert_eq!(body, r#"{"protocol":"mmp/1","type":"hello"}"#);
    assert_eq!(consumed, encoded.len());
}

/// Verifies mmp transport registers connection and discovers agents.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_registers_connection_and_discovers_agents() {
    let mut service = MessageService::default();
    let mut connection = MessageConnection::default();
    let encoded = encode_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default","capabilities":["search"]}"#,
    );

    let (response, consumed) =
        handle_mmp_frame(&encoded, 4096, &mut service, &mut connection, 10).unwrap();
    let (body, _) = decode_mmp_frame(&response, 4096).unwrap();

    assert_eq!(consumed, encoded.len());
    assert!(body.contains(r#""type":"welcome""#));
    assert!(body.contains(r#""agent_id":"a1""#));
    assert!(connection.agent_id.is_some());

    let discover = encode_mmp_body(r#"{"protocol":"mmp/1","type":"discover"}"#);
    let (response, _) =
        handle_mmp_frame(&discover, 4096, &mut service, &mut connection, 11).unwrap();
    let (body, _) = decode_mmp_frame(&response, 4096).unwrap();
    assert!(body.contains(r#""type":"discover_result""#));
    assert!(body.contains(r#""capabilities":["search"]"#));
}

/// Verifies that registration metadata is validated as JSON instead of being
/// scraped permissively from the body. Invalid roles or capability arrays must
/// fail before they become agent identity state.
#[test]
fn mmp_transport_rejects_invalid_hello_role_and_capabilities() {
    let cases = [
        (
            r#"{"protocol":"mmp/1","type":"hello","role":""}"#,
            "role is invalid",
        ),
        (
            r#"{"protocol":"mmp/1","type":"hello","role":"default","capabilities":"search"}"#,
            "capabilities must be an array",
        ),
        (
            r#"{"protocol":"mmp/1","type":"hello","role":"default","capabilities":["search",3]}"#,
            "capabilities entry is invalid",
        ),
        (
            r#"{"protocol":"mmp/1","type":"hello","role":"default","capabilities":[""]}"#,
            "capabilities is invalid",
        ),
    ];

    for (body, expected) in cases {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();

        let response = dispatch_mmp_body(body, &mut service, &mut connection, 10);

        assert!(
            response.contains(r#""code":"invalid_envelope""#),
            "{response}"
        );
        assert!(response.contains(expected), "{response}");
        assert!(connection.agent_id.is_none());
        assert!(service.discover_agents().is_empty());
    }
}

/// Verifies that discovery filters use the same strict role and capability
/// validation as registration. A malformed discovery query should fail with a
/// protocol error instead of silently broadening or changing the filter.
#[test]
fn mmp_transport_rejects_invalid_discover_filters() {
    let cases = [
        (
            r#"{"protocol":"mmp/1","type":"discover","role":""}"#,
            "role is invalid",
        ),
        (
            r#"{"protocol":"mmp/1","type":"discover","capabilities":["rust",false]}"#,
            "capabilities entry is invalid",
        ),
    ];

    for (body, expected) in cases {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        let _ = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
            &mut service,
            &mut connection,
            10,
        );

        let response = dispatch_mmp_body(body, &mut service, &mut connection, 11);

        assert!(
            response.contains(r#""code":"invalid_envelope""#),
            "{response}"
        );
        assert!(response.contains(expected), "{response}");
    }
}

/// Verifies mmp transport accepts send from registered connection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_accepts_send_from_registered_connection() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );

    let (response, _) = handle_mmp_frame(
        &encode_mmp_body(&body),
        4096,
        &mut service,
        &mut sender_connection,
        12,
    )
    .unwrap();
    let (response_body, _) = decode_mmp_frame(&response, 4096).unwrap();

    assert!(response_body.contains(r#""type":"ack""#));
    assert!(response_body.contains(r#""queued_recipients":1"#));
    let received = service.receive_for(&target.agent_id, 13);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload, "hello");
}

/// Verifies that MMP transport retries with the same message id and identical
/// envelope content are idempotent. The sender sees the same accepted status
/// shape, while the recipient receives only one logical message.
#[test]
fn mmp_transport_deduplicates_retried_message_ids() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let mut target_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"target"}"#,
        &mut service,
        &mut target_connection,
        11,
    );
    let target_id = target_connection.agent_id.clone().unwrap();
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let send_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target_id
    );

    let first_response = dispatch_mmp_body(&send_body, &mut service, &mut sender_connection, 12);
    let second_response = dispatch_mmp_body(&send_body, &mut service, &mut sender_connection, 13);
    let receive_response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":10}"#,
        &mut service,
        &mut target_connection,
        14,
    );
    let value: serde_json::Value = serde_json::from_str(&receive_response).unwrap();

    assert!(
        first_response.contains(r#""type":"ack""#),
        "{first_response}"
    );
    assert_eq!(second_response, first_response);
    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["messages"][0]["envelope"]["id"],
        serde_json::json!("m1")
    );
}

/// Verifies that transport dispatch rejects reuse of a message id when the
/// envelope content changes. This protects recipients from seeing one
/// idempotency key attached to multiple distinct payloads.
#[test]
fn mmp_transport_rejects_conflicting_duplicate_message_ids() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "target", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let first_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );
    let second_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"different"}}"#,
        sender_id, target.agent_id
    );

    let _ = dispatch_mmp_body(&first_body, &mut service, &mut sender_connection, 12);
    let response = dispatch_mmp_body(&second_body, &mut service, &mut sender_connection, 13);

    assert!(response.contains(r#""type":"error""#), "{response}");
    assert!(
        response.contains(r#""code":"invalid_envelope""#),
        "{response}"
    );
    assert!(response.contains("message id"), "{response}");
    assert_eq!(service.receive_for(&target.agent_id, 14).len(), 1);
}

/// Verifies mmp transport rejects explicit sender spoofing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_rejects_explicit_sender_spoofing() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"a999"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        target.agent_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"error""#));
    assert!(response.contains(r#""code":"unauthorized""#));
    assert!(service.receive_for(&target.agent_id, 13).is_empty());
}

/// Verifies mmp transport accepts matching explicit sender.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_accepts_matching_explicit_sender() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"ack""#));
    assert_eq!(service.receive_for(&target.agent_id, 13).len(), 1);
}

/// Verifies that MMP recipient objects identify exactly one delivery target.
/// Accepting ambiguous target objects would let malformed envelopes route by
/// parser priority instead of by the sender's explicit protocol intent.
#[test]
fn mmp_transport_rejects_recipient_with_multiple_selectors() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}","role":"worker"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(
        response.contains(r#""code":"invalid_envelope""#),
        "{response}"
    );
    assert!(
        response.contains("multiple independent selectors"),
        "{response}"
    );
    assert!(service.receive_for(&target.agent_id, 13).is_empty());
}

/// Verifies that named recipient scopes must be meaningful strings. Empty
/// role, capability, or group selectors cannot identify a valid local delivery
/// target and must be rejected at the transport boundary.
#[test]
fn mmp_transport_rejects_empty_named_recipient_selector() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"group":""}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(
        response.contains(r#""code":"invalid_envelope""#),
        "{response}"
    );
    assert!(
        response.contains("recipient group is invalid"),
        "{response}"
    );
}

/// Verifies mmp transport rejects send without content type or payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_rejects_send_without_content_type_or_payload() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let missing_content_type = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"payload":"hello"}}"#,
        sender_id, target.agent_id
    );
    let missing_payload = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m2","time":"message:client-2","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8"}}"#,
        sender_id, target.agent_id
    );

    let missing_content_type_response = dispatch_mmp_body(
        &missing_content_type,
        &mut service,
        &mut sender_connection,
        12,
    );
    let missing_payload_response =
        dispatch_mmp_body(&missing_payload, &mut service, &mut sender_connection, 13);

    assert!(missing_content_type_response.contains(r#""code":"invalid_envelope""#));
    assert!(missing_content_type_response.contains(r#""delivery_status":"rejected""#));
    assert!(missing_content_type_response.contains("content_type"));
    assert!(missing_payload_response.contains(r#""code":"invalid_envelope""#));
    assert!(missing_payload_response.contains(r#""delivery_status":"rejected""#));
    assert!(missing_payload_response.contains("payload"));
    assert!(service.receive_for(&target.agent_id, 14).is_empty());
}

/// Verifies that transport send validation enforces the payload content rules
/// in the MMP envelope. Text payloads must use the documented charset form,
/// JSON payloads must contain parseable JSON, and binary payloads must declare
/// and satisfy base64 encoding before the message can be enqueued.
#[test]
fn mmp_transport_validates_payload_media_type_and_encoding() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let text_without_charset = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain","payload":"hello"}}"#,
        sender_id, target.agent_id
    );
    let invalid_json = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m2","time":"message:client-2","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"application/json","payload":"not-json"}}"#,
        sender_id, target.agent_id
    );
    let binary_without_encoding = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m3","time":"message:client-3","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"application/octet-stream","payload":"AQID"}}"#,
        sender_id, target.agent_id
    );
    let invalid_base64 = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m4","time":"message:client-4","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"application/octet-stream","payload_encoding":"base64","payload":"not base64"}}"#,
        sender_id, target.agent_id
    );

    let text_response = dispatch_mmp_body(
        &text_without_charset,
        &mut service,
        &mut sender_connection,
        12,
    );
    let json_response = dispatch_mmp_body(&invalid_json, &mut service, &mut sender_connection, 13);
    let binary_response = dispatch_mmp_body(
        &binary_without_encoding,
        &mut service,
        &mut sender_connection,
        14,
    );
    let base64_response =
        dispatch_mmp_body(&invalid_base64, &mut service, &mut sender_connection, 15);

    assert!(text_response.contains(r#""code":"invalid_envelope""#));
    assert!(text_response.contains("text/plain; charset=utf-8"));
    assert!(json_response.contains(r#""code":"invalid_envelope""#));
    assert!(json_response.contains("valid JSON"));
    assert!(binary_response.contains(r#""code":"invalid_envelope""#));
    assert!(binary_response.contains("payload_encoding base64"));
    assert!(base64_response.contains(r#""code":"invalid_envelope""#));
    assert!(base64_response.contains("valid base64"));
    assert!(service.receive_for(&target.agent_id, 16).is_empty());
}

/// Verifies that `task_status` messages carry the structured task payload
/// required by the baseline MMP task channel before the envelope is accepted
/// for delivery.
#[test]
fn mmp_transport_validates_task_status_payload() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let valid = format!(
        r#"{{"protocol":"mmp/1","type":"task_status","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","state":"running","progress_percent":25,"summary":"working"}}}}"#,
        sender_id, target.agent_id
    );
    let invalid_state = format!(
        r#"{{"protocol":"mmp/1","type":"task_status","id":"m2","time":"message:client-2","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","state":"paused","progress_percent":25,"summary":"working"}}}}"#,
        sender_id, target.agent_id
    );
    let invalid_progress = format!(
        r#"{{"protocol":"mmp/1","type":"task_status","id":"m3","time":"message:client-3","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","state":"running","progress_percent":101,"summary":"working"}}}}"#,
        sender_id, target.agent_id
    );
    let missing_task_id = format!(
        r#"{{"protocol":"mmp/1","type":"task_status","id":"m4","time":"message:client-4","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"state":"running","summary":"working"}}}}"#,
        sender_id, target.agent_id
    );

    let valid_response = dispatch_mmp_body(&valid, &mut service, &mut sender_connection, 12);
    let invalid_state_response =
        dispatch_mmp_body(&invalid_state, &mut service, &mut sender_connection, 13);
    let invalid_progress_response =
        dispatch_mmp_body(&invalid_progress, &mut service, &mut sender_connection, 14);
    let missing_task_id_response =
        dispatch_mmp_body(&missing_task_id, &mut service, &mut sender_connection, 15);

    assert!(
        valid_response.contains(r#""type":"ack""#),
        "{valid_response}"
    );
    assert!(
        invalid_state_response.contains(r#""code":"invalid_envelope""#),
        "{invalid_state_response}"
    );
    assert!(
        invalid_state_response.contains("unsupported task state"),
        "{invalid_state_response}"
    );
    assert!(
        invalid_progress_response.contains("progress_percent"),
        "{invalid_progress_response}"
    );
    assert!(
        missing_task_id_response.contains("task_id"),
        "{missing_task_id_response}"
    );
    let received = service.receive_for(&target.agent_id, 16);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].message_type, "task_status");
}

/// Verifies that `task_result` transport envelopes require the completion
/// payload fields used by recipients to distinguish successful and failed task
/// completion.
#[test]
fn mmp_transport_validates_task_result_payload() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let valid = format!(
        r#"{{"protocol":"mmp/1","type":"task_result","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","success":true,"summary":"done","output":"ok"}}}}"#,
        sender_id, target.agent_id
    );
    let invalid_success = format!(
        r#"{{"protocol":"mmp/1","type":"task_result","id":"m2","time":"message:client-2","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","success":"yes","summary":"done","output":"ok"}}}}"#,
        sender_id, target.agent_id
    );
    let missing_output = format!(
        r#"{{"protocol":"mmp/1","type":"task_result","id":"m3","time":"message:client-3","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"application/json","payload":{{"task_id":"task-1","success":true,"summary":"done"}}}}"#,
        sender_id, target.agent_id
    );
    let text_content_type = format!(
        r#"{{"protocol":"mmp/1","type":"task_result","id":"m4","time":"message:client-4","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":"task-1","ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"done"}}"#,
        sender_id, target.agent_id
    );

    let valid_response = dispatch_mmp_body(&valid, &mut service, &mut sender_connection, 12);
    let invalid_success_response =
        dispatch_mmp_body(&invalid_success, &mut service, &mut sender_connection, 13);
    let missing_output_response =
        dispatch_mmp_body(&missing_output, &mut service, &mut sender_connection, 14);
    let text_content_type_response =
        dispatch_mmp_body(&text_content_type, &mut service, &mut sender_connection, 15);

    assert!(
        valid_response.contains(r#""type":"ack""#),
        "{valid_response}"
    );
    assert!(
        invalid_success_response.contains("boolean success"),
        "{invalid_success_response}"
    );
    assert!(
        missing_output_response.contains("output"),
        "{missing_output_response}"
    );
    assert!(
        text_content_type_response.contains("application/json"),
        "{text_content_type_response}"
    );
    let received = service.receive_for(&target.agent_id, 16);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].message_type, "task_result");
}

/// Verifies mmp transport rejects send without required envelope metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_rejects_send_without_required_envelope_metadata() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#),
        4096,
        &mut service,
        &mut sender_connection,
        10,
    )
    .unwrap();
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let cases = [
        (
            "time",
            format!(
                r#"{{"protocol":"mmp/1","type":"send","id":"m1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
                sender_id, target.agent_id
            ),
        ),
        (
            "sender",
            format!(
                r#"{{"protocol":"mmp/1","type":"send","id":"m2","time":"message:client-2","recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
                target.agent_id
            ),
        ),
        (
            "correlation_id",
            format!(
                r#"{{"protocol":"mmp/1","type":"send","id":"m3","time":"message:client-3","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
                sender_id, target.agent_id
            ),
        ),
        (
            "ttl_ms",
            format!(
                r#"{{"protocol":"mmp/1","type":"send","id":"m4","time":"message:client-4","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
                sender_id, target.agent_id
            ),
        ),
    ];

    for (field, body) in cases {
        let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

        assert!(
            response.contains(r#""code":"invalid_envelope""#),
            "{response}"
        );
        assert!(response.contains(field), "{response}");
    }
    assert!(service.receive_for(&target.agent_id, 13).is_empty());
}

/// Verifies mmp transport rejects send before hello.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_rejects_send_before_hello() {
    let mut service = MessageService::default();
    let mut connection = MessageConnection::default();
    let response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"send","id":"m1","recipient":{"session":true},"payload":"hello"}"#,
        &mut service,
        &mut connection,
        10,
    );

    assert!(response.contains(r#""type":"error""#));
    assert!(response.contains(r#""code":"unauthorized""#));
}

/// Verifies mmp transport reports unsupported protocol.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_reports_unsupported_protocol() {
    let mut service = MessageService::default();
    let mut connection = MessageConnection::default();

    let response = dispatch_mmp_body(
        r#"{"protocol":"mmp/2","type":"hello"}"#,
        &mut service,
        &mut connection,
        10,
    );

    assert!(response.contains(r#""code":"unsupported_protocol""#));
    assert!(!response.contains(r#""code":"invalid_envelope""#));
}

/// Verifies mmp accept maps unsupported protocol to protocol error code.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_accept_maps_unsupported_protocol_to_protocol_error_code() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    message.protocol = "mmp/2";

    let error = service.accept(&sender.agent_id, message).unwrap_err();

    assert_eq!(mmp_error_code(&error), "unsupported_protocol");
}

/// Verifies mmp transport rejects bare receive message type.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_rejects_bare_receive_message_type() {
    let mut service = MessageService::default();
    let mut connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello"}"#,
        &mut service,
        &mut connection,
        10,
    );

    let response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"receive"}"#,
        &mut service,
        &mut connection,
        11,
    );

    assert!(response.contains(r#""type":"error""#));
    assert!(response.contains(r#""code":"invalid_envelope""#));
}

/// Verifies that receive limits are validated instead of silently falling back
/// to the default batch size. Malformed limits are protocol errors because they
/// can otherwise hide caller bugs and make pagination behavior unpredictable.
#[test]
fn mmp_transport_rejects_invalid_receive_limit() {
    let cases = [
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":"bad"}"#,
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":-1}"#,
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":[]}"#,
    ];

    for body in cases {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        let _ = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello"}"#,
            &mut service,
            &mut connection,
            10,
        );

        let response = dispatch_mmp_body(body, &mut service, &mut connection, 11);

        assert!(
            response.contains(r#""code":"invalid_envelope""#),
            "{response}"
        );
        assert!(response.contains("limit"), "{response}");
    }
}

/// Verifies mmp transport delivers envelope with monotonic metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_delivers_envelope_with_monotonic_metadata() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let mut target_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"target"}"#,
        &mut service,
        &mut target_connection,
        11,
    );
    let target_id = target_connection.agent_id.clone().unwrap();
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let send_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target_id
    );
    let _ = dispatch_mmp_body(&send_body, &mut service, &mut sender_connection, 12);

    let response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":10}"#,
        &mut service,
        &mut target_connection,
        13,
    );

    assert!(response.contains(r#""type":"deliver""#));
    assert!(response.contains(
        r#""envelope":{"protocol":"mmp/1","id":"m1","type":"send","sequence":1,"time":"message:client-1""#
    ));
}

/// Verifies mmp transport preserves unknown envelope fields on delivery.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mmp_transport_preserves_unknown_envelope_fields_on_delivery() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let mut target_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"target"}"#,
        &mut service,
        &mut target_connection,
        11,
    );
    let target_id = target_connection.agent_id.clone().unwrap();
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let send_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","sequence":999,"time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello","trace":{{"span":"s1","sampled":true}},"priority":3}}"#,
        sender_id, target_id
    );
    let _ = dispatch_mmp_body(&send_body, &mut service, &mut sender_connection, 12);

    let response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":10}"#,
        &mut service,
        &mut target_connection,
        13,
    );
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    let envelope = &value["messages"][0]["envelope"];

    assert_eq!(envelope["sequence"], serde_json::json!(1));
    assert_eq!(envelope["trace"]["span"], serde_json::json!("s1"));
    assert_eq!(envelope["trace"]["sampled"], serde_json::json!(true));
    assert_eq!(envelope["priority"], serde_json::json!(3));
}

/// Verifies that a binary payload with an explicit base64 declaration is
/// accepted and delivered with the payload encoding preserved. The recipient
/// needs that envelope metadata to decode bytes without guessing from the media
/// type alone.
#[test]
fn mmp_transport_delivers_base64_binary_payload_with_encoding() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let mut target_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"target"}"#,
        &mut service,
        &mut target_connection,
        11,
    );
    let target_id = target_connection.agent_id.clone().unwrap();
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let send_body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"application/octet-stream","payload_encoding":"base64","payload":"AQIDBA=="}}"#,
        sender_id, target_id
    );

    let send_response = dispatch_mmp_body(&send_body, &mut service, &mut sender_connection, 12);
    let receive_response = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"mmp.receive","limit":10}"#,
        &mut service,
        &mut target_connection,
        13,
    );
    let value: serde_json::Value = serde_json::from_str(&receive_response).unwrap();
    let envelope = &value["messages"][0]["envelope"];

    assert!(send_response.contains(r#""type":"ack""#), "{send_response}");
    assert_eq!(
        envelope["content_type"],
        serde_json::json!("application/octet-stream")
    );
    assert_eq!(envelope["payload_encoding"], serde_json::json!("base64"));
    assert_eq!(envelope["payload"], serde_json::json!("AQIDBA=="));
}

/// Verifies message type validation requires namespaced extensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn message_type_validation_requires_namespaced_extensions() {
    assert!(validate_message_type("receive").is_err());
    assert!(validate_message_type("receive/ack").is_err());
    assert!(validate_message_type("com.example.receive").is_ok());
    assert!(validate_message_type("com.example/receive").is_ok());
    assert!(validate_message_type("urn:example:receive").is_ok());
}

/// Verifies discovers registered agents in stable order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn discovers_registered_agents_in_stable_order() {
    let mut service = MessageService::default();
    let first = service.register_agent(None, None, "default", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());

    let agents = service.discover_agents();

    assert_eq!(agents[0].agent_id, first.agent_id);
    assert_eq!(agents[1].agent_id, second.agent_id);
}

/// Verifies delivers direct and role messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn delivers_direct_and_role_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let reviewer = service.register_agent(None, None, "reviewer", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Role("reviewer".to_string());

    service.accept_at(&sender.agent_id, message, 10).unwrap();

    let received = service.receive_for(&reviewer.agent_id, 11);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload, "hello");
    assert!(service.receive_for(&sender.agent_id, 11).is_empty());
}

/// Verifies delivers to pane window and capability recipients.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn delivers_to_pane_window_and_capability_recipients() {
    let mut ids = IdFactory::default();
    let pane = ids.pane();
    let window = ids.window();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(
        Some(pane.clone()),
        Some(window.clone()),
        "worker",
        vec!["search".to_string()],
    );

    let mut pane_message = envelope(sender.clone());
    pane_message.id = "pane".to_string();
    pane_message.recipient = Recipient::Pane(pane);
    service
        .accept_at(&sender.agent_id, pane_message, 10)
        .unwrap();

    let mut window_message = envelope(sender.clone());
    window_message.id = "window".to_string();
    window_message.recipient = Recipient::Window(window.clone());
    service
        .accept_at(&sender.agent_id, window_message, 11)
        .unwrap();
    assert_eq!(service.queued_window_message_count(&window), 1);
    assert_eq!(service.queued_window_message_count(&ids.window()), 0);

    let mut capability_message = envelope(sender.clone());
    capability_message.id = "capability".to_string();
    capability_message.recipient = Recipient::Capability("search".to_string());
    service
        .accept_at(&sender.agent_id, capability_message, 12)
        .unwrap();

    let received = service.receive_for(&target.agent_id, 13);

    assert_eq!(
        received
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        vec!["pane", "window", "capability"]
    );
}

/// Verifies subscribers receive only their own visible messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn subscribers_receive_only_their_own_visible_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut first_message = envelope(sender.clone());
    first_message.id = "to-first".to_string();
    first_message.recipient = Recipient::Agent(first.agent_id.clone());
    let first_delivery = service
        .accept_at(&sender.agent_id, first_message, 10)
        .unwrap();

    let mut second_message = envelope(sender.clone());
    second_message.id = "to-second".to_string();
    second_message.recipient = Recipient::Agent(second.agent_id.clone());
    let second_delivery = service
        .accept_at(&sender.agent_id, second_message, 11)
        .unwrap();

    let first_batch = service
        .receive_subscribed(&first.agent_id, 12, usize::MAX)
        .unwrap();
    let second_batch = service
        .receive_subscribed(&second.agent_id, 12, usize::MAX)
        .unwrap();

    assert_eq!(first_delivery.sequence, 1);
    assert_eq!(second_delivery.sequence, 2);
    assert_eq!(first_batch.messages.len(), 1);
    assert_eq!(first_batch.messages[0].envelope.id, "to-first");
    assert_eq!(first_batch.messages[0].sequence, first_delivery.sequence);
    assert_eq!(second_batch.messages.len(), 1);
    assert_eq!(second_batch.messages[0].envelope.id, "to-second");
    assert_eq!(second_batch.messages[0].sequence, second_delivery.sequence);
}

/// Verifies subscribed delivery excludes expired messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn subscribed_delivery_excludes_expired_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();

    let mut expired = envelope(sender.clone());
    expired.id = "expired".to_string();
    expired.recipient = Recipient::Agent(target.agent_id.clone());
    expired.ttl_ms = Some(5);
    let expired_delivery = service
        .accept_at(&sender.agent_id, expired.clone(), 10)
        .unwrap();

    let mut live = envelope(sender.clone());
    live.id = "live".to_string();
    live.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, live, 11).unwrap();

    let batch = service
        .receive_subscribed(&target.agent_id, 16, usize::MAX)
        .unwrap();

    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].envelope.id, "live");
    let expired_retry = service.accept_at(&sender.agent_id, expired, 16).unwrap();
    assert_eq!(expired_retry.sequence, expired_delivery.sequence);
    assert_eq!(expired_retry.status, DeliveryStatus::Expired);
}

/// Verifies cursor advance limits delivery to newer messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn cursor_advance_limits_delivery_to_newer_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();

    let mut first = envelope(sender.clone());
    first.id = "first".to_string();
    first.recipient = Recipient::Agent(target.agent_id.clone());
    let first_delivery = service.accept_at(&sender.agent_id, first, 10).unwrap();

    let mut second = envelope(sender.clone());
    second.id = "second".to_string();
    second.recipient = Recipient::Agent(target.agent_id.clone());
    let second_delivery = service.accept_at(&sender.agent_id, second, 11).unwrap();

    let initial = service
        .receive_subscribed(&target.agent_id, 12, usize::MAX)
        .unwrap();
    assert_eq!(
        initial
            .messages
            .iter()
            .map(|message| message.envelope.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    service
        .advance_subscription(&target.agent_id, first_delivery.sequence)
        .unwrap();
    let after_first_ack = service
        .receive_subscribed(&target.agent_id, 13, usize::MAX)
        .unwrap();
    assert_eq!(after_first_ack.messages.len(), 1);
    assert_eq!(after_first_ack.messages[0].envelope.id, "second");

    service
        .advance_subscription(&target.agent_id, second_delivery.sequence)
        .unwrap();
    assert!(
        service
            .receive_subscribed(&target.agent_id, 14, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies cursor advance retains messages for other subscribers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn cursor_advance_retains_messages_for_other_subscribers() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    let delivery = service.accept_at(&sender.agent_id, fanout, 10).unwrap();

    service
        .advance_subscription(&first.agent_id, delivery.sequence)
        .unwrap();

    assert!(
        service
            .receive_subscribed(&first.agent_id, 11, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
    let second_batch = service
        .receive_subscribed(&second.agent_id, 11, usize::MAX)
        .unwrap();
    assert_eq!(second_batch.messages.len(), 1);
    assert_eq!(second_batch.messages[0].envelope.id, "fanout");
    assert_eq!(second_batch.messages[0].sequence, delivery.sequence);
}

/// Verifies fanout ready batches subscribed recipients without advancing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn fanout_ready_batches_subscribed_recipients_without_advancing() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&second.agent_id).unwrap();
    service.subscribe(&first.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    service.accept_at(&sender.agent_id, fanout, 10).unwrap();

    let ready = service.fanout_ready(11, 10);

    assert_eq!(
        ready
            .iter()
            .map(|batch| batch.recipient.as_str())
            .collect::<Vec<_>>(),
        vec![first.agent_id.as_str(), second.agent_id.as_str()]
    );
    assert_eq!(ready[0].batch.messages[0].envelope.id, "fanout");
    assert_eq!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Verifies acknowledging fanout batch advances only that recipient.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn acknowledging_fanout_batch_advances_only_that_recipient() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    let delivery = service.accept_at(&sender.agent_id, fanout, 10).unwrap();
    let ready = service.fanout_ready(11, 10);

    service.acknowledge_fanout_batch(&ready[0]).unwrap();

    assert!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
    let second_batch = service
        .receive_subscribed(&second.agent_id, 12, usize::MAX)
        .unwrap();
    assert_eq!(second_batch.messages[0].sequence, delivery.sequence);
}

/// Verifies flush message fanout writes frames and advances cursors.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn flush_message_fanout_writes_frames_and_advances_cursors() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, message, 10).unwrap();
    let mut sink = CollectingFanoutSink::default();

    let sent = flush_message_fanout(&mut service, 11, 10, &mut sink).unwrap();

    assert_eq!(sent, 1);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, target.agent_id);
    let (body, _) = decode_mmp_frame(&sink.frames[0].1, 4096).unwrap();
    assert!(body.contains(r#""type":"deliver""#));
    assert!(body.contains(r#""payload":"hello""#));
    assert!(body.contains(r#""envelope":{"protocol":"mmp/1""#));
    assert!(body.contains(r#""sequence":1"#));
    assert!(body.contains(r#""time":"message:test""#));
    assert!(
        service
            .receive_subscribed(&target.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies flush message fanout for writes only requested recipient.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn flush_message_fanout_for_writes_only_requested_recipient() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Session;
    service.accept_at(&sender.agent_id, message, 10).unwrap();
    let mut sink = CollectingFanoutSink::default();

    let sent = flush_message_fanout_for(&mut service, &second.agent_id, 11, 10, &mut sink).unwrap();

    assert_eq!(sent, 1);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, second.agent_id);
    assert_eq!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        service
            .receive_subscribed(&second.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies failed fanout write does not advance cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn failed_fanout_write_does_not_advance_cursor() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, message, 10).unwrap();
    let mut sink = FailingFanoutSink;

    let error = flush_message_fanout(&mut service, 11, 10, &mut sink).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Verifies undeliverable messages are rejected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn undeliverable_messages_are_rejected() {
    let mut ids = IdFactory::default();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    let _registered_id = ids.agent();
    message.recipient = Recipient::Agent(ids.agent());

    let error = service.accept(&sender.agent_id, message).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
    assert_eq!(mmp_error_code(&error), "undeliverable");
}

/// Verifies that offline presence makes a registered recipient unavailable for
/// both acceptance-time matching and later receive-time delivery. Other
/// presence states can still be addressable, but `offline` must not receive new
/// local messages.
#[test]
fn offline_recipients_are_rejected_and_not_delivered() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Offline, 10)
        .unwrap();
    let mut rejected = envelope(sender.clone());
    rejected.recipient = Recipient::Agent(target.agent_id.clone());

    let error = service
        .accept_at(&sender.agent_id, rejected, 11)
        .unwrap_err();

    assert_eq!(mmp_error_code(&error), "undeliverable");

    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Available, 12)
        .unwrap();
    let mut accepted = envelope(sender.clone());
    accepted.id = "accepted-before-offline".to_string();
    accepted.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, accepted, 13).unwrap();
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Offline, 14)
        .unwrap();

    let batch = service
        .receive_subscribed(&target.agent_id, 15, usize::MAX)
        .unwrap();

    assert!(batch.messages.is_empty());
}

/// Verifies responses can be filtered by correlation id.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn responses_can_be_filtered_by_correlation_id() {
    let mut service = MessageService::default();
    let requester = service.register_agent(None, None, "default", Vec::new());
    let responder = service.register_agent(None, None, "worker", Vec::new());
    let mut response = envelope(responder.clone());
    response.id = "response-1".to_string();
    response.recipient = Recipient::Agent(requester.agent_id.clone());
    response.correlation_id = Some("request-1".to_string());

    service
        .accept_at(&responder.agent_id, response, 10)
        .unwrap();

    let responses = service.responses_for(&requester.agent_id, "request-1", 11);

    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].id, "response-1");
}

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

/// Verifies task payloads render structured json.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn task_payloads_render_structured_json() {
    let status = TaskStatusPayload {
        task_id: "task-1".to_string(),
        state: TaskState::Running,
        progress_percent: Some(25),
        summary: "working".to_string(),
    };
    let result = TaskResultPayload {
        task_id: "task-1".to_string(),
        success: true,
        summary: "done".to_string(),
        output: "ok".to_string(),
    };

    assert!(status.to_json().contains(r#""state":"running""#));
    assert!(result.to_json().contains(r#""success":true"#));
}

/// Verifies expired messages are not delivered.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn expired_messages_are_not_delivered() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(sender.agent_id.clone());
    message.ttl_ms = Some(5);

    service.accept_at(&sender.agent_id, message, 10).unwrap();

    assert!(service.receive_for(&sender.agent_id, 16).is_empty());
}

/// Verifies that messages with an immediate TTL do not enter the delivery
/// queue. This covers the protocol case where the sender can be told that a
/// message expired before delivery instead of receiving an accepted ack for a
/// payload that can never be delivered.
#[test]
fn zero_ttl_messages_are_rejected_before_delivery() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    message.ttl_ms = Some(0);

    let error = service
        .accept_at(&sender.agent_id, message, 10)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(mmp_error_code(&error), "expired");
    assert!(service.receive_for(&target.agent_id, 10).is_empty());
}

/// Verifies queue retention evicts oldest messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn queue_retention_evicts_oldest_messages() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut first = envelope(sender.clone());
    first.id = "m1".to_string();
    let mut second = envelope(sender.clone());
    second.id = "m2".to_string();

    service.accept(&sender.agent_id, first).unwrap();
    service.accept(&sender.agent_id, second).unwrap();

    let received = service.receive_for(&sender.agent_id, 0);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].id, "m2");
}

/// Verifies that accepted-message deduplication state follows the bounded
/// retained delivery queue. Retrying a recently retained message remains
/// idempotent, but messages evicted from the replay window no longer keep a
/// cloned envelope alive indefinitely.
#[test]
fn accepted_message_retention_tracks_retained_queue() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut first = envelope(sender.clone());
    first.id = "m1".to_string();
    let mut second = envelope(sender.clone());
    second.id = "m2".to_string();

    service.accept(&sender.agent_id, first).unwrap();
    service.accept(&sender.agent_id, second).unwrap();
    let snapshot = service.snapshot_state();

    assert_eq!(snapshot.retained_messages.len(), 1);
    assert_eq!(snapshot.accepted_messages.len(), 1);
    assert_eq!(snapshot.accepted_messages[0].envelope.id, "m2");
}

/// Verifies oversized payload is rejected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn oversized_payload_is_rejected() {
    let mut service = MessageService::with_limits(10, 4);
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    message.payload = "too-large".to_string();

    let error = service.accept(&sender.agent_id, message).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert_eq!(mmp_error_code(&error), "payload_too_large");
}

/// Verifies that transport dispatch maps configured payload-size rejection to
/// the MMP `payload_too_large` error code rather than collapsing it into a
/// generic invalid-envelope response.
#[test]
fn mmp_transport_reports_payload_too_large_for_oversized_payload() {
    let mut service = MessageService::with_limits(10, 4);
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"too-large"}}"#,
        sender_id, target.agent_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"error""#), "{response}");
    assert!(
        response.contains(r#""code":"payload_too_large""#),
        "{response}"
    );
    assert!(
        service.receive_for(&target.agent_id, 13).is_empty(),
        "{response}"
    );
}

/// Verifies that transport dispatch maps recipient routing misses to the MMP
/// `undeliverable` error code. This keeps unavailable or unregistered
/// recipients distinguishable from unrelated `not_found` failures at the
/// protocol boundary.
#[test]
fn mmp_transport_reports_undeliverable_for_unavailable_recipient() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let missing_agent = AgentId::parse('a', "a999").unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, missing_agent
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"error""#), "{response}");
    assert!(response.contains(r#""code":"undeliverable""#), "{response}");
    assert!(
        response.contains(r#""delivery_status":"undeliverable""#),
        "{response}"
    );
    assert!(
        service.receive_for(&missing_agent, 13).is_empty(),
        "{response}"
    );
}

/// Verifies that MMP transport routing treats registered but offline agents as
/// unavailable recipients. This keeps the protocol-level `undeliverable` error
/// aligned with live presence state, not just registration existence.
#[test]
fn mmp_transport_reports_undeliverable_for_offline_recipient() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let mut target_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"worker"}"#,
        &mut service,
        &mut target_connection,
        10,
    );
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"presence","id":"p1","status":"offline"}"#,
        &mut service,
        &mut target_connection,
        11,
    );
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let target_id = target_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"error""#), "{response}");
    assert!(response.contains(r#""code":"undeliverable""#), "{response}");
    assert!(
        response.contains(r#""delivery_status":"undeliverable""#),
        "{response}"
    );
    assert!(service.receive_for(target_id, 13).is_empty(), "{response}");
}

/// Verifies that transport dispatch reports immediate TTL expiry with the MMP
/// `expired` error code. This prevents zero-TTL sends from being accepted and
/// then silently filtered before any recipient can observe them.
#[test]
fn mmp_transport_reports_expired_for_zero_ttl_payload() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":0,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );

    let response = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);

    assert!(response.contains(r#""type":"error""#), "{response}");
    assert!(response.contains(r#""code":"expired""#), "{response}");
    assert!(
        response.contains(r#""delivery_status":"expired""#),
        "{response}"
    );
    assert!(service.receive_for(&target.agent_id, 12).is_empty());
}

/// Verifies that a message accepted before its TTL elapses reports an expired
/// delivery status on an idempotent retry after expiry. The message remains
/// unavailable to recipients, but the sender can observe that the accepted
/// message aged out instead of disappearing without status.
#[test]
fn mmp_transport_reports_expired_status_for_accepted_ttl_retry() {
    let mut service = MessageService::default();
    let mut sender_connection = MessageConnection::default();
    let _ = dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        &mut service,
        &mut sender_connection,
        10,
    );
    let target = service.register_agent(None, None, "worker", Vec::new());
    let sender_id = sender_connection.agent_id.as_ref().unwrap();
    let body = format!(
        r#"{{"protocol":"mmp/1","type":"send","id":"m1","time":"message:client-1","sender":{{"agent_id":"{}","role":"default"}},"recipient":{{"agent_id":"{}"}},"correlation_id":null,"ttl_ms":5,"content_type":"text/plain; charset=utf-8","payload":"hello"}}"#,
        sender_id, target.agent_id
    );

    let accepted = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 12);
    let expired_retry = dispatch_mmp_body(&body, &mut service, &mut sender_connection, 18);

    assert!(accepted.contains(r#""status":"accepted""#), "{accepted}");
    assert!(
        expired_retry.contains(r#""status":"expired""#),
        "{expired_retry}"
    );
    assert!(service.receive_for(&target.agent_id, 18).is_empty());
}
