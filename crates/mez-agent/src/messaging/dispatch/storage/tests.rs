//! Lower protocol controls for actual reservation versus storage qualification.
//! The small trusted port tests the lower proof boundary only, not disk I/O.

use super::*;
use crate::messaging::{
    MessageAcceptanceStore, MessageAcceptanceStoreError, MessageAcceptanceWrite,
    MessageAcceptanceWriteOutcome,
};

/// One real invocation of the lower trusted qualification port, with no I/O claim.
struct Commit(usize);
impl MessageAcceptanceStore for Commit {
    /// Counts qualification only after the lower actually issues its linear input.
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.0 += 1;
        Ok(())
    }
}

/// Registered raw caller, recipient, and complete bounded original wire body.
fn fixture() -> (MessageService, MessageConnection, AgentId, String) {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "raw", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let body = serde_json::json!({
        "protocol":"mmp/1","type":"send","id":"original","time":"original-clock",
        "scope":"session","sender":{"agent_id":sender.agent_id.as_str()},
        "recipient":{"agent_id":target.agent_id.as_str()},"ttl_ms":null,
        "correlation_id":null,"content_type":"text/plain; charset=utf-8","payload":"original payload"
    })
    .to_string();
    (
        service,
        MessageConnection {
            agent_id: Some(sender.agent_id),
            delivery_cursor: None,
        },
        target.agent_id,
        body,
    )
}

/// Parsing must allocate no occurrence and preparation must publish no success.
/// Only actual trusted write completion/adoption enables the identical duplicate;
/// round-trip snapshot data cannot recreate this live storage qualification.
#[test]
fn raw_preparation_publishes_only_after_qualified_adoption_and_restore_cannot_upgrade() {
    let (mut service, connection, target, body) = fixture();
    let storage_scope = MessageStorageScope::default();
    let before = service.snapshot_state();
    let request = parse_mmp_storage_request(&body, &service, &connection)
        .unwrap()
        .unwrap();
    assert_eq!(service.snapshot_state(), before);
    assert!(!format!("{request:?}").contains("original payload"));
    let MmpStoragePreparation::Acceptance(ticket) = request
        .prepare(&mut service, &storage_scope, 20, 4096, 512 * 1024)
        .unwrap()
    else {
        panic!("fresh send must reserve")
    };
    assert!(
        service
            .receive_subscribed(&target, 20, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let mut port = Commit(0);
    let MessageAcceptanceWriteOutcome::Committed(commit) = service
        .begin_storage_acceptance_write(&ticket, &storage_scope)
        .unwrap()
        .persist(&mut port)
    else {
        panic!("port actually commits")
    };
    assert_eq!(port.0, 1);
    let delivery = service.publish_acceptance(&commit).unwrap().0;
    let MmpStoragePreparation::Accepted(duplicate) =
        parse_mmp_storage_request(&body, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
            .unwrap()
    else {
        panic!("exact live replay")
    };
    assert_eq!(duplicate, delivery);
    let foreign_scope = MessageStorageScope::default();
    assert_ne!(storage_scope, foreign_scope);
    assert!(
        parse_mmp_storage_request(&body, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &foreign_scope, 30, 4096, 512 * 1024)
            .is_err()
    );
    assert_eq!(port.0, 1);
    let snapshot = service.snapshot_state();
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    let error = parse_mmp_storage_request(&body, &restored, &connection)
        .unwrap()
        .unwrap()
        .prepare(&mut restored, &storage_scope, 30, 4096, 512 * 1024)
        .unwrap_err();
    assert!(error.message().contains("qualification"));
    assert_eq!(restored.snapshot_state(), snapshot);
}

/// A matching memory-only receipt cannot acquire storage success by entering the
/// prepared adapter. Changed immutable content and forged senders also reject;
/// none can reserve a replacement or mutate accepted/recipient state.
#[test]
fn raw_storage_replay_rejects_memory_receipts_changed_bodies_and_foreign_senders() {
    let (mut service, mut connection, _, body) = fixture();
    let storage_scope = MessageStorageScope::default();
    let response = dispatch_mmp_body(&body, &mut service, &mut connection, 20);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["type"],
        "ack"
    );
    let before = service.snapshot_state();
    let error = parse_mmp_storage_request(&body, &service, &connection)
        .unwrap()
        .unwrap()
        .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
        .unwrap_err();
    assert!(error.message().contains("qualification"));
    let changed = body.replace("original payload", "changed payload");
    assert!(
        parse_mmp_storage_request(&changed, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
            .is_err()
    );
    let foreign = body.replace(connection.agent_id.as_ref().unwrap().as_str(), "foreign");
    assert!(parse_mmp_storage_request(&foreign, &service, &connection).is_err());
    assert_eq!(service.snapshot_state(), before);
    assert!(!service.has_unpublished_acceptance());
}

/// An explicit ACK must validate genuine offers and leave the cursor unchanged
/// while reserved. Cancelling this never-issued ticket permits only the normal
/// old ACK owner; an inert repeated ACK reserves no worker at all.
#[test]
fn raw_explicit_ack_prepares_original_offer_and_duplicate_is_inert() {
    let (mut service, mut sender, target, body) = fixture();
    let storage_scope = MessageStorageScope::default();
    dispatch_mmp_body(&body, &mut service, &mut sender, 20);
    let connection = MessageConnection {
        agent_id: Some(target.clone()),
        delivery_cursor: None,
    };
    let ack = r#"{"protocol":"mmp/1","type":"ack","sequence":1}"#;
    assert!(
        parse_mmp_storage_request(ack, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
            .is_err()
    );
    service.receive_subscribed(&target, 30, 10).unwrap();
    let MmpStoragePreparation::Acknowledgement(ticket) =
        parse_mmp_storage_request(ack, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
            .unwrap()
    else {
        panic!("offered ACK must reserve")
    };
    assert_eq!(service.subscription(&target).unwrap().last_sequence, 0);
    service.cancel_acknowledgement(&ticket).unwrap();
    service.advance_subscription(&target, 1).unwrap();
    let MmpStoragePreparation::Acknowledged(cursor) =
        parse_mmp_storage_request(ack, &service, &connection)
            .unwrap()
            .unwrap()
            .prepare(&mut service, &storage_scope, 30, 4096, 512 * 1024)
            .unwrap()
    else {
        panic!("duplicate ACK must stay inert")
    };
    assert_eq!(cursor.last_sequence, 1);
}

/// Identity/discovery stays with its existing audited adapter, while malformed
/// headers and unsupported versions reject before storage or identity mutation.
#[test]
fn raw_storage_parser_preserves_nonstorage_dispatch_and_header_rejection() {
    let (service, connection, _, _) = fixture();
    let before = service.snapshot_state();
    for kind in [
        "hello",
        "presence",
        "discover",
        "heartbeat",
        "transport/receive",
    ] {
        let body = serde_json::json!({"protocol":"mmp/1","type":kind}).to_string();
        assert!(
            parse_mmp_storage_request(&body, &service, &connection)
                .unwrap()
                .is_none()
        );
    }
    for bad in ["{}", "not-json", r#"{"protocol":"mmp/99","type":"send"}"#] {
        assert!(parse_mmp_storage_request(bad, &service, &connection).is_err());
    }
    assert_eq!(service.snapshot_state(), before);
}

/// Raw task lifecycle frames must enter the same unpublished reservation path
/// as ordinary sends. Typed payload validation still applies; a malformed task
/// cannot be hidden by the storage adapter or allocate an accepted occurrence.
#[test]
fn raw_task_frames_share_preparation_and_preserve_typed_payload_validation() {
    let (mut service, connection, _, body) = fixture();
    let storage_scope = MessageStorageScope::default();
    for (kind, payload) in [
        (
            "task_status",
            serde_json::json!({"task_id":"child","state":"running","progress_percent":25,"summary":"working"}),
        ),
        (
            "task_result",
            serde_json::json!({"task_id":"child","success":true,"summary":"done","output":"ok"}),
        ),
    ] {
        let mut wire: serde_json::Value = serde_json::from_str(&body).unwrap();
        wire["type"] = kind.into();
        wire["content_type"] = "application/json".into();
        wire["payload"] = payload.to_string().into();
        let request = parse_mmp_storage_request(&wire.to_string(), &service, &connection)
            .unwrap()
            .unwrap();
        let MmpStoragePreparation::Acceptance(ticket) = request
            .prepare(&mut service, &storage_scope, 20, 4096, 512 * 1024)
            .unwrap()
        else {
            panic!("task frame must reserve")
        };
        service.cancel_acceptance(&ticket).unwrap();
        wire["payload"] = "{}".into();
        assert!(parse_mmp_storage_request(&wire.to_string(), &service, &connection).is_err());
        assert!(!service.has_unpublished_acceptance());
    }
}
