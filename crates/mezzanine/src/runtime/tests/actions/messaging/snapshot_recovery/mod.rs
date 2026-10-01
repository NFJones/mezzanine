//! Durable peer-presentation restoration and capacity regressions.
//!
//! Restore preserves exact receipt ownership and rejects over-capacity recovery
//! atomically. Legacy reconstruction uses accepted history, not live eligibility.

use super::*;

mod receipt_capacity;

mod legacy_reconstruction {
    //! Legacy receipts reconstruct acknowledged occurrences even after expiry.
    //!
    //! Reconstruction reserves capacity before installing any partial state.

    use super::*;

    /// Verifies a version-5 restore rejects 1,025 acknowledged visible retained
    /// deliveries before inserting any reconstructed receipt. Legacy payloads have
    /// no durable outbox, so reconstruction must reserve the shared receipt bound
    /// atomically rather than truncate transport replay or leave partial state.
    #[test]
    fn runtime_v5_restore_rejects_oversized_peer_receipt_reconstruction_atomically() {
        let mut service = test_runtime_service();
        let mut message_state = service.message_service().snapshot_state();
        message_state.retention_messages = 1_025;
        *service.message_service_mut() =
            MessageService::from_snapshot_state(&message_state).unwrap();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let recipient = service
            .ensure_runtime_message_identity(
                "agent-%1",
                PaneId::opaque("%1".to_string()),
                "agent",
                &[],
                now_ms,
            )
            .unwrap();
        service
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
        let sender = service
            .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
            .unwrap();
        for sequence in 1..=1_025 {
            let delivery = service
                .message_service_mut()
                .accept_at_with_scope(
                    &sender.agent_id,
                    Envelope {
                        protocol: "mmp/1",
                        id: format!("legacy-overflow-{sequence}"),
                        message_type: "send".to_string(),
                        time: format!("runtime:{now_ms}"),
                        sender: sender.clone(),
                        recipient: mez_agent::messaging::Recipient::Agent(
                            recipient.agent_id.clone(),
                        ),
                        correlation_id: None,
                        ttl_ms: None,
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: "acknowledged legacy delivery".to_string(),
                        extension_fields: Vec::new(),
                    },
                    MessageScope::Session,
                    now_ms,
                )
                .unwrap();
            service
                .message_service_mut()
                .advance_subscription(&recipient.agent_id, delivery.sequence)
                .unwrap();
        }

        let mut snapshot =
            crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
        snapshot.payload_version = 5;
        snapshot.message_state = Some(service.message_service().snapshot_state());
        assert!(snapshot.unsettled_peer_presentations.is_empty());

        service
            .restore_message_state_for_restored_snapshot(&snapshot)
            .unwrap();
        assert!(
            service
                .restore_agent_sessions_for_restored_snapshot(snapshot.payload_version < 6)
                .is_err()
        );
        assert!(
            service
                .snapshot_unsettled_received_peer_message_presentations()
                .is_empty()
        );
        service.terminate_all_pane_processes().unwrap();
    }

    /// Verifies v2-v5 receiver receipt reconstruction uses retained acceptance
    /// history rather than live eligibility. An acknowledged plaintext envelope
    /// must regain its receiver-only receipt even after its TTL expires or its
    /// recipient is unavailable, because both conditions arose after the canonical
    /// context and cursor were committed.
    #[test]
    fn runtime_v5_restore_reconstructs_expired_and_offline_peer_receipts() {
        for (case, ttl_ms, offline) in [("expired", Some(1), false), ("offline", None, true)] {
            let mut service = test_runtime_service();
            service
                .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
                .unwrap();
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            let now_ms = current_unix_seconds().saturating_mul(1000);
            let recipient = service
                .ensure_runtime_message_identity(
                    "agent-%1",
                    PaneId::opaque("%1".to_string()),
                    "agent",
                    &[],
                    now_ms,
                )
                .unwrap();
            service
                .message_service_mut()
                .subscribe_from_retained_start(&recipient.agent_id)
                .unwrap();
            let sender = service
                .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
                .unwrap();
            let message_id = format!("legacy-{case}-receipt");
            let sender_agent_id = sender.agent_id.clone();
            let delivery = service
                .message_service_mut()
                .accept_at_with_scope(
                    &sender_agent_id,
                    Envelope {
                        protocol: "mmp/1",
                        id: message_id.clone(),
                        message_type: "send".to_string(),
                        time: format!("runtime:{now_ms}"),
                        sender,
                        recipient: mez_agent::messaging::Recipient::Agent(
                            recipient.agent_id.clone(),
                        ),
                        correlation_id: None,
                        ttl_ms,
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: format!("legacy {case} receipt"),
                        extension_fields: Vec::new(),
                    },
                    MessageScope::Session,
                    now_ms,
                )
                .unwrap();
            service
                .message_service_mut()
                .advance_subscription(&recipient.agent_id, delivery.sequence)
                .unwrap();
            if offline {
                service
                    .message_service_mut()
                    .update_presence(
                        &recipient.agent_id,
                        mez_agent::messaging::AgentPresenceStatus::Offline,
                        now_ms.saturating_add(2),
                    )
                    .unwrap();
            }

            let mut snapshot =
                crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
            snapshot.payload_version = 5;
            snapshot.message_state = Some(service.message_service().snapshot_state());
            service
                .restore_message_state_for_restored_snapshot(&snapshot)
                .unwrap();
            service
                .restore_agent_sessions_for_restored_snapshot(true)
                .unwrap();
            assert!(
                service
                    .snapshot_unsettled_received_peer_message_presentations()
                    .iter()
                    .any(|receipt| receipt.identity
                        == format!(
                            "peer-message recipient={} sequence={} id={message_id}",
                            recipient.agent_id, delivery.sequence
                        ))
            );
            service.terminate_all_pane_processes().unwrap();
        }
    }
}

mod durable_settlement;
