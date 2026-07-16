//! Messaging transport body tests.

use super::*;

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
