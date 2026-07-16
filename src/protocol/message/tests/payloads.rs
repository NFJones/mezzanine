//! Messaging payloads tests.

use super::*;

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

    assert_eq!(error.kind(), MessageErrorKind::InvalidArgs);
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
