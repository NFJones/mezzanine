//! Recovery prerequisites for bounded terminal-delivery maintenance.
//!
//! Expiry must not convert an already canonical native input into failed mail.
use super::*;

/// An interruption after canonical context storage leaves the transport cursor
/// behind. Once TTL has elapsed, the ordinary sweep must still reconcile that
/// exact offered input, while an offered input without canonical commit must
/// remain unacknowledged. Neither case may fabricate a second context event.
#[test]
fn runtime_expired_peer_commit_reconciles_only_canonical_input() {
    for committed in [false, true] {
        let mut service = test_runtime_service();
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
            .control
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
        let sender = service
            .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
            .unwrap();
        if !committed {
            service.start_agent_prompt_turn("%1", "continue").unwrap();
        }
        let delivery = service
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: "expired-canonical".to_string(),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: Some(1000),
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "committed evidence".to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now_ms,
            )
            .unwrap();
        if committed {
            service.fail_next_peer_message_receive_after_context_storage_for_tests();
            assert!(service.start_agent_prompt_turn("%1", "continue").is_err());
        } else {
            service
                .control
                .message_service()
                .receive_subscribed(&recipient.agent_id, now_ms, 100)
                .unwrap();
        }
        assert!(
            service
                .control
                .message_service()
                .subscription(&recipient.agent_id)
                .unwrap()
                .last_sequence
                < delivery.sequence
        );
        assert_eq!(
            service
                .agent_turn_contexts()
                .values()
                .flat_map(|context| context.blocks())
                .filter(|block| block.label.contains("expired-canonical"))
                .count(),
            usize::from(committed)
        );
        for _ in 0..2 {
            service
                .deliver_pending_runtime_agent_messages(now_ms + 1001)
                .unwrap();
        }
        let cursor = service
            .control
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence;
        assert_eq!(cursor == delivery.sequence, committed);
        assert_eq!(
            service
                .agent_turn_contexts()
                .values()
                .flat_map(|context| context.blocks())
                .filter(|block| block.label.contains("expired-canonical"))
                .count(),
            0,
            "expired queued context is cleaned up without replay; the active control never received this input"
        );
        service
            .control
            .message_service_mut()
            .settle_delivery_outcomes_at(now_ms + 1001)
            .unwrap();
        assert_eq!(
            service
                .control
                .message_service()
                .delivery_outcomes_for(&sender.agent_id, 100)
                .unwrap()
                .is_empty(),
            committed
        );
    }
}
