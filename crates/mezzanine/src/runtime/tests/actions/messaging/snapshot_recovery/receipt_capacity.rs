//! Atomic receiver-outbox capacity admission and snapshot validity coverage.
//!
//! Filling the shared receipt bound must leave extra mail pending before any
//! prompt, context, or cursor mutation, without truncating recoverable evidence.

use super::*;

/// Verifies user-prompt delivery refuses the 1,025th visible receiver receipt
/// before it changes the prompt ledger, context, or durable delivery cursor.
/// The retained 1,024-entry projection must remain a valid snapshot.
#[test]
fn runtime_user_prompt_peer_receipt_outbox_capacity_leaves_extra_message_pending() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
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
    let receipt_turn = AgentTurnRecord {
        turn_id: "receipt-capacity-fill".to_string(),
        conversation_id: conversation_id.clone(),
        agent_id: recipient.agent_id.to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: current_unix_seconds(),
        deadline_at_unix_millis: now_ms.saturating_add(60_000),
        policy_profile: "runtime".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Queued,
        initial_capability: None,
    };
    for sequence in 1..=1_024 {
        let delivery = service
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: format!("receipt-fill-{sequence}"),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "retained receipt".to_string(),
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
        service.register_received_peer_message_presentation(
            recipient.agent_id.clone(),
            delivery.sequence,
            &receipt_turn,
            Envelope {
                protocol: "mmp/1",
                id: format!("receipt-fill-{sequence}"),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "retained receipt".to_string(),
                extension_fields: Vec::new(),
            },
        );
    }
    let extra = service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id.clone(),
            Envelope {
                protocol: "mmp/1",
                id: "receipt-capacity-extra".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "must remain pending".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert!(
        service
            .start_agent_prompt_turn("%1", "continue work")
            .is_err()
    );
    assert_eq!(
        service
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        1_024
    );
    let mut snapshot =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    snapshot.message_state = Some(service.message_service().snapshot_state());
    snapshot.unsettled_peer_presentations =
        service.snapshot_unsettled_received_peer_message_presentations();
    snapshot.agent_sessions = vec![crate::storage::snapshot::SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id,
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    assert_eq!(snapshot.unsettled_peer_presentations.len(), 1_024);
    snapshot.validate().unwrap();
    assert_eq!(extra.sequence, 1_025);
    service.terminate_all_pane_processes().unwrap();
}
