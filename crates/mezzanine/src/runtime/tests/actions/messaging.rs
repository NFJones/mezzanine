//! Runtime tests for actions messaging behavior.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::runtime::ControlIdempotencyCache;
use crate::runtime::{current_unix_millis, current_unix_seconds};
use mez_agent::messaging::{Envelope, MessageScope, MessageService};
use mez_core::ids::PaneId;

/// Verifies a message accepted while a turn is active becomes one canonical
/// reference event at its actor-observed arrival point.
///
/// The delivery cursor must advance only after the event append succeeds, a
/// repeated fanout pass must not duplicate the event, and the message must
/// remain later than the prompt that causally preceded its arrival.
#[test]
fn runtime_active_turn_local_message_commits_once_at_arrival() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the current implementation")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "active-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
        correlation_id: Some(started.turn_id.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "new evidence arrived".to_string(),
        extension_fields: Vec::new(),
    };
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now_ms)
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );

    let context = service.agent_turn_contexts().get(&started.turn_id).unwrap();
    let prompt_index = context
        .blocks()
        .iter()
        .position(|block| block.label == "user prompt")
        .unwrap();
    let message_indexes = context
        .blocks()
        .iter()
        .enumerate()
        .filter(|(_, block)| block.label.contains("active-message-1"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(message_indexes.len(), 1);
    assert!(prompt_index < message_indexes[0]);
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );
}

/// Verifies a failed user-turn setup leaves its pending peer message unrendered
/// and unacknowledged, while a retry commits and presents it exactly once.
#[test]
fn runtime_user_turn_peer_message_precommit_failure_retries_one_echo() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
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
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "user-turn-retry".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "retry user turn peer mail".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    service.fail_next_peer_message_turn_commit_for_tests();
    assert!(
        service
            .start_agent_prompt_turn("%1", "continue work")
            .is_err()
    );
    assert!(peer_echo_pane_lines(&service, "%1").is_empty());
    assert_ne!(
        service
            .control
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );

    service
        .start_agent_prompt_turn("%1", "continue work")
        .unwrap();
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains("retry user turn peer mail"))
            .count(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a failed idle peer-trigger setup leaves its pending message
/// unrendered and unacknowledged, while the next delivery commits one row.
#[test]
fn runtime_idle_turn_peer_message_precommit_failure_retries_one_echo() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
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
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "idle-turn-retry".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "retry idle turn peer mail".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    service.fail_next_peer_message_turn_commit_for_tests();
    assert!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .is_err()
    );
    assert!(peer_echo_pane_lines(&service, "%1").is_empty());
    assert_ne!(
        service
            .control
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains("retry idle turn peer mail"))
            .count(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies each recoverable receive commit window preserves exactly one
/// canonical peer context event, advances the durable cursor, and creates one
/// receiver-only presentation row.
///
/// The matrix covers user-started and idle peer-triggered turns with a
/// deterministic failure immediately after context storage, immediately after
/// cursor advancement, and during presentation after acknowledgement.
/// Recovery runs through the ordinary delivery sweep, which must resume the
/// original partial turn instead of creating a second turn or reusing payload
/// text as an identity. A presentation failure is deliberately nonblocking:
/// it retains a receipt for retry while the acknowledged turn receives provider
/// ownership. The persisted source proves the one visible row is keyed by the
/// stable delivery sequence plus message id, while the paired identical payload
/// values ensure content alone could not provide this guarantee.
#[test]
fn runtime_receive_commit_fault_windows_recover_one_context_cursor_and_row() {
    for (path, fault_after_cursor, presentation_failure, post_admission_failure, verbose_json) in [
        ("user", false, false, false, false),
        ("user", true, false, false, false),
        ("idle", false, false, false, false),
        ("idle", true, false, false, false),
        ("user", false, true, false, false),
        ("idle", false, true, false, false),
        ("idle", false, false, true, false),
        ("idle", false, true, false, true),
    ] {
        let root = temp_root(&format!(
            "runtime-receive-recovery-{path}-{fault_after_cursor}"
        ));
        let store = AgentTranscriptStore::new(root.clone());
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(store.clone());
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service.set_pane_screen(
            "%1".to_string(),
            TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
        );
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
            .control
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
        if verbose_json {
            service
                .replace_config_layers(vec![ConfigLayer {
                    name: "verbose-receipt".to_string(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
                }])
                .unwrap();
        }
        let sender = service
            .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
            .unwrap();
        let message_id = format!("receive-recovery-{path}-{fault_after_cursor}");
        let payload = "same payload must not be the deduplication key";
        let delivery = service
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: message_id.clone(),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: if verbose_json {
                        "application/json".to_string()
                    } else {
                        "text/plain; charset=utf-8".to_string()
                    },
                    payload: payload.to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now_ms,
            )
            .unwrap();

        if post_admission_failure {
            service.fail_next_peer_message_turn_post_admission_for_tests();
        } else if presentation_failure {
            service.fail_next_received_peer_message_presentation_for_tests();
        } else if fault_after_cursor {
            service.fail_next_peer_message_receive_after_cursor_advance_for_tests();
        } else {
            service.fail_next_peer_message_receive_after_context_storage_for_tests();
        }
        if path == "user" {
            let result = service.start_agent_prompt_turn("%1", "continue work");
            assert_eq!(result.is_ok(), presentation_failure);
        } else {
            let result = service.deliver_pending_runtime_agent_messages(now_ms);
            assert_eq!(
                result.is_ok(),
                presentation_failure && !post_admission_failure
            );
        }

        // A post-context/pre-cursor interruption leaves its receipt only in
        // actor memory. Snapshot projection must exclude that unacknowledged
        // source so the v6 payload remains self-consistent at the exact fault
        // boundary, before the ordinary delivery sweep can recover the turn.
        if !fault_after_cursor && !presentation_failure && !post_admission_failure {
            let mut snapshot =
                crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
            snapshot.message_state = Some(service.message_service().snapshot_state());
            snapshot.unsettled_peer_presentations =
                service.snapshot_unsettled_received_peer_message_presentations();
            assert!(snapshot.unsettled_peer_presentations.is_empty());
            snapshot.validate().unwrap();
        }

        if verbose_json {
            service
                .replace_config_layers(vec![ConfigLayer {
                    name: "normal-retry".to_string(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: "[agents]\npeer_message_log_mode = \"normal\"\n".to_string(),
                }])
                .unwrap();
        }
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap();
        let turns = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.agent_id == recipient.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            turns.len(),
            1,
            "{path} after_cursor={fault_after_cursor} presentation_failure={presentation_failure}"
        );
        assert_eq!(turns[0].state, AgentTurnState::Running);
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == turns[0].turn_id),
            "{path} after_cursor={fault_after_cursor} presentation_failure={presentation_failure}: recovered turn must be provider-owned"
        );
        let peer_blocks = service
            .agent_turn_contexts()
            .get(&turns[0].turn_id)
            .unwrap()
            .blocks()
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::PeerMessage && block.label.contains(&message_id)
            })
            .collect::<Vec<_>>();
        assert_eq!(peer_blocks.len(), 1, "{peer_blocks:#?}");
        assert_eq!(
            service
                .control
                .message_service()
                .subscription(&recipient.agent_id)
                .unwrap()
                .last_sequence,
            delivery.sequence
        );
        assert_eq!(
            peer_echo_pane_lines(&service, "%1")
                .iter()
                .filter(|line| line.contains(payload))
                .count(),
            1
        );
        let receive_identity = format!(
            "peer-message recipient={} sequence={} id={message_id}",
            recipient.agent_id, delivery.sequence
        );
        assert_eq!(
            store
                .inspect_presentation(&conversation_id)
                .unwrap()
                .iter()
                .filter(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(receive_identity.as_str()))
                })
                .count(),
            1
        );
        service.terminate_all_pane_processes().unwrap();
        let _ = fs::remove_dir_all(root);
    }
}

/// Verifies a receive failure after context storage but before cursor
/// acknowledgement cannot grant provider ownership once the source envelope
/// has expired.
///
/// The queued turn and its canonical peer block intentionally survive the
/// recoverable fault, but the durable cursor remains before the delivery. A
/// later sweep at an expired timestamp must neither enqueue scheduler work nor
/// claim a provider task. This prevents a partial receive commit from becoming
/// executable solely because its turn metadata happened to be present.
#[test]
fn runtime_post_context_pre_cursor_expiry_does_not_admit_provider_work() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds()
        .saturating_mul(1000)
        .saturating_sub(2);
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
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "post-context-pre-cursor-expiry".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: Some(1),
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "expired mail cannot start provider work".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    service.fail_next_peer_message_receive_after_context_storage_for_tests();
    assert!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .is_err()
    );

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(2))
            .unwrap(),
        0
    );
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient.agent_id.as_str())
        .unwrap();
    assert_eq!(turn.state, AgentTurnState::Interrupted);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(!service.agent_work_is_scheduled(&turn.turn_id));
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty(),
        "an expired pre-cursor receipt must not survive as an unacknowledgeable outbox orphan"
    );
    assert!(
        service
            .peer_message_delivery_timer_transition(false, 1, now_ms.saturating_add(2))
            .side_effects
            .is_empty(),
        "an expired orphan must not re-arm the peer delivery timer"
    );
    let mut snapshot =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    snapshot.message_state = Some(service.message_service().snapshot_state());
    snapshot.validate().unwrap();
    assert_eq!(
        service
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence.saturating_sub(1)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies cumulative cursor recovery retains every receipt from one partial
/// recipient turn when an earlier envelope expires but a later envelope still
/// remains deliverable.
///
/// The cursor can advance directly through the later sequence, thereby
/// acknowledging both canonical context events. Recovery must therefore keep
/// the entire recipient-and-turn receipt group until that cumulative commit,
/// rather than retiring the expired first receipt and losing its one required
/// receiver presentation row. A repeated delivery sweep proves neither context
/// identity nor pane row is duplicated after the group has settled.
#[test]
fn runtime_partial_receive_group_recovers_expired_prefix_with_later_delivery() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    let deliveries = [
        ("group-expired-prefix", Some(1)),
        ("group-deliverable-suffix", None),
    ]
    .into_iter()
    .map(|(id, ttl_ms)| {
        service
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: id.to_string(),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: id.to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now_ms,
            )
            .unwrap()
    })
    .collect::<Vec<_>>();
    service.fail_next_peer_message_receive_after_context_storage_for_tests();
    assert!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .is_err()
    );

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(2))
            .unwrap(),
        0,
        "the later delivery must recover the existing partial turn rather than create another"
    );
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient.agent_id.as_str())
        .unwrap();
    assert_eq!(turn.state, AgentTurnState::Running);
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    for delivery in &deliveries {
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.label.contains(&delivery.message_id))
                .count(),
            1
        );
        assert_eq!(
            peer_echo_pane_lines(&service, "%1")
                .iter()
                .filter(|line| line.contains(&delivery.message_id))
                .count(),
            1
        );
    }
    assert_eq!(
        service
            .message_service()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        deliveries[1].sequence
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(2))
            .unwrap(),
        0
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/new` retires an unrendered receipt owned by the conversation it
/// replaces, so a later delivery sweep cannot render that old row in the new
/// conversation.
#[test]
fn runtime_new_retires_unrendered_peer_presentation_receipt() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    let sender_agent_id = sender.agent_id.clone();
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender_agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "new-retires-unrendered-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "old conversation receipt".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    service.fail_next_peer_message_receive_after_context_storage_for_tests();
    assert!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .is_err()
    );
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty(),
        "pre-cursor receipts stay in actor memory and cannot enter a v6 snapshot"
    );

    let response = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(response.contains("new=true"), "{response}");
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty()
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(peer_echo_pane_lines(&service, "%1").is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/resume` retires an unrendered receipt owned by the conversation
/// it replaces before replaying the target conversation into the pane.
#[test]
fn runtime_resume_retires_unrendered_peer_presentation_receipt() {
    let root = temp_root("runtime-resume-retires-unrendered-receipt");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "resume-receipt-target".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "target-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume target".to_string(),
        })
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    let sender_agent_id = sender.agent_id.clone();
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender_agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "resume-retires-unrendered-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "old conversation receipt".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    service.fail_next_peer_message_receive_after_context_storage_for_tests();
    assert!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .is_err()
    );
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty(),
        "pre-cursor receipts stay in actor memory and cannot enter a v6 snapshot"
    );

    let response = service
        .execute_agent_shell_command(&primary, "/resume resume-receipt-target")
        .unwrap();
    assert!(response.contains(r#""body":null"#), "{response}");
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred direct resume settles");
    assert!(settled.contains("resumed=true"), "{settled}");
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty()
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .all(|line| !line.contains("old conversation receipt"))
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies receipt state retires immediately after its one live receiver row
/// when no transcript presentation store is configured.
///
/// A store-less runtime still commits the canonical peer context and renders the
/// recipient row exactly once. It has no durable presentation owner to await,
/// however, so retaining the acknowledged receipt would create unbounded actor
/// state. The snapshot projection must therefore be empty after the live row is
/// accepted rather than depending on a later persistence sweep that cannot run.
#[test]
fn runtime_received_peer_receipt_retires_after_live_render_without_store() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    service
        .start_agent_prompt_turn("%1", "receive store-less peer mail")
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let payload = "store-less receipt must retire after its live row";
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "store-less-presentation-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: payload.to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains(payload))
            .count(),
        1
    );
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a receive receipt remains unresolved while its presentation append
/// is queued, and settles only after the persistence worker confirms the exact
/// durable entry.
///
/// The live pane may display the accepted message before the external
/// persistence adapter writes it, but a second delivery sweep must not append a
/// duplicate row during that interval. Completing the queued effect with the
/// store's actual durable write then proves receipt retirement is coupled to
/// persisted source identity rather than to the renderer's successful return.
#[test]
fn runtime_received_peer_receipt_waits_for_queued_presentation_persistence() {
    let root = temp_root("runtime-receipt-queued-persistence");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient.agent_id)
        .unwrap();
    service
        .start_agent_prompt_turn("%1", "receive durable peer mail")
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let sender_agent_id = sender.agent_id.clone();
    let payload = "queued persistence must settle the receipt";
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender_agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "queued-presentation-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: payload.to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains(payload))
            .count(),
        1
    );
    assert!(
        store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains(payload))
            .count(),
        1,
        "the unresolved receipt must suppress duplicate live presentation"
    );

    let identity = format!(
        "peer-message recipient={} sequence={} id=queued-presentation-receipt",
        recipient.agent_id, delivery.sequence
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let (path, entries) = effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistPresentationEntries { path, entries, .. }
                if entries.iter().any(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(identity.as_str()))
                }) =>
            {
                Some((path, entries))
            }
            _ => None,
        })
        .expect("queued peer presentation effect");
    let bytes = store.append_presentation_many(&entries).unwrap();
    service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id: conversation_id.clone(),
            path,
            entries: entries.len(),
            bytes,
        })
        .unwrap();
    assert_eq!(
        store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .iter()
            .filter(|entry| {
                entry
                    .source_text
                    .as_deref()
                    .is_some_and(|source| source.contains(identity.as_str()))
            })
            .count(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies an acknowledged receipt whose external presentation write fails is
/// reconstructed after restart from a recipient-scoped durable receipt outbox,
/// then persists exactly one receiver row after its original transport message
/// has expired and been evicted by bounded queue retention.
///
/// The failed worker event must not mark the receipt complete merely because a
/// live pane accepted the row. A restarted runtime restores the pane binding
/// and MMP snapshot, discovers the missing durable identity, and retries the
/// original receipt once. This covers the failure boundary without introducing
/// a second durable receipt journal beside the message cursor and presentation
/// store that already own the recoverable facts.
#[test]
fn runtime_failed_peer_presentation_persistence_reconstructs_after_restart() {
    let root = temp_root("runtime-receipt-failed-persistence-restart");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "verbose-failed-receipt".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    service
        .start_agent_prompt_turn("%1", "receive restartable peer mail")
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let sender_agent_id = sender.agent_id.clone();
    let payload = "failed persistence must replay one receiver row";
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender_agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "failed-presentation-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: Some(1),
                content_type: "application/json".to_string(),
                payload: payload.to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let identity = format!(
        "peer-message recipient={} sequence={} id=failed-presentation-receipt",
        recipient.agent_id, delivery.sequence
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let mut failed_effect = None;
    let mut completed_effects = Vec::new();
    for effect in effects {
        if let RuntimeSideEffect::PersistPresentationEntries { path, entries, .. } = effect {
            if entries.iter().any(|entry| {
                entry
                    .source_text
                    .as_deref()
                    .is_some_and(|source| source.contains(identity.as_str()))
            }) {
                failed_effect = Some((path, entries));
            } else {
                completed_effects.push((path, entries));
            }
        }
    }
    let (path, entries) = failed_effect.expect("queued peer presentation effect");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let failed_transition = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationFailed {
            conversation_id: conversation_id.clone(),
            path,
            entries: entries.len(),
            error: "injected presentation write failure".to_string(),
        })
        .unwrap();
    for (path, entries) in completed_effects {
        let bytes = store.append_presentation_many(&entries).unwrap();
        service
            .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
                conversation_id: conversation_id.clone(),
                path,
                entries: entries.len(),
                bytes,
            })
            .unwrap();
    }
    assert!(
        store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .iter()
            .all(|entry| !entry
                .source_text
                .as_deref()
                .is_some_and(|source| source.contains(identity.as_str())))
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains(payload))
            .count(),
        1,
        "an async persistence retry must reuse the retained source without rerendering the live row"
    );
    let retry_effects = failed_transition.side_effects;
    assert_eq!(
        retry_effects
            .iter()
            .filter(|effect| {
                matches!(effect, RuntimeSideEffect::PersistPresentationEntries { entries, .. }
                if entries.iter().any(|entry| {
                    entry.source_text.as_deref().is_some_and(|source| {
                        source.contains(identity.as_str())
                    })
                }))
            })
            .count(),
        0,
        "a failed write defers its retry until every pending presentation effect for the conversation settles"
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "normal-restart".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"normal\"\n".to_string(),
        }])
        .unwrap();
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .iter()
            .any(|receipt| receipt.presentation_eligible)
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(1))
            .unwrap(),
        0,
        "the acknowledged retry must persist without committing another transport message"
    );
    let retry_effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let (retry_path, retry_entries) = retry_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistPresentationEntries { path, entries, .. }
                if entries.iter().any(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(identity.as_str()))
                }) =>
            {
                Some((path, entries))
            }
            _ => None,
        })
        .expect("normal-mode persistence retry for verbose-committed receipt");
    let retry_bytes = store.append_presentation_many(&retry_entries).unwrap();
    service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id: service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .session_id
                .clone(),
            path: retry_path,
            entries: retry_entries.len(),
            bytes: retry_bytes,
        })
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata_records = service
        .drain_transcript_persistence_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistAgentSessionMetadata { records, .. } => Some(records),
            _ => None,
        })
        .expect("queued metadata checkpoint before restart");
    store
        .save_agent_session_metadata_checkpoint(service.session().id.as_str(), &metadata_records)
        .unwrap();
    let mut snapshot =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    snapshot.message_state = Some(service.message_service().snapshot_state());
    snapshot.unsettled_peer_presentations =
        service.snapshot_unsettled_received_peer_message_presentations();
    snapshot.agent_sessions = vec![crate::storage::snapshot::SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    snapshot.validate().unwrap();
    let mut eviction_snapshot = snapshot.message_state.clone().unwrap();
    eviction_snapshot.retention_messages = 1;
    let mut evicting_messages = MessageService::from_snapshot_state(&eviction_snapshot).unwrap();
    let eviction_sender = evicting_messages
        .registered_identity(&recipient.agent_id)
        .unwrap()
        .clone();
    evicting_messages
        .accept_at_with_scope(
            &recipient.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "evict-received-presentation-source".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{}", now_ms.saturating_add(1)),
                sender: eviction_sender,
                recipient: mez_agent::messaging::Recipient::Agent(sender_agent_id),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "evict the already acknowledged transport envelope".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms.saturating_add(1),
        )
        .unwrap();
    snapshot.message_state = Some(evicting_messages.snapshot_state());
    assert!(
        snapshot
            .message_state
            .as_ref()
            .unwrap()
            .retained_messages
            .iter()
            .all(|message| message.envelope.id != "failed-presentation-receipt")
    );
    let mut restarted = test_runtime_service();
    restarted.session.id = service.session().id.clone();
    restarted.set_agent_transcript_store(store.clone());
    restarted
        .restore_message_state_for_restored_snapshot(&snapshot)
        .unwrap();
    restarted
        .restore_agent_sessions_for_restored_snapshot(false)
        .unwrap();
    assert_eq!(
        restarted
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(2))
            .unwrap(),
        0,
        "the retained receipt must settle after its acknowledged transport envelope expires"
    );
    let conversation_id = restarted
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert_eq!(
        store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .iter()
            .filter(|entry| {
                entry
                    .source_text
                    .as_deref()
                    .is_some_and(|source| source.contains(identity.as_str()))
            })
            .count(),
        1
    );
    assert_eq!(
        peer_echo_pane_lines(&restarted, "%1")
            .iter()
            .filter(|line| line.contains(payload))
            .count(),
        1,
        "the verbose-committed receipt must replay once after normal-mode v6 restart"
    );
    service.terminate_all_pane_processes().unwrap();
    restarted.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies settlement requires the peer-presentation source type as well as
/// the decoded receiver identity, so peer-shaped JSON under a user content type
/// and a substring in untrusted payload text cannot settle message A.
#[test]
fn runtime_peer_presentation_settlement_ignores_identity_embedded_in_other_payload() {
    let root = temp_root("runtime-peer-presentation-structured-settlement");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    service
        .start_agent_prompt_turn("%1", "receive adversarial peer presentation mail")
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let sender_id = sender.agent_id.clone();
    let first = service
        .message_service_mut()
        .accept_at_with_scope(
            &sender_id,
            Envelope {
                protocol: "mmp/1",
                id: "settlement-message-a".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "message A".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    let first_identity = format!(
        "peer-message recipient={} sequence={} id=settlement-message-a",
        recipient.agent_id, first.sequence
    );
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender_id,
            Envelope {
                protocol: "mmp/1",
                id: "settlement-message-b".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{}", now_ms.saturating_add(1)),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: format!("message B contains unrelated identity {first_identity}"),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms.saturating_add(1),
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let second_entries = effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistPresentationEntries { entries, .. }
                if entries.iter().any(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains("settlement-message-b"))
                }) =>
            {
                Some(entries)
            }
            _ => None,
        })
        .expect("message B presentation effect");
    store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 80,
            style_names: vec!["user-prompt".to_string()],
            display_lines: vec!["spoofed user row".to_string()],
            copy_lines: vec!["spoofed user row".to_string()],
            ansi_text: None,
            source_text: Some(format!(
                r#"{{"direction":"received","receive_identity":"{first_identity}","peer":"sender","payload":"spoofed","content_type":"text/plain; charset=utf-8","direct_parent":false}}"#
            )),
            source_content_type: Some("text/plain; charset=utf-8".to_string()),
        })
        .unwrap();
    store.append_presentation_many(&second_entries).unwrap();
    service
        .settle_received_peer_message_presentations(&conversation_id)
        .unwrap();
    let pending = service.snapshot_unsettled_received_peer_message_presentations();
    assert!(
        pending
            .iter()
            .any(|receipt| receipt.identity == first_identity)
    );
    assert!(pending.iter().all(|receipt| receipt.identity
        != "peer-message recipient=agent-%1 sequence=2 id=settlement-message-b"));
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a failed receipt waits for a later delivery lifecycle event before
/// retrying, preserving both original effects and producing exactly one retry
/// source without recursively churning completion effects.
#[test]
fn runtime_partial_presentation_drain_defers_and_deduplicates_receipt_retry() {
    let root = temp_root("runtime-peer-presentation-partial-drain");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
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
    service
        .start_agent_prompt_turn("%1", "receive two durable peer messages")
        .unwrap();
    service.use_transcript_effect_adapter();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let sender_id = sender.agent_id.clone();
    let deliveries = ["partial-drain-a", "partial-drain-b"]
        .into_iter()
        .map(|id| {
            service
                .message_service_mut()
                .accept_at_with_scope(
                    &sender_id,
                    Envelope {
                        protocol: "mmp/1",
                        id: id.to_string(),
                        message_type: "send".to_string(),
                        time: format!("runtime:{now_ms}"),
                        sender: sender.clone(),
                        recipient: mez_agent::messaging::Recipient::Agent(
                            recipient.agent_id.clone(),
                        ),
                        correlation_id: None,
                        ttl_ms: None,
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: id.to_string(),
                        extension_fields: Vec::new(),
                    },
                    MessageScope::Session,
                    now_ms,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2
    );
    let first_identity = format!(
        "peer-message recipient={} sequence={} id=partial-drain-a",
        recipient.agent_id, deliveries[0].sequence
    );
    let second_identity = format!(
        "peer-message recipient={} sequence={} id=partial-drain-b",
        recipient.agent_id, deliveries[1].sequence
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let mut first_effect = None;
    let mut second_effect = None;
    for effect in effects {
        if let RuntimeSideEffect::PersistPresentationEntries { path, entries, .. } = effect {
            let source = entries[0].source_text.as_deref().unwrap_or_default();
            if source.contains(first_identity.as_str()) {
                first_effect = Some((path, entries));
            } else if source.contains(second_identity.as_str()) {
                second_effect = Some((path, entries));
            }
        }
    }
    let (first_path, first_entries) = first_effect.expect("first queued presentation effect");
    let (second_path, second_entries) = second_effect.expect("second queued presentation effect");
    let failed = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationFailed {
            conversation_id: conversation_id.clone(),
            path: first_path,
            entries: first_entries.len(),
            error: "injected first presentation failure".to_string(),
        })
        .unwrap();
    assert!(failed.side_effects.iter().all(|effect| !matches!(
        effect,
        RuntimeSideEffect::PersistPresentationEntries { entries, .. }
            if entries.iter().any(|entry| entry.source_text.as_deref().is_some_and(|source| source.contains(first_identity.as_str())))
    )));
    let bytes = store.append_presentation_many(&second_entries).unwrap();
    let completed = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id: conversation_id.clone(),
            path: second_path,
            entries: second_entries.len(),
            bytes,
        })
        .unwrap();
    assert!(
        completed
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::PersistPresentationEntries { .. }))
    );
    let immediate_effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert_eq!(
        immediate_effects
            .iter()
            .filter(|effect| matches!(
                effect,
                RuntimeSideEffect::PersistPresentationEntries { entries, .. }
                    if entries.iter().any(|entry| entry.source_text.as_deref().is_some_and(|source| source.contains(first_identity.as_str())))
            ))
            .count(),
        0,
        "persistence settlement must not recursively queue its own retry"
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0,
        "the acknowledged receipt retries without committing another inbox message"
    );
    let retry_effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert_eq!(
        retry_effects
            .iter()
            .filter(|effect| matches!(
                effect,
                RuntimeSideEffect::PersistPresentationEntries { entries, .. }
                    if entries.iter().any(|entry| entry.source_text.as_deref().is_some_and(|source| source.contains(first_identity.as_str())))
            ))
            .count(),
        1,
        "one later delivery sweep queues the retained receipt once"
    );
    let (retry_path, retry_entries) = retry_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistPresentationEntries { path, entries, .. }
                if entries.iter().any(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(first_identity.as_str()))
                }) =>
            {
                Some((path, entries))
            }
            _ => None,
        })
        .expect("lifecycle retry presentation effect");
    let repeated_failure = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationFailed {
            conversation_id: conversation_id.clone(),
            path: retry_path,
            entries: retry_entries.len(),
            error: "injected repeated presentation failure".to_string(),
        })
        .unwrap();
    assert!(
        repeated_failure
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::PersistPresentationEntries { .. }))
    );
    assert_eq!(
        peer_echo_pane_lines(&service, "%1")
            .iter()
            .filter(|line| line.contains("partial-drain-a"))
            .count(),
        1,
        "persistence retries must not install another live pane row"
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    let recovery_effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let (recovery_path, recovery_entries) = recovery_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistPresentationEntries { path, entries, .. }
                if entries.iter().any(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(first_identity.as_str()))
                }) =>
            {
                Some((path, entries))
            }
            _ => None,
        })
        .expect("later recovery presentation effect");
    let recovery_bytes = store.append_presentation_many(&recovery_entries).unwrap();
    service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id,
            path: recovery_path,
            entries: recovery_entries.len(),
            bytes: recovery_bytes,
        })
        .unwrap();
    assert_eq!(
        store
            .inspect_presentation(&service.agent_shell_store().get("%1").unwrap().session_id)
            .unwrap()
            .iter()
            .filter(|entry| entry
                .source_text
                .as_deref()
                .is_some_and(|source| source.contains(first_identity.as_str())))
            .count(),
        1,
        "the later recovery settles exactly one retained receipt"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies one group envelope creates one receiver-owned visible and durable
/// presentation row for each committing recipient, even though both rows share
/// the accepted delivery sequence and immutable envelope id.
///
/// Group fanout is the boundary that proves a pane-derived receipt key is
/// insufficient: two registered recipient identities accept the same envelope
/// but own different conversations and terminal rows. The assertion checks both
/// live pane output and each conversation's persisted source, preventing a
/// retry or cross-recipient deduplication from collapsing one recipient's row.
#[test]
fn runtime_group_fanout_receipts_are_scoped_to_each_committing_recipient() {
    let root = temp_root("runtime-group-fanout-receipts");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_terminal_command(&primary, "split-window")
        .unwrap();
    for pane_id in ["%1", "%2"] {
        service.set_pane_screen(
            pane_id.to_string(),
            TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
        );
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipients = ["%1", "%2"]
        .into_iter()
        .map(|pane_id| {
            service
                .ensure_runtime_message_identity(
                    format!("agent-{pane_id}").as_str(),
                    PaneId::opaque(pane_id.to_string()),
                    "agent",
                    &["receipt-fanout"],
                    now_ms,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    for recipient in &recipients {
        service
            .control
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
    }
    let conversations = ["%1", "%2"]
        .into_iter()
        .map(|pane_id| {
            service
                .start_agent_prompt_turn(pane_id, "receive group peer mail")
                .unwrap();
            let conversation_id = service
                .agent_shell_store()
                .get(pane_id)
                .unwrap()
                .session_id
                .clone();
            (pane_id, conversation_id)
        })
        .collect::<Vec<_>>();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let sender_agent_id = sender.agent_id.clone();
    let payload = "one receipt row for every group recipient";
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender_agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "group-fanout-receipt".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Group("receipt-fanout".to_string()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: payload.to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2
    );
    for ((pane_id, conversation_id), recipient) in conversations.iter().zip(&recipients) {
        let pane_lines = peer_echo_pane_lines(&service, pane_id);
        assert_eq!(
            pane_lines
                .iter()
                .filter(|line| line.contains("agent-sender> one receipt row for"))
                .count(),
            1,
            "{pane_id} must own exactly one visible receiver row: {pane_lines:#?}"
        );
        let identity = format!(
            "peer-message recipient={} sequence={} id=group-fanout-receipt",
            recipient.agent_id, delivery.sequence
        );
        assert_eq!(
            store
                .inspect_presentation(conversation_id)
                .unwrap()
                .iter()
                .filter(|entry| {
                    entry
                        .source_text
                        .as_deref()
                        .is_some_and(|source| source.contains(identity.as_str()))
                })
                .count(),
            1,
            "{pane_id} must own exactly one durable receiver row"
        );
    }
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies `wait` parks its turn and model-originated MMP mail resumes that
/// same turn without creating a new peer-message-triggered follow-up.
///
/// The parked wait releases provider capacity while preserving the original
/// turn, context, and scheduler ownership until committed model mail fairly
/// reacquires capacity for its continuation.
#[test]
fn runtime_wait_parks_turn_and_peer_mail_resumes_same_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "ask a peer and wait for the reply")
        .unwrap();
    let turn_count = service.agent_turn_ledger().turns().len();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "wait for MMP peer mail".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "the requested MMP reply is still pending".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "wait-1".to_string(),
                    payload: mez_agent::AgentActionPayload::Wait,
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    let idle = service.agent_scheduler().snapshot();
    assert_eq!(idle.running, 0);
    assert_eq!(idle.waiting, 1);
    assert_eq!(idle.active_capacity_used, 0);
    let parked_frame = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        parked_frame
            .frame_context
            .panes
            .get("%1")
            .unwrap()
            .agent_status
            .as_deref(),
        Some("waiting")
    );
    let parked_agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"parked-agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(
        parked_agents.contains(r#""status":"waiting""#),
        "{parked_agents}"
    );

    let now_ms = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "wait-bridge-status".to_string(),
                message_type: "task_status".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "application/json".to_string(),
                payload: r#"{"task_id":"wait-1","state":"running","summary":"runtime bridge"}"#
                    .to_string(),
                extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "wait-reply-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "the requested peer result".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    service.fail_next_received_peer_message_presentation_for_tests();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let running = service.agent_scheduler().snapshot();
    assert_eq!(running.waiting, 0);
    assert_eq!(running.running, 1);
    assert_eq!(running.active_capacity_used, 1);
    let running_frame = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        running_frame
            .frame_context
            .panes
            .get("%1")
            .unwrap()
            .agent_status
            .as_deref(),
        Some("thinking")
    );
    assert!(running_frame.frame_context.animation_tick_ms > 0);
    let running_agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"running-agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(
        running_agents.contains(r#""status":"running""#),
        "{running_agents}"
    );
    assert_eq!(service.agent_peer_message_turn_count(&started.agent_id), 0);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
}

/// Verifies a parked peer wait keeps its deadline paused while fair scheduler
/// reacquisition waits behind occupied provider capacity.
#[test]
fn runtime_wait_deadline_remains_paused_until_scheduler_reacquires_capacity() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "wait for a peer reply")
        .unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "wait for MMP peer mail".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "the requested MMP reply is still pending".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "wait-capacity".to_string(),
                    payload: mez_agent::AgentActionPayload::Wait,
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let deadline_before_wake = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .deadline_at_unix_millis;

    service.configure_agent_scheduler_limit(1).unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "capacity-holder".to_string(),
            conversation_id: "capacity-holder".to_string(),
            agent_id: "agent-capacity-holder".to_string(),
            pane_id: None,
            kind: mez_agent::ScheduledWorkKind::BackgroundTask,
        })
        .unwrap();
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "capacity-holder"
    );

    let now_ms = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "capacity-delayed-wait-reply".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "peer reply".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(now_ms)
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.agent_scheduler().snapshot().reacquiring, 1);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .deadline_at_unix_millis,
        deadline_before_wake
    );

    std::thread::sleep(std::time::Duration::from_millis(5));
    service
        .agent_scheduler_mut()
        .complete("capacity-holder")
        .unwrap();
    service.start_ready_agent_turns().unwrap();
    let resumed = service.agent_turn_ledger().turn(&started.turn_id).unwrap();
    assert_eq!(resumed.state, AgentTurnState::Running);
    assert!(resumed.deadline_at_unix_millis > deadline_before_wake);
}

/// Verifies a model-originated reply already queued when `wait` settles wakes
/// the same turn without waiting for the next delivery timer sweep.
#[test]
fn runtime_wait_immediately_resumes_for_already_queued_peer_mail() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "wait for a peer reply already in transit")
        .unwrap();
    let now_ms = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "already-queued-wait-reply".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "reply arrived before the wait parked".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "wait for MMP peer mail".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "the requested MMP reply is still pending".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "wait-queued-reply".to_string(),
                    payload: mez_agent::AgentActionPayload::Wait,
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let scheduler = service.agent_scheduler().snapshot();
    assert_eq!(scheduler.waiting, 0);
    assert_eq!(scheduler.running, 1);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
    assert!(
        service.agent_turn_contexts()[&started.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label.contains("already-queued-wait-reply"))
    );
}

/// Verifies an active local message invalidates an older provider generation
/// before that generation can append or dispatch its response.
///
/// The message is committed and acknowledged at actor delivery, so a provider
/// completion whose consumed high-water mark predates it must remain
/// diagnostic-only. The continuation queued by message delivery retains the
/// newer canonical snapshot as the only model-visible future.
#[tokio::test]
async fn runtime_local_message_discards_older_provider_generation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect before the message arrives")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let consumed_high_water_mark = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests(&turn.turn_id, consumed_high_water_mark)
        .unwrap();

    let now_ms = current_unix_seconds().saturating_mul(1000);
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "stale-provider-message".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "newer local evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );

    let response = runtime_say_response(&turn.turn_id, "obsolete provider conclusion", true);
    let action = response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .first()
        .unwrap()
        .clone();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response,
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["obsolete provider conclusion".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    assert!(
        service
            .apply_agent_provider_completed_event(&recipient, &turn.turn_id, execution)
            .await
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("newer local evidence"))
    );
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| !block.content.contains("obsolete provider conclusion"))
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
}

/// Verifies a peer message for an idle agent starts one LocalMessage-triggered
/// turn carrying the message as lower-priority peer context.
///
/// Peer text must never enter the conversation as a user instruction, the block
/// must carry the untrusted-data guidance, a repeated delivery pass must not
/// duplicate the block, and the durable cursor must advance only after the
/// canonical event exists.
#[test]
fn runtime_idle_agent_peer_message_starts_local_message_turn() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "inactive-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "background evidence".to_string(),
        extension_fields: Vec::new(),
    };
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now_ms)
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient_identity.agent_id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(turn.trigger, mez_agent::AgentTurnTrigger::LocalMessage);
    assert!(matches!(
        turn.state,
        AgentTurnState::Queued | AgentTurnState::Running
    ));
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.label.contains("inactive-message-1"))
            .count(),
        1,
        "a repeated delivery pass must not duplicate the committed block"
    );
    let peer = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::PeerMessage)
        .unwrap();
    assert!(peer.content.contains("background evidence"));
    assert!(peer.content.contains("untrusted data"), "{}", peer.content);
    assert!(peer.content.contains("never approve"), "{}", peer.content);
    assert!(
        peer.content.contains("your own approval mode"),
        "{}",
        peer.content
    );
    assert!(peer.content.contains("from_agent=agent-sender"));
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::UserInstruction),
        "peer text must never enter the conversation as user input"
    );
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );
}

/// Returns the agent gutter lines currently visible in one test pane.
fn peer_echo_pane_lines(
    service: &crate::runtime::RuntimeSessionService,
    pane_id: &str,
) -> Vec<String> {
    service
        .pane_screen(pane_id)
        .map(|screen| {
            screen
                .normal_content_lines()
                .into_iter()
                .filter(|line| line.starts_with("▐ "))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Verifies peer-message labels prefer a live endpoint title and otherwise
/// preserve the canonical runtime agent id.
#[test]
fn runtime_peer_message_endpoint_labels_use_live_titles_with_agent_id_fallback() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .session
        .set_pane_title_explicit("%1", "  coordinator pane  ")
        .unwrap();

    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%1"),
        "coordinator pane"
    );
    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%9"),
        "agent-%9"
    );
    assert_eq!(
        service.runtime_peer_message_endpoint_label("external-agent"),
        "external-agent"
    );
}

/// Verifies delivered peer mail is logged prompt-style in the recipient pane
/// with the sender named at the destination end of the direction arrow.
///
/// An operator watching a pane must see interagent traffic the way user prompts
/// appear, including the same hanging-indent wrapping for a long payload, and the
/// echo must stay pure observation: the durable block keeps the peer trust domain,
/// no user instruction appears, no context block claims the echo, and a repeated
/// delivery pass neither re-echoes nor re-commits the message.
#[test]
fn runtime_peer_message_echo_logs_sender_prefix_without_user_trust_domain() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &[], now_ms)
        .unwrap();
    let peer_message = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-1", "alpha beta gamma delta epsilon"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );

    let echoed = peer_echo_pane_lines(&service, "%1");
    assert!(
        echoed.iter().any(|line| line == "▐ agent-%3> alpha beta"),
        "{echoed:#?}"
    );
    assert!(
        echoed.iter().any(|line| line == "▐      gamma delta"),
        "{echoed:#?}"
    );
    assert!(
        echoed.iter().any(|line| line == "▐      epsilon"),
        "{echoed:#?}"
    );
    assert_eq!(
        echoed.iter().filter(|line| line.contains("> ")).count(),
        1,
        "the sender label prints once instead of repeating on continuation rows: {echoed:#?}"
    );
    assert!(
        echoed.iter().all(|line| line.chars().count() <= 24),
        "wrapped peer rows must stay inside the pane width: {echoed:#?}"
    );

    // The idle delivery started a message-triggered turn, so this arrival takes
    // the active-turn append path and must echo there too.
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-2", "cwd ok"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let active_turn_echoed = peer_echo_pane_lines(&service, "%1");
    assert!(
        active_turn_echoed
            .iter()
            .any(|line| line == "▐ agent-%3> cwd ok"),
        "{active_turn_echoed:#?}"
    );

    // Distinct accepted envelopes carrying identical text remain distinct
    // committed messages. Logging is keyed by the canonical delivery, never
    // by payload text.
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-3", "cwd ok"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let repeated_payload_echoed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        repeated_payload_echoed
            .iter()
            .filter(|line| line == &"▐ agent-%3> cwd ok")
            .count(),
        2,
        "identical payloads from separate committed envelopes both log: {repeated_payload_echoed:#?}"
    );

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient_identity.agent_id.as_str())
        .cloned()
        .expect("peer message turn");
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let peer_blocks = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::PeerMessage)
        .collect::<Vec<_>>();
    assert_eq!(peer_blocks.len(), 3, "{peer_blocks:#?}");
    assert!(
        peer_blocks[0]
            .content
            .contains("alpha beta gamma delta epsilon")
    );
    assert!(peer_blocks[1].content.contains("cwd ok"));
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::UserInstruction),
        "the echoed peer line must never create user-trust context"
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.label.contains("agent-%3>")),
        "the pane echo is presentation-only and must not become provider context"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies accepted `send_message` actions create one sender-side row while
/// rejected and undeliverable sends remain absent from the sender transcript.
///
/// A sender row proves only message-service acceptance, never recipient
/// observation or processing, so failures must not leak their raw payload.
#[test]
fn runtime_send_message_echoes_at_sender_only_after_acceptance() {
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent-%2", "text/plain", "ack, running now");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert!(
        sent.iter()
            .any(|line| line.contains("agent-%2< ack, running now")),
        "{sent:#?}"
    );
    service.terminate_all_pane_processes().unwrap();

    let (mut rejected, execution, _target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .expect("recipient rejection")
            .code,
        "invalid_message_recipient"
    );
    let rejected_lines = peer_echo_pane_lines(&rejected, "%1");
    assert!(
        !rejected_lines.iter().any(|line| line.contains("handoff")),
        "{rejected_lines:#?}"
    );
    rejected.terminate_all_pane_processes().unwrap();

    let (mut undeliverable, execution, _target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .expect("unavailable recipient failure")
            .code,
        "message_recipient_unavailable"
    );
    let undeliverable_lines = peer_echo_pane_lines(&undeliverable, "%1");
    assert!(
        !undeliverable_lines
            .iter()
            .any(|line| line.contains("handoff")),
        "{undeliverable_lines:#?}"
    );
    undeliverable.terminate_all_pane_processes().unwrap();
}

/// Verifies pending peer mail committed into a user-started turn is logged
/// exactly once, without becoming user-trust context.
///
/// A user prompt commits the recipient's unread peer mail into the new turn
/// instead of starting a message-triggered turn, so it is a separate commit
/// site. A message committed there must be as operator-visible as one committed
/// at arrival time, while its durable block stays a peer reference event and the
/// logged line stays presentation-only.
#[test]
fn runtime_peer_message_echo_logs_one_line_for_user_prompt_turn_commit() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &[], now_ms)
        .unwrap();
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "prompt-path-echo-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "pending peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    let started = service
        .start_agent_prompt_turn("%1", "inspect the pending mail")
        .unwrap();
    let context = service.agent_turn_contexts().get(&started.turn_id).unwrap();
    let peer_blocks = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::PeerMessage)
        .collect::<Vec<_>>();
    assert_eq!(peer_blocks.len(), 1, "{peer_blocks:#?}");
    assert!(peer_blocks[0].content.contains("pending peer evidence"));
    assert!(
        !context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::UserInstruction
                && block.content.contains("pending peer evidence")
        }),
        "committed peer mail must never enter the turn as user input"
    );
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );

    let echoed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        echoed
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "a message committed into a user-started turn logs exactly one line: {echoed:#?}"
    );
    assert!(
        echoed
            .iter()
            .any(|line| line == "▐ agent-%3> pending peer evidence"),
        "{echoed:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime-owned bridge traffic follows the same commit rule as
/// model-originated peer mail, so the pane log never depends on whether the
/// recipient happened to be busy.
///
/// An all-bridge batch starts no idle turn and commits nothing, so it logs
/// nothing. Commit membership is otherwise unchanged: a committed bridge
/// notification consumes no display row and no placeholder, even when its JSON
/// payload carries an `output` field, while the model-authored peer message it
/// was committed alongside still logs exactly once. `verbose` restores the
/// bridge echo and logs the full bounded payload for JSON traffic too.
#[test]
fn runtime_peer_message_echo_logs_committed_bridge_traffic_once() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let child = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let bridge = |id: &str, task_id: &str, summary: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "task_status".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: Some(task_id.to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: mez_agent::messaging::TaskStatusPayload {
            task_id: task_id.to_string(),
            state: mez_agent::messaging::TaskState::Running,
            progress_percent: Some(0),
            summary: summary.to_string(),
        }
        .to_json(),
        extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
    };
    let model = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    let result = |id: &str, task_id: &str, output: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "task_result".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: Some(task_id.to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: format!(
            r#"{{"task_id":"{task_id}","success":true,"summary":"bridge result","output":{output}}}"#
        ),
        extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
    };
    let accept = |service: &mut crate::runtime::RuntimeSessionService, envelope: Envelope| {
        let sender = envelope.sender.agent_id.clone();
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender, envelope, MessageScope::Session, now_ms)
            .unwrap();
    };
    let compact = |lines: Vec<String>| {
        lines
            .join("\n")
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>()
    };

    accept(
        &mut service,
        bridge("bridge-1", "bridge-turn-1", "bridge evidence one"),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0,
        "an all-bridge batch starts no idle turn"
    );
    let idle = peer_echo_pane_lines(&service, "%1");
    assert!(
        !idle.iter().any(|line| line.contains("agent-%3>")),
        "nothing is committed, so nothing is logged: {idle:#?}"
    );

    accept(
        &mut service,
        bridge("bridge-2", "bridge-turn-2", "bridge evidence two"),
    );
    accept(&mut service, model("mixed-1", "mixed peer request"));
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        3,
        "the started turn commits the mixed batch and the earlier bridge message"
    );
    let committed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        committed
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "only the canonical plaintext model message renders in normal mode: {committed:#?}"
    );
    let committed_text = compact(committed.clone());
    assert_eq!(
        committed_text.matches("mixedpeerrequest").count(),
        1,
        "the committed model message logs exactly once: {committed_text}"
    );
    for summary in ["bridgeevidenceone", "bridgeevidencetwo"] {
        assert_eq!(
            committed_text.matches(summary).count(),
            0,
            "normal presentation suppresses non-plaintext bridge payloads: {committed_text}"
        );
    }

    accept(
        &mut service,
        bridge("bridge-3", "bridge-turn-3", "bridge evidence three"),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let active = compact(peer_echo_pane_lines(&service, "%1"));
    assert_eq!(
        active.matches("mixedpeerrequest").count(),
        1,
        "an already-committed message is never echoed twice: {active}"
    );
    assert_eq!(
        active.matches("bridgeevidencethree").count(),
        0,
        "an active-turn committed task status remains presentation-silent: {active}"
    );

    // A task-result bridge payload remains presentation-silent because normal
    // mode admits only the canonical plaintext media type.
    accept(
        &mut service,
        result("bridge-4", "bridge-turn-4", "\"task complete\""),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let projected = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        projected
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "non-plaintext bridge status and result payloads remain presentation-silent: {projected:#?}"
    );
    let projected_text = compact(projected);
    assert_eq!(
        projected_text.matches("taskcomplete").count(),
        0,
        "normal mode no longer projects JSON result output: {projected_text}"
    );
    for omitted in ["bridgeresult", "success"] {
        assert_eq!(
            projected_text.matches(omitted).count(),
            0,
            "a suppressed bridge payload reaches no row, so {omitted} must not be logged: \
             {projected_text}"
        );
    }

    // Verbose mode logs the complete bounded raw bridge payload.
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "peer-message-log-mode-verbose".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
        }])
        .unwrap();
    accept(
        &mut service,
        bridge("bridge-5", "bridge-turn-5", "verbose bridge evidence"),
    );
    accept(
        &mut service,
        result("bridge-6", "bridge-turn-6", "\"verbose task complete\""),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "verbose mode does not change the commit rule"
    );
    let verbose_text = compact(service.pane_screen("%1").unwrap().normal_content_lines());
    assert!(
        verbose_text.contains("summaryverbosebridgeevidence"),
        "verbose mode logs the bounded `task_status` payload: {verbose_text}"
    );
    assert!(
        verbose_text.contains("bridgeresult"),
        "verbose mode logs the whole bounded payload: \
         {verbose_text}"
    );
    assert!(
        verbose_text.contains("verbosetaskcomplete"),
        "{verbose_text}"
    );
    assert_eq!(
        verbose_text.matches("mixedpeerrequest").count(),
        1,
        "verbose mode never re-echoes a committed message: {verbose_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies model-authored peer mail logs at an accepted sender and committing
/// recipient in default normal mode, including a child with no subagent display
/// name.
///
/// Normal-mode presentation admits only the exact canonical
/// `text/plain; charset=utf-8` media type. Delegation lineage and the optional
/// `subagent_display_name` extension do not affect that decision, so canonical
/// model `send_message` traffic keeps its receiver-side `{name}> ` row.
#[test]
fn runtime_model_peer_mail_without_bridge_provenance_logs_at_sender_and_receiver() {
    // Parent -> child: acceptance creates one sender-side row.
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent:agent-%2", "text/plain", "parent reply");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert!(
        sent.iter()
            .any(|line| line.contains("agent-%2< parent reply")),
        "{sent:#?}"
    );
    service.terminate_all_pane_processes().unwrap();

    // Child -> parent: the committed inbound echo names the sender. The child
    // has no lineage and no display name, and one case carries a
    // `subagent_display_name` field on a canonical plaintext `send` envelope,
    // proving that metadata does not affect the media-type decision.
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let child = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let model_mail = |id: &str, payload: &str, extension_fields: Vec<(String, String)>| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields,
    };
    for (id, payload, extension_fields) in [
        ("model-mail-1", "child report", Vec::new()),
        (
            "model-mail-2",
            "named child report",
            vec![("subagent_display_name".to_string(), "\"kid\"".to_string())],
        ),
    ] {
        let envelope = model_mail(id, payload, extension_fields);
        let sender = envelope.sender.agent_id.clone();
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender, envelope, MessageScope::Session, now_ms)
            .unwrap();
    }
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "model peer mail still starts the parent's message-triggered turn"
    );
    let received = peer_echo_pane_lines(&service, "%1");
    assert!(
        received
            .iter()
            .any(|line| line == "▐ agent-%3> child report"),
        "a model-authored inbound message from a child with no display name keeps its \
         echo: {received:#?}"
    );
    assert!(
        received
            .iter()
            .any(|line| line == "▐ agent-%3> named child report"),
        "a `subagent_display_name` extension on a `send` envelope never suppresses the \
         echo: {received:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies committed direct-parent messages retain the stable `parent` label
/// across a parent-pane rename while every non-direct sender falls back to its
/// ordinary endpoint label.
///
/// This drives the receiver commit path rather than only the echo helper. Two
/// parent envelopes commit into the child's active turn on opposite sides of a
/// parent title rename, proving presentation consults exact recipient lineage
/// instead of the mutable title. The predicate assertions separately preserve
/// exactness for sibling, unrelated, and grandparent identities, accept durable
/// restored lineage without treating it as live authority, and reject the same
/// edge once a parent conversation fence makes it stale.
#[test]
fn runtime_direct_parent_peer_message_uses_stable_label_only_for_valid_exact_lineage() {
    let mut service = test_runtime_service();
    service
        .attach_primary(
            "parent before rename",
            true,
            Size::new(60, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_terminal_command(
            &service.session.layout_owner_client_id().cloned().unwrap(),
            "split-window; rename-pane child pane",
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    service.set_pane_screen(
        "%2".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "parent".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%3",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "sibling".to_string(),
            terminal: false,
        },
    );
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let parent = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    let child = service
        .ensure_runtime_message_identity(
            "agent-%2",
            PaneId::opaque("%2".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&child.agent_id)
        .unwrap();
    service
        .start_agent_prompt_turn("%2", "receive parent mail")
        .unwrap();
    let parent_message = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: parent.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(child.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("direct-parent-before-rename", "first parent instruction"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    service
        .session
        .set_pane_title_explicit("%1", "parent after rename")
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("direct-parent-after-rename", "second parent instruction"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );

    let received = peer_echo_pane_lines(&service, "%2");
    assert!(
        received.iter().any(|line| line == "▐ parent> first parent")
            && received.iter().any(|line| line == "▐      instruction"),
        "the first committed parent message must use the stable label: {received:#?}"
    );
    assert!(
        received
            .iter()
            .any(|line| line == "▐ parent> second parent")
            && received
                .iter()
                .filter(|line| line.as_str() == "▐      instruction")
                .count()
                == 2,
        "the renamed parent must retain the stable label: {received:#?}"
    );
    assert!(
        !received
            .iter()
            .any(|line| line.contains("parent before rename")
                || line.contains("parent after rename")),
        "parent pane titles must never replace the stable direct-parent label: {received:#?}"
    );

    for sender in ["agent-%3", "external-agent", "agent-root"] {
        assert!(
            !service.runtime_peer_message_sender_is_direct_parent("agent-%2", sender),
            "only the exact immediate parent can use the parent label: {sender}"
        );
    }
    assert!(service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"));
    for (sender, id, payload, expected_label) in [
        (
            "agent-%3",
            "sibling-fallback",
            "sibling evidence",
            "agent-%3",
        ),
        (
            "external-agent",
            "unrelated-fallback",
            "unrelated evidence",
            "external-agent",
        ),
        (
            "agent-root",
            "grandparent-fallback",
            "grandparent evidence",
            "agent-root",
        ),
    ] {
        let sender_identity = service
            .ensure_runtime_message_identity(sender, None, "agent", &[], now_ms)
            .unwrap();
        let sender_agent_id = sender_identity.agent_id.clone();
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.to_string(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender_identity,
            recipient: mez_agent::messaging::Recipient::Agent(child.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.to_string(),
            extension_fields: Vec::new(),
        };
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender_agent_id, envelope, MessageScope::Session, now_ms)
            .unwrap();
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            1
        );
        assert!(
            peer_echo_pane_lines(&service, "%2")
                .iter()
                .any(|line| line.starts_with(&format!("▐ {expected_label}>"))),
            "non-direct sender {sender} must keep its endpoint label"
        );
    }
    service.set_restored_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "restored child".to_string(),
            terminal: false,
        },
    );
    assert!(
        service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"),
        "restored lineage remains valid for presentation even though it is not live authority"
    );
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("restored-parent", "restored parent evidence"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        peer_echo_pane_lines(&service, "%2")
            .iter()
            .any(|line| line.starts_with("▐ parent> restored parent")),
        "validated restored lineage must retain the parent presentation alias"
    );
    service.fence_subagent_descendants_for_parent_conversation("agent-%1", "replacement");
    assert!(
        !service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"),
        "a fenced historical edge must fall back to ordinary endpoint labeling"
    );
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("fenced-parent", "fenced parent evidence"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        peer_echo_pane_lines(&service, "%2")
            .iter()
            .any(|line| line.starts_with("▐ parent after rename>")),
        "fenced lineage must use the renamed parent's ordinary endpoint label"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies outbound presentation uses a spawn-owned display name while an
/// unavailable recipient retains the model-authored recipient expression.
#[test]
fn runtime_outbound_recipient_display_label_preserves_spawn_name_and_fallback() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "CypherSol".to_string(),
            terminal: false,
        },
    );
    let named_recipient =
        crate::runtime::Recipient::Agent(AgentId::opaque("agent-%2".to_string()).unwrap());
    assert_eq!(
        service.runtime_outbound_recipient_display_label(&named_recipient, "agent:%2"),
        "CypherSol"
    );

    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "agent-%2".to_string(),
            terminal: false,
        },
    );
    assert_eq!(
        service.runtime_outbound_recipient_display_label(&named_recipient, "agent:%2"),
        "agent-%2",
        "literal name mode must retain its assigned identity"
    );

    let unavailable_recipient =
        crate::runtime::Recipient::Agent(AgentId::opaque("agent-missing".to_string()).unwrap());
    assert_eq!(
        service.runtime_outbound_recipient_display_label(
            &unavailable_recipient,
            "agent:agent-missing",
        ),
        "agent:agent-missing"
    );
}

/// Verifies runtime-owned subagent bridge notifications for an idle parent
/// start no turn and leave scheduler and provider-task accounting unchanged.
///
/// `task_status`/`task_result` notifications are authored by the runtime's own
/// subagent lifecycle, not by a model `send_message` action, so they must wait
/// behind the durable cursor for the parent's next turn instead of waking an
/// idle parent with a peer-message turn; a later model-originated peer message
/// still starts exactly one turn and injects both blocks.
#[test]
fn runtime_idle_agent_runtime_owned_bridge_notifications_start_no_turn() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let parent_identity = service
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
        .subscribe_from_retained_start(&parent_identity.agent_id)
        .unwrap();
    let child_identity = service
        .ensure_runtime_message_identity("agent-%2", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let status_envelope = Envelope {
        protocol: "mmp/1",
        id: "turn-child:task_status:started".to_string(),
        message_type: "task_status".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child_identity.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(parent_identity.agent_id.clone()),
        correlation_id: Some("turn-child".to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: mez_agent::messaging::TaskStatusPayload {
            task_id: "turn-child".to_string(),
            state: mez_agent::messaging::TaskState::Running,
            progress_percent: Some(0),
            summary: "subagent task started".to_string(),
        }
        .to_json(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &child_identity.agent_id,
            status_envelope,
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(
        service.agent_turn_ledger().turns().is_empty(),
        "runtime-owned bridge traffic must not start a turn for an idle parent"
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&parent_identity.agent_id)
            .unwrap()
            .last_sequence,
        0
    );

    let peer_envelope = Envelope {
        protocol: "mmp/1",
        id: "model-peer-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child_identity.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(parent_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "model-originated peer request".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &child_identity.agent_id,
            peer_envelope,
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "the model-originated turn carries the pending runtime-owned notification"
    );
    let parent_turns: Vec<_> = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.agent_id == parent_identity.agent_id.as_str())
        .cloned()
        .collect();
    assert_eq!(
        parent_turns.len(),
        1,
        "a model-originated peer message starts exactly one turn"
    );
    let context = service
        .agent_turn_contexts()
        .get(&parent_turns[0].turn_id)
        .unwrap();
    assert!(
        context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::PeerMessage
                && block.label.contains("task_status")
                && block.content.contains("subagent task started")
        }),
        "pending runtime-owned notifications are still injected"
    );
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::PeerMessage
            && block.content.contains("model-originated peer request")
    }));
}

/// Verifies a peer message that expires before delivery starts no turn, is not
/// injected, and stays behind the durable cursor.
///
/// Expiry keeps the existing message-service semantics: an envelope already
/// expired at accept time is rejected for the sender, and an envelope that
/// expires while waiting is never acknowledged as delivered.
#[test]
fn runtime_expired_peer_message_is_not_injected_or_acknowledged() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "expired-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: Some(1),
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "expired peer evidence".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now_ms)
        .unwrap();

    let after_expiry = now_ms.saturating_add(5_000);
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(after_expiry)
            .unwrap(),
        0
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Verifies the dedicated peer-message delivery timer arms only while
/// deliverable mail exists, delivers pending mail when it fires, and stops
/// re-arming once the inbox drains.
#[test]
fn runtime_peer_message_delivery_timer_arms_delivers_and_stops() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "timed-message-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "timer-driven peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    let armed = service.peer_message_delivery_timer_transition(false, 7, now_ms);
    assert_eq!(armed.side_effects.len(), 1);
    match &armed.side_effects[0] {
        crate::runtime::RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
            assert_eq!(
                key.kind,
                crate::runtime::RuntimeTimerKind::PeerMessageDelivery
            );
            assert_eq!(key.generation, 7);
            assert!(*delay_ms > 0);
        }
        other => panic!("unexpected peer-message timer side effect: {other:?}"),
    }
    assert!(
        service
            .peer_message_delivery_timer_transition(true, 8, now_ms)
            .side_effects
            .is_empty(),
        "an active delivery timer must not be armed twice"
    );

    let applied = service
        .apply_peer_message_delivery_timer(now_ms, 7)
        .unwrap();
    assert!(applied.applied);
    assert!(
        applied.side_effects.is_empty(),
        "a drained inbox must not keep the delivery timer armed"
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage),
        "timer delivery must start the idle agent's message-triggered turn"
    );
}

/// Verifies the configured peer-message loop limit stops further
/// message-triggered turns while leaving inbox mail pending behind the durable
/// cursor.
#[test]
fn runtime_peer_message_loop_limit_stops_message_turns() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "limited-message-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "loop-limited peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        0,
        "loop-limited mail stays behind the durable cursor"
    );
}

/// Verifies an objective refresh that carries no objective is a no-op: the
/// published value and the presence timestamp survive it unchanged.
#[test]
fn runtime_absent_objective_refresh_keeps_published_value() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the objective contract")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    let published = service
        .message_service()
        .registered_identity(&agent_id)
        .and_then(|identity| identity.objective.clone());
    assert_eq!(published.as_deref(), Some("inspect the objective contract"));
    let published_at_ms = service
        .message_service()
        .presence()
        .into_iter()
        .find(|record| record.identity.agent_id == agent_id)
        .map(|record| record.updated_at_ms)
        .expect("published presence record");

    assert!(!service.publish_prepared_runtime_agent_objective(started.agent_id.as_str(), None));

    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.clone()),
        published
    );
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == agent_id)
            .map(|record| record.updated_at_ms),
        Some(published_at_ms),
        "an absent objective refresh must not churn presence"
    );
}

/// Verifies the model-generated objective path is live: an objective carried by
/// the turn envelope is published and wins over the prompt-derived fallback.
#[test]
fn runtime_model_authored_objective_wins_over_prompt_fallback() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the fallback objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let action = mez_agent::AgentAction {
        id: "list-agents-objective".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Review the peer discovery bounds","actions":[{"type":"list_agents"}]}"#.to_string();

    assert!(service.publish_runtime_agent_objective_for_response(&turn, &execution));

    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Review the peer discovery bounds"),
        "the model-authored objective must win over the prompt fallback"
    );
}

/// Verifies each caller resolves durable objective metadata exactly once before
/// publishing it, so a second-read failure or concurrent metadata replacement
/// cannot discard a prepared prompt, model-response, or sender identity value.
///
/// The storage probe fails only the second read after it is armed. Each path
/// must therefore publish the first resolved durable override and must not ask
/// the raw publisher to resolve persistence again after preparation.
#[test]
fn runtime_prepared_objectives_survive_would_be_second_metadata_reads() {
    let root = temp_root("runtime-prepared-objective-single-read");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
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
    store
        .save_user_objective(&conversation_id, Some("Prepared durable objective"))
        .unwrap();

    store.fail_second_subsequent_user_objective_read();
    let started = service
        .start_agent_prompt_turn("%1", "Automatic prompt objective")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Prepared durable objective"),
        "prompt start must publish its prepared durable value"
    );

    let turn = messaging_test_turn(&service, &started.turn_id);
    let action = mez_agent::AgentAction {
        id: "prepared-objective-response".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Model replacement","actions":[{"type":"list_agents"}]}"#.to_string();
    store.fail_second_subsequent_user_objective_read();
    assert!(!service.publish_runtime_agent_objective_for_response(&turn, &execution));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Prepared durable objective"),
        "model response must retain the prepared durable override"
    );

    store.fail_second_subsequent_user_objective_read();
    let sender = service.runtime_message_sender_identity(&turn).unwrap();
    assert_eq!(
        sender.objective.as_deref(),
        Some("Prepared durable objective"),
        "sender identity must return its prepared durable override"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies unreadable durable metadata blocks both prompt and model objective
/// refreshes, preserving the already-published identity rather than admitting
/// an automatic overwrite through either path.
#[test]
fn runtime_unreadable_objective_metadata_preserves_published_prompt_and_model_values() {
    let root = temp_root("runtime-unreadable-objective-metadata");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "Preserve this published objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    let conversation_id = turn.conversation_id.clone();
    store
        .save_user_objective(&conversation_id, Some("Durable objective"))
        .unwrap();
    fs::write(
        root.join(&conversation_id).join("metadata.json"),
        b"not valid metadata\n",
    )
    .unwrap();

    let prompt_error = service
        .start_agent_prompt_turn("%1", "Do not replace the published objective")
        .unwrap_err();
    assert!(
        prompt_error
            .message()
            .contains("conversation objective metadata is unavailable")
    );

    let action = mez_agent::AgentAction {
        id: "list-agents-corrupt-objective".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Model overwrite","actions":[{"type":"list_agents"}]}"#.to_string();
    assert!(!service.publish_runtime_agent_objective_for_response(&turn, &execution));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Preserve this published objective")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies sender identity is read after its objective refresh, so an
/// immediately dispatched message carries the durable user override instead of
/// the stale identity returned by registration.
#[test]
fn runtime_message_sender_identity_returns_the_post_refresh_objective() {
    let root = temp_root("runtime-message-sender-objective-refresh");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "Initial automatic objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    store
        .save_user_objective(
            &turn.conversation_id,
            Some("Immediate durable sender objective"),
        )
        .unwrap();

    let sender = service.runtime_message_sender_identity(&turn).unwrap();

    assert_eq!(
        sender.objective.as_deref(),
        Some("Immediate durable sender objective")
    );
    let _ = fs::remove_dir_all(root);
}

/// Builds one pending model-originated peer message for the loop-limit tests.
fn limited_peer_message(service: &mut crate::runtime::RuntimeSessionService, now_ms: u64) {
    let recipient_identity = service
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
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "loop-limit-episode-message".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "loop-limit episode evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
}

/// Verifies the peer-message loop limit is a stable episode: no turn starts, the
/// delivery timer stops re-arming, and the limit diagnostic is emitted once
/// rather than once per tick.
#[test]
fn runtime_peer_message_loop_limit_is_a_stable_episode() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    limited_peer_message(&mut service, now_ms);

    for _ in 0..2 {
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            0
        );
    }
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert!(service.agent_peer_message_limit_reported("agent-%1"));
    assert!(
        service
            .peer_message_delivery_timer_transition(false, 11, now_ms)
            .side_effects
            .is_empty(),
        "limit-blocked mail must not keep re-arming the delivery timer"
    );
    let diagnostics = service
        .event_log()
        .expect("event log")
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| event.payload.contains("peer_message_loop_limit"))
        .count();
    assert_eq!(
        diagnostics, 1,
        "the limit diagnostic must be emitted once per episode"
    );
}

/// Verifies peer mail flows again after the defined reset trigger: direct user
/// input clears the counter and the limit episode, so delivery re-arms and a
/// message-triggered turn starts.
#[test]
fn runtime_peer_mail_flows_again_after_direct_user_input_reset() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    limited_peer_message(&mut service, now_ms);
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(
        service
            .peer_message_delivery_timer_transition(false, 12, now_ms)
            .side_effects
            .is_empty()
    );

    service.reset_agent_peer_message_turns("agent-%1");

    assert!(!service.agent_peer_message_limit_reported("agent-%1"));
    assert_eq!(
        service
            .peer_message_delivery_timer_transition(false, 12, now_ms)
            .side_effects
            .len(),
        1,
        "pending peer mail must re-arm delivery once the limit episode ends"
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage),
        "peer mail must start a message-triggered turn after the reset"
    );
}

/// Verifies retained message subscriptions survive a process-style snapshot
/// restore, commit unread messages once in sequence order before the next
/// prompt, and do not replay them after a second restore.
#[test]
fn runtime_local_message_cursor_restores_exactly_once_in_arrival_order() {
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let mut before_restart = test_runtime_service();
    let recipient_identity = before_restart
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    before_restart
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = before_restart
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    for (id, payload) in [
        ("restart-message-1", "first retained message"),
        ("restart-message-2", "second retained message"),
    ] {
        before_restart
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: id.to_string(),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(
                        recipient_identity.agent_id.clone(),
                    ),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: payload.to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now_ms,
            )
            .unwrap();
    }
    let restored_messages = mez_agent::messaging::MessageService::from_snapshot_state(
        &before_restart.control.message_service().snapshot_state(),
    )
    .unwrap();
    let mut after_restart = RuntimeSessionService::from_parts(
        SessionFixture::new().build(),
        PathBuf::from("/tmp/mez-1000/message-restart.sock"),
        100,
        ControlIdempotencyCache::default(),
        restored_messages,
        None,
    )
    .unwrap();
    after_restart
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let started = after_restart
        .start_agent_prompt_turn("%1", "use all retained messages")
        .unwrap();
    let context = after_restart
        .agent_turn_contexts()
        .get(&started.turn_id)
        .unwrap();
    let ordered_labels = context
        .blocks()
        .iter()
        .map(|block| block.label.as_str())
        .collect::<Vec<_>>();
    let first = ordered_labels
        .iter()
        .position(|label| label.contains("restart-message-1"))
        .unwrap();
    let second = ordered_labels
        .iter()
        .position(|label| label.contains("restart-message-2"))
        .unwrap();
    let prompt = ordered_labels
        .iter()
        .position(|label| *label == "user prompt")
        .unwrap();
    assert!(first < second && second < prompt, "{ordered_labels:?}");
    assert_eq!(
        after_restart
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        2
    );

    let restored_again = mez_agent::messaging::MessageService::from_snapshot_state(
        &after_restart.control.message_service().snapshot_state(),
    )
    .unwrap();
    let mut second_restart = RuntimeSessionService::from_parts(
        SessionFixture::new().build(),
        PathBuf::from("/tmp/mez-1000/message-restart-2.sock"),
        100,
        ControlIdempotencyCache::default(),
        restored_again,
        None,
    )
    .unwrap();
    second_restart
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let replay_check = second_restart
        .start_agent_prompt_turn("%1", "do not replay old messages")
        .unwrap();
    assert!(
        second_restart
            .agent_turn_contexts()
            .get(&replay_check.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .all(|block| !block.label.contains("restart-message-"))
    );
}

/// Verifies macro step slash commands are dispatched through the child agent shell.
///
/// This protects slash-command compatibility for macro steps: a step containing
/// `/loop` must not be delivered as a passive MMP message because that would
/// bypass the subagent shell parser and break the feature contract.
#[test]
fn runtime_agent_macro_send_message_queues_child_shell_turn() {
    let config_root = temp_root("runtime-macro-step-message");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. /loop inspect release notes for the requested version.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let child_agent_id = service
        .macro_managed_subagent_ids()
        .into_iter()
        .next()
        .expect("macro child should be registered");
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    assert_eq!(parent_turn.state, AgentTurnState::Blocked);
    let parent_execution = service
        .agent_turn_executions()
        .get(&parent_turn.turn_id)
        .expect(
            "parent macro orchestration execution should be waiting on runtime-owned first step",
        );
    assert_eq!(parent_execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        parent_execution.action_results[0].status,
        ActionStatus::Running
    );
    let structured = parent_execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap_or_default();
    assert!(
        structured.contains(r#""join_policy":"macro_step""#),
        "{structured}"
    );
    assert!(structured.contains(&child_agent_id), "{structured}");
    assert!(
        service
            .message_service()
            .receive_for(&AgentId::opaque(child_agent_id.clone()).unwrap(), u64::MAX)
            .is_empty()
    );
    let child_pane_id = child_agent_id
        .strip_prefix("agent-")
        .expect("macro child agent should identify its pane");
    let child_turn_id = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == child_pane_id)
        .map(|(turn_id, _)| turn_id.clone())
        .expect("runtime-owned /loop macro step should queue loop work");
    let loop_completion = service
        .agent_loop_state(child_pane_id)
        .and_then(|state| state.completion.as_ref())
        .expect("runtime-owned loop should retain the macro parent completion");
    assert_eq!(loop_completion.child_agent_id, child_agent_id);
    assert!(!service.has_joined_subagent_dependency(&child_turn_id));
    let macro_run = service
        .macro_run_for_tests(parent_turn.turn_id.as_str())
        .expect("macro run state should be keyed by parent turn");
    assert_eq!(macro_run.current_step, 0);
    assert_eq!(
        macro_run.steps[0].child_turn_id.as_deref(),
        Some(child_turn_id.as_str())
    );
    assert_eq!(
        service.macro_parent_turn_for_child(child_turn_id.as_str()),
        Some(&parent_turn.turn_id)
    );
    assert!(
        macro_run.steps[0]
            .submitted_prompt
            .as_deref()
            .unwrap_or_default()
            .contains("User additional context for this macro invocation:\nfor v1.2")
    );
    let child_pane_id = child_agent_id.strip_prefix("agent-").unwrap();
    let child_pane_text = service
        .pane_screen(child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        child_pane_text.contains("user> /loop inspect release notes for the requested version."),
        "{child_pane_text}"
    );
    let child_context = service.agent_turn_contexts().get(&child_turn_id).unwrap();
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("inspect release notes for the requested version.")
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("User additional context for this macro invocation:\nfor v1.2")
    }));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == child_turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert_eq!(
        service
            .agent_loop_turns_for_tests()
            .values()
            .filter(|loop_turn| loop_turn.pane_id == child_pane_id)
            .count(),
        1
    );
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that MAAP `send_message` still reaches the shared message queue
/// when its media metadata is valid. This protects the accepted text path while
/// invalid media handling is tightened to match MMP transport validation.
#[test]
fn runtime_executes_send_message_action_through_message_service() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain; charset=utf-8", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let structured: serde_json::Value = serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("delivery structured content"),
    )
    .unwrap();
    assert_eq!(structured["scope"], "session");
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""delivery_status":"accepted""#)
    );
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Malformed recipient arguments must yield durable correction feedback without
/// delivery; a subsequent model-authored correction delivers exactly once.
#[test]
fn runtime_invalid_message_recipient_queues_correction_without_delivery() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(
        result.error.as_ref().unwrap().code,
        "invalid_message_recipient"
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .is_some_and(|structured| structured.contains(r#""scope":"session""#))
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("accepted_recipient_forms")
    );
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    let mut response = execution.response.clone();
    let action = &mut response.action_batch.as_mut().unwrap().actions[0];
    action.id = "msg-corrected".to_string();
    if let mez_agent::AgentActionPayload::SendMessage { recipient, .. } = &mut action.payload {
        *recipient = format!("agent:{target}");
    }
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(corrected.action_results[0].status, ActionStatus::Succeeded);
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "handoff");
}

/// An unavailable recipient is model-correctable: the failed action remains
/// durable context and one corrected recipient choice delivers exactly once.
#[test]
fn runtime_unavailable_message_recipient_queues_correction_without_delivery() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(
        result.error.as_ref().unwrap().code,
        "message_recipient_unavailable"
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(r#""code":"message_recipient_unavailable""#))
    );
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );

    let mut response = execution.response.clone();
    let action = &mut response.action_batch.as_mut().unwrap().actions[0];
    action.id = "msg-corrected".to_string();
    if let mez_agent::AgentActionPayload::SendMessage { recipient, .. } = &mut action.payload {
        *recipient = format!("agent:{target}");
    }
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(corrected.action_results[0].status, ActionStatus::Succeeded);
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "handoff");
}

/// Repeating an unavailable recipient consumes the correction budget instead
/// of retrying delivery to the same absent agent indefinitely.
#[test]
fn runtime_unavailable_message_recipient_exhausts_correction_budget() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    service.set_agent_action_failure_retry_limit(1);
    let mut response = execution.response.clone();
    response.action_batch.as_mut().unwrap().actions[0].id = "msg-repeated".to_string();

    let repeated = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);

    assert_eq!(repeated.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// Repeating an invalid recipient consumes the existing correction budget and
/// terminates without delivering messages or scheduling unbounded continuations.
#[test]
fn runtime_invalid_message_recipient_exhausts_correction_budget() {
    let (mut service, execution, target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    service.set_agent_action_failure_retry_limit(1);
    let mut response = execution.response.clone();
    response.action_batch.as_mut().unwrap().actions[0].id = "msg-repeated".to_string();
    let repeated = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchProvider { response }, 1)
        .unwrap()
        .remove(0);
    assert_eq!(repeated.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );
}

/// A joined child must retain a partially successful batch while correcting an
/// invalid parent recipient, then deliver and hand off each effect exactly once.
///
/// This composes recipient correction with the spawned-child lifecycle. The
/// successful sibling action must not be replayed during correction, the child
/// must keep both first-attempt results as provider context, and its corrected
/// `agent:<parent-id>` delivery must not displace the normal joined handoff.
#[test]
fn runtime_spawned_child_corrects_parent_recipient_without_replaying_sibling() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(2)
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let sibling_target = AgentId::opaque("agent-message-sibling").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: sibling_target.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: Some("worker".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();

    let parent = service
        .start_agent_prompt_turn("%1", "delegate messaging")
        .unwrap();
    let spawn_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn messaging child".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "delegate the messaging task".to_string(),

                actions: vec![runtime_spawn_agent_action(
                    "spawn-messaging-child",
                    "send both handoff messages",
                )],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let _spawned_execution = service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &spawn_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id)
        .cloned()
        .expect("spawned child turn");
    assert_eq!(
        child.state,
        AgentTurnState::Running,
        "{child:?}; execution={:?}",
        service.agent_turn_executions().get(&child.turn_id)
    );
    // The child uses its own effective approval policy; this scenario exercises
    // sibling-then-parent delivery ordering rather than the approval gate.
    service.set_pane_approval_policy_override(
        &child.pane_id,
        Some(mez_agent::ApprovalPolicy::AutoAllow),
    );

    let send_action = |id: &str, recipient: String, payload: &str| mez_agent::AgentAction {
        id: id.to_string(),

        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient,
            scope: Some("session".to_string()),
            content_type: "text/plain".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "send sibling and parent messages".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "send the completed sibling before the parent handoff".to_string(),

                actions: vec![
                    send_action(
                        "message-sibling-once",
                        format!("agent:{sibling_target}"),
                        "completed sibling delivery",
                    ),
                    send_action(
                        "message-parent-invalid",
                        "parent".to_string(),
                        "child handoff",
                    ),
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first = service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert!(first.action_results.iter().any(|result| {
        result.action_id == "message-sibling-once" && result.status == ActionStatus::Succeeded
    }));
    assert!(first.action_results.iter().any(|result| {
        result.action_id == "message-parent-invalid"
            && result.status == ActionStatus::Failed
            && result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "invalid_message_recipient")
    }));
    assert_eq!(
        service
            .message_service()
            .receive_for(&sibling_target, u64::MAX)
            .len(),
        1
    );
    let child_context = runtime_prepared_context_for_turn(&service, &child.turn_id);
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("[action_result message-sibling-once send_message succeeded]")
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block
            .content
            .contains("[action_result message-parent-invalid send_message failed]")
            && block.content.contains("invalid_message_recipient")
    }));
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| { task.turn_id == child.turn_id })
    );

    let corrected_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "correct the parent recipient".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "deliver the corrected parent handoff".to_string(),

                actions: vec![send_action(
                    "message-parent-corrected",
                    format!("agent:{}", parent.agent_id),
                    "child handoff",
                )],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let corrected = service
        .poll_agent_provider_tasks_with_provider(&corrected_provider, 1)
        .unwrap();
    assert_eq!(corrected.len(), 1);
    assert_eq!(corrected[0].terminal_state, AgentTurnState::Running);
    assert_eq!(
        service
            .message_service()
            .receive_for(&sibling_target, u64::MAX)
            .len(),
        1
    );
    let parent_id = AgentId::opaque(parent.agent_id.clone()).unwrap();
    let parent_messages = service.message_service().receive_for(&parent_id, u64::MAX);
    assert_eq!(
        parent_messages
            .iter()
            .filter(|message| message.payload == "child handoff")
            .count(),
        1
    );
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    let terminal_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "Child work is complete.",
            true,
        ),
    };
    let terminal = service
        .poll_agent_provider_tasks_with_provider(&terminal_provider, 1)
        .unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].terminal_state, AgentTurnState::Completed);
    assert!(!service.has_joined_subagent_dependency(&child.turn_id));
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert_eq!(
        parent_context
            .blocks()
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::PeerMessage
                    && block.content.contains("child handoff")
            })
            .count(),
        1
    );
    assert_eq!(
        parent_context
            .blocks()
            .iter()
            .filter(|block| block.label == "action result spawn-messaging-child")
            .count(),
        1
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| { task.turn_id == parent.turn_id })
    );
    service.terminate_all_pane_processes().unwrap();
}

/// The text/plain shorthand is canonicalized before accepted MMP delivery;
/// correcting malformed recipients must not change this valid payload behavior.
#[test]
fn runtime_canonicalizes_send_message_text_plain_alias() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` uses the same text, JSON, and binary
/// payload metadata validation as the MMP transport endpoint. Rejected actions
/// must not enqueue messages because the agent-facing action result is the
/// durable protocol feedback for the failed local delivery.
#[test]
fn runtime_rejects_send_message_action_with_invalid_mmp_payload_metadata() {
    let cases = [
        (
            "text/html",
            "hello worker",
            "MMP text payloads require text/plain; charset=utf-8 or text/markdown",
        ),
        (
            "application/json",
            "not-json",
            "MMP JSON payload must be valid JSON",
        ),
        (
            "application/octet-stream",
            "AQID",
            "MMP binary payloads require payload_encoding base64",
        ),
    ];

    for (content_type, payload, expected_message) in cases {
        let (service, execution, target_agent) =
            execute_runtime_send_message_action(content_type, payload);

        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        let result = &execution.action_results[0];
        assert_eq!(result.status, ActionStatus::Failed);
        assert!(result.is_error);
        assert_eq!(
            result.error.as_ref().map(|error| error.code.as_str()),
            Some("invalid_message_payload")
        );
        assert_eq!(
            result.error.as_ref().map(|error| error.message.as_str()),
            Some(expected_message)
        );
        let structured = result.structured_content_json.as_deref().unwrap();
        assert!(structured.contains(r#""scope":"session""#));
        assert!(structured.contains(r#""delivery_status":"rejected""#));
        assert!(structured.contains(r#""code":"invalid_params""#));
        assert!(structured.contains(expected_message), "{structured}");
        assert!(
            service
                .message_service()
                .receive_for(&target_agent, u64::MAX)
                .is_empty()
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1")
        );
        let context = service.agent_turn_contexts().get("turn-1").unwrap();
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("[action_result msg-1 send_message failed]")
                && block.content.contains("invalid_message_payload")
        }));
        assert!(
            context
                .blocks()
                .iter()
                .all(|block| block.source != ContextSourceKind::RuntimeHint)
        );
    }
}

/// Verifies that MAAP `send_message` accepts valid JSON payloads through the
/// same shared validator. This catches accidental text-only validation when the
/// action path is kept in sync with MMP transport dispatch.
#[test]
fn runtime_accepts_send_message_action_with_valid_json_payload() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("application/json", r#"{"status":"ok"}"#);

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "application/json");
    assert_eq!(messages[0].payload, r#"{"status":"ok"}"#);
}

/// Verifies the per-agent objective refreshes from the turn prompt through the
/// same identity registry discovery reads, that an unchanged republish performs
/// no write, and that a changed value republishes once.
#[test]
fn runtime_agent_objective_refresh_is_bounded_and_throttled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "  inspect   the   discovery contract ")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("inspect the discovery contract")
    );
    let published_at_ms = service
        .message_service()
        .presence()
        .into_iter()
        .find(|record| record.identity.agent_id == agent_id)
        .map(|record| record.updated_at_ms)
        .unwrap();

    assert!(!service.publish_prepared_runtime_agent_objective(
        started.agent_id.as_str(),
        Some("inspect the discovery contract")
    ));
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == agent_id)
            .map(|record| record.updated_at_ms),
        Some(published_at_ms)
    );

    assert!(service.publish_prepared_runtime_agent_objective(
        started.agent_id.as_str(),
        Some("Review the discovery contract")
    ));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Review the discovery contract")
    );
}

/// Verifies the prompt-derived fallback objective reuses the shared bounds and
/// publishes nothing when the prompt text cannot satisfy them.
#[test]
fn runtime_agent_objective_fallback_honors_shared_bounds() {
    assert_eq!(
        RuntimeSessionService::runtime_agent_objective_from_prompt("  Inspect\t the   backlog ")
            .as_deref(),
        Some("Inspect the backlog")
    );
    assert!(RuntimeSessionService::runtime_agent_objective_from_prompt("   ").is_none());
    assert!(
        RuntimeSessionService::runtime_agent_objective_from_prompt("inspect\u{7}the pane")
            .is_none()
    );
}

/// Verifies a turn without a model-authored objective still derives one from
/// its own prompt context, and keeps the previous published objective when a
/// refresh cannot be bounded.
#[test]
fn runtime_agent_turn_objective_uses_turn_prompt_context() {
    let mut service = test_runtime_service();
    let turn = AgentTurnRecord {
        turn_id: "turn-objective".to_string(),
        conversation_id: "conversation-objective".to_string(),
        agent_id: "agent-%9".to_string(),
        pane_id: "%9".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 2,
        policy_profile: "runtime".to_string(),
        model_profile: "test".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Queued,
        initial_capability: None,
    };
    service.agent_turn_contexts_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentContext::new(vec![ContextBlock::user_event(
            "user prompt",
            "  inspect   the turn context ",
        )])
        .unwrap(),
    );
    assert_eq!(
        service.runtime_agent_turn_objective(&turn).as_deref(),
        Some("inspect the turn context")
    );

    service.agent_turn_contexts_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentContext::new(vec![ContextBlock::user_event(
            "user prompt",
            "inspect\u{7}the turn context",
        )])
        .unwrap(),
    );
    assert!(service.runtime_agent_turn_objective(&turn).is_none());
}

/// Returns the ledger turn with the supplied identity.
fn messaging_test_turn(
    service: &crate::runtime::RuntimeSessionService,
    turn_id: &str,
) -> mez_agent::AgentTurnRecord {
    service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .expect("messaging test turn")
}

/// Builds a running execution carrying one planned action result.
fn messaging_test_execution(
    turn: &mez_agent::AgentTurnRecord,
    action: &mez_agent::AgentAction,
    result: mez_agent::ActionResult,
    terminal_state: mez_agent::AgentTurnState,
) -> mez_agent::AgentTurnExecution {
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "messaging test batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "messaging test batch".to_string(),

                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![result],
        final_turn: false,
        terminal_state,
    }
}

/// Executes one read-only `list_agents` action and returns its structured result.
fn execute_list_agents_action(
    service: &mut crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
    agent_type: Option<&str>,
    scope: Option<&str>,
) -> serde_json::Value {
    let action = mez_agent::AgentAction {
        id: "list-agents-1".to_string(),

        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: agent_type.map(str::to_string),
            scope: scope.map(str::to_string),
        },
    };
    let planned =
        mez_agent::plan_action_result(turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(turn, &action, planned, mez_agent::AgentTurnState::Running);
    assert_eq!(
        service
            .execute_running_list_agents_actions_for_turn(turn, &mut execution)
            .unwrap(),
        1
    );
    serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("agent discovery structured content"),
    )
    .expect("agent discovery result json")
}

/// Verifies read-only agent discovery always includes the requesting agent,
/// defaults to primary agents only, and widens to internal controllers.
#[test]
fn runtime_list_agents_defaults_to_primary_and_includes_self() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover session peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    service
        .ensure_runtime_message_identity("agent-peer", None, "reviewer", &[], now_ms)
        .unwrap();
    service
        .ensure_runtime_message_identity("agent-internal", None, "worker", &[], now_ms)
        .unwrap();
    service.register_macro_managed_subagent(
        "agent-internal",
        &turn.turn_id,
        &turn.agent_id,
        "review",
    );

    let primary = execute_list_agents_action(&mut service, &turn, None, Some("session"));
    assert_eq!(primary["agent_type"], "primary");
    assert_eq!(primary["truncated"], false);
    let rows = primary["agents"].as_array().unwrap();
    assert!(rows.iter().all(|row| row["kind"] == "primary"));
    let self_row = rows
        .iter()
        .find(|row| row["is_self"] == true)
        .expect("requesting agent row");
    assert_eq!(self_row["agent_id"], turn.agent_id);
    assert!(rows.iter().any(|row| row["agent_id"] == "agent-peer"));
    assert!(!rows.iter().any(|row| row["agent_id"] == "agent-internal"));

    let internal =
        execute_list_agents_action(&mut service, &turn, Some("internal"), Some("session"));
    let internal_ids = internal["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(internal_ids, vec!["agent-internal".to_string()]);
    assert!(
        internal["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] == "internal")
    );

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let all_ids = all["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for agent_id in ["agent-peer", "agent-internal", turn.agent_id.as_str()] {
        assert!(all_ids.contains(&agent_id.to_string()), "{agent_id}");
    }

    let subagents =
        execute_list_agents_action(&mut service, &turn, Some("subagent"), Some("session"));
    assert_eq!(subagents["count"], 0);
    assert!(subagents["agents"].as_array().unwrap().is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Project-default discovery returns the requester and same-project peers,
/// while an explicit session scope widens the same list without exposing roots.
#[test]
fn runtime_list_agents_defaults_to_requester_project_and_session_widens() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover project peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let requester = service.runtime_message_sender_identity(&turn).unwrap();
    let project_scope = mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
        b"/workspace/requester-project",
    );
    service
        .message_service_mut()
        .rebind_agent_project_scope(&requester.agent_id, Some(project_scope.clone()))
        .unwrap();
    let same_project = mez_agent::messaging::SenderIdentity {
        agent_id: AgentId::opaque("agent-same-project").unwrap(),
        project_scope: Some(project_scope),
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    let other_project = mez_agent::messaging::SenderIdentity {
        agent_id: AgentId::opaque("agent-other-project").unwrap(),
        project_scope: Some(
            mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/other-project",
            ),
        ),
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    service
        .message_service_mut()
        .ensure_agent_identity(same_project.clone(), 0)
        .unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(other_project.clone(), 0)
        .unwrap();

    let project = execute_list_agents_action(&mut service, &turn, Some("all"), None);
    assert_eq!(project["scope"], "project");
    let project_ids = project["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(project_ids.contains(&turn.agent_id.as_str()));
    assert!(project_ids.contains(&same_project.agent_id.as_str()));
    assert!(!project_ids.contains(&other_project.agent_id.as_str()));

    let session = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    assert_eq!(session["scope"], "session");
    assert!(
        session["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["agent_id"] == other_project.agent_id.as_str())
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies persistent children expose bounded ownership metadata without
/// changing the authority represented by MMP discovery rows.
#[test]
fn runtime_list_agents_reports_persistent_parent_ownership() {
    let mut service = test_runtime_service();
    let parent_conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "discover my persistent MMP worker")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    service
        .ensure_runtime_message_identity("agent-%2", None, "worker", &["subagent"], now_ms)
        .unwrap();
    service.publish_prepared_runtime_agent_objective(
        "agent-%2",
        Some("Triage persistent peer requests"),
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: turn.agent_id.clone(),
            root_agent_id: turn.agent_id.clone(),
            depth: 1,
            display_name: "persistent worker".to_string(),
            terminal: false,
        },
    );
    service.set_persistent_subagent(
        "agent-%2",
        crate::runtime::RuntimePersistentSubagent {
            conversation_id: "persistent-child-conversation".to_string(),
            parent_agent_id: turn.agent_id.clone(),
            parent_conversation_id,
            objective: "Triage persistent peer requests".to_string(),
        },
    );

    let listed = execute_list_agents_action(&mut service, &turn, Some("subagent"), Some("session"));
    let row = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_id"] == "agent-%2")
        .expect("persistent child discovery row");
    assert_eq!(row["persistent"], true);
    assert_eq!(row["parent_agent_id"], turn.agent_id);
    assert_eq!(row["owned_by_self"], true);
    assert_eq!(row["objective"], "Triage persistent peer requests");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies discovery rows honor the documented row and string bounds and that
/// an unsupported agent-type filter is rejected instead of widened.
#[test]
fn runtime_list_agents_bounds_rows_and_rejects_unknown_agent_type() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover many peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    for index in 0..(mez_agent::AGENT_LIST_MAX_ROWS + 3) {
        service
            .ensure_runtime_message_identity(
                &format!("agent-peer-{index:03}"),
                None,
                "worker",
                &[],
                now_ms,
            )
            .unwrap();
    }

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let rows = all["agents"].as_array().unwrap();
    assert_eq!(rows.len(), mez_agent::AGENT_LIST_MAX_ROWS);
    assert_eq!(all["truncated"], true);
    for row in rows {
        for field in ["agent_id", "role", "pane_id", "window_id", "objective"] {
            if let Some(value) = row[field].as_str() {
                assert!(
                    value.len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES,
                    "{field}"
                );
            }
        }
        for capability in row["capabilities"].as_array().unwrap() {
            assert!(capability.as_str().unwrap().len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES);
        }
    }

    let action = mez_agent::AgentAction {
        id: "list-agents-1".to_string(),

        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("peers".to_string()),
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    assert!(
        service
            .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
            .is_err()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Plans one ask-mode message action and queues its resumable blocked approval.
fn block_runtime_send_message(
    service: &mut crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
    recipient: &str,
    payload: &str,
) -> (mez_agent::AgentAction, String) {
    let action = mez_agent::AgentAction {
        id: "message-1".to_string(),

        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: recipient.to_string(),
            scope: Some("session".to_string()),
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    let blocked = mez_agent::plan_action_result(
        turn,
        &action,
        mez_agent::ActionPlanningInput {
            approval_policy: mez_agent::ApprovalPolicy::Ask,
            message_rule_decision: Some(mez_agent::permissions::RuleDecision::Prompt),
            ..mez_agent::ActionPlanningInput::default()
        },
    )
    .expect("message plan");
    assert_eq!(blocked.status, ActionStatus::Blocked);
    let execution =
        messaging_test_execution(turn, &action, blocked, mez_agent::AgentTurnState::Blocked);
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    // Register the owning assistant execution so the settled message evidence
    // can be committed to the turn context on resumption.
    service
        .append_agent_execution_chronology(turn, &execution)
        .unwrap();
    let approval_ids = service
        .queue_blocked_approvals_for_execution(turn, &execution)
        .expect("queued message approval");
    assert_eq!(approval_ids.len(), 1);
    (action, approval_ids[0].clone())
}

/// Approves one queued blocked approval and returns its decided record.
fn approve_blocked_runtime_action(
    service: &mut crate::runtime::RuntimeSessionService,
    approval_id: &str,
) -> mez_agent::permissions::BlockedApprovalRequest {
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            approval_id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some("client-1".to_string()),
            current_unix_seconds(),
        )
        .expect("approve blocked action");
    service
        .blocked_approvals()
        .get(approval_id)
        .cloned()
        .expect("decided approval")
}

/// Verifies an ask-mode message blocks with a bounded approval payload and
/// delivers exactly once after `/approve` resumes it.
#[test]
fn runtime_send_message_approval_blocks_and_resumes_after_approve() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "message a peer agent")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let target = AgentId::opaque("agent-peer").unwrap();
    service
        .ensure_runtime_message_identity("agent-peer", None, "worker", &[], now_ms)
        .unwrap();

    let (_, approval_id) =
        block_runtime_send_message(&mut service, &turn, "agent:agent-peer", "hello peer");
    let approval = service
        .blocked_approvals()
        .get(&approval_id)
        .cloned()
        .expect("queued approval");
    assert_eq!(approval.action_kind, "send_message");
    assert_eq!(approval.action_summary, "send_message to agent:agent-peer");
    assert!(
        !approval
            .declared_effects
            .iter()
            .any(|effect| effect.contains("hello peer"))
    );

    let decided = approve_blocked_runtime_action(&mut service, &approval_id);
    let controller = mez_core::ids::ClientId::opaque("client-1".to_string()).unwrap();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
            .unwrap(),
        Some(1)
    );

    let stored = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .expect("resumed execution");
    assert_eq!(stored.action_results[0].status, ActionStatus::Succeeded);
    let structured: serde_json::Value = serde_json::from_str(
        stored.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("delivery structured content"),
    )
    .unwrap();
    assert_eq!(structured["scope"], "session");
    assert_eq!(structured["delivery_status"], "accepted");
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "hello peer");
    // The approval record is retained for audit while its resumable reference
    // is consumed by the resumed delivery.
    assert_eq!(
        service
            .blocked_approvals()
            .get(&approval_id)
            .expect("retained approval record")
            .state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an approved send is re-validated against the recipient and payload
/// identity it was approved for, and delivers nothing when either changed.
#[test]
fn runtime_send_message_approval_rejects_changed_recipient_or_payload() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "message a peer agent")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let target = AgentId::opaque("agent-peer").unwrap();
    let other = AgentId::opaque("agent-other").unwrap();
    for agent_id in ["agent-peer", "agent-other"] {
        service
            .ensure_runtime_message_identity(agent_id, None, "worker", &[], now_ms)
            .unwrap();
    }

    let (_, approval_id) =
        block_runtime_send_message(&mut service, &turn, "agent:agent-peer", "hello peer");
    let decided = approve_blocked_runtime_action(&mut service, &approval_id);
    let controller = mez_core::ids::ClientId::opaque("client-1".to_string()).unwrap();

    let mut execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .expect("blocked execution");
    let mez_agent::AgentActionPayload::SendMessage { payload, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *payload = "changed payload".to_string();
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    let error = service
        .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
        .expect_err("changed payload must not resume");
    assert!(error.message().contains("no longer matches"), "{error:?}");

    let mez_agent::AgentActionPayload::SendMessage { payload, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *payload = "hello peer".to_string();
    let mez_agent::AgentActionPayload::SendMessage { recipient, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *recipient = format!("agent:{other}");
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    let error = service
        .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
        .expect_err("changed recipient must not resume");
    assert!(error.message().contains("no longer matches"), "{error:?}");
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );

    let mez_agent::AgentActionPayload::SendMessage { recipient, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *recipient = "agent:agent-peer".to_string();
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution);
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies discovery rows enforce the documented capability bound and signal
/// shortened text instead of silently dropping it.
#[test]
fn runtime_list_agents_bounds_capabilities_and_signals_row_truncation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover an oversized peer")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let oversized = "c".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES + 64);
    let capabilities = (0..(mez_agent::AGENT_LIST_MAX_CAPABILITIES + 5))
        .map(|_| oversized.as_str())
        .collect::<Vec<_>>();
    service
        .ensure_runtime_message_identity("agent-oversized", None, "worker", &capabilities, now_ms)
        .unwrap();

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let rows = all["agents"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["agent_id"] == "agent-oversized")
        .expect("oversized peer row");
    let row_capabilities = row["capabilities"].as_array().unwrap();
    assert_eq!(
        row_capabilities.len(),
        mez_agent::AGENT_LIST_MAX_CAPABILITIES,
        "one row must carry at most the documented capability bound"
    );
    for capability in row_capabilities {
        assert!(capability.as_str().unwrap().len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES);
    }
    assert_eq!(
        row["truncated"], true,
        "a shortened row must signal its bounded text"
    );
    let self_row = rows
        .iter()
        .find(|row| row["is_self"] == true)
        .expect("self row");
    assert_eq!(self_row["truncated"], false);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the injected peer block and its provider label bound and sanitize
/// peer-supplied sender fields, so an oversized or evil identity cannot shape
/// framing text or the block label.
#[test]
fn runtime_peer_message_context_bounds_evil_sender_identity() {
    let oversized = "x".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES * 2);
    let evil_capability = format!(
        "caps {}",
        "y".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES)
    );
    let envelope = Envelope {
        protocol: "mmp/1",
        id: oversized.clone(),
        message_type: "send".to_string(),
        time: "runtime:1".to_string(),
        sender: mez_agent::messaging::SenderIdentity {
            agent_id: AgentId::opaque(oversized.clone()).unwrap(),
            project_scope: None,
            pane_id: None,
            window_id: None,
            role: Some(oversized.clone()),
            capabilities: vec![evil_capability.clone(); mez_agent::AGENT_LIST_MAX_CAPABILITIES + 2],
            objective: Some(oversized.clone()),
        },
        recipient: mez_agent::messaging::Recipient::Session,
        correlation_id: Some(oversized.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "bounded payload".to_string(),
        extension_fields: Vec::new(),
    };

    let content = crate::runtime::control::runtime_peer_message_context_content(&envelope);
    assert!(
        !content.contains(&oversized),
        "peer-supplied identity text must be bounded at render time"
    );
    assert!(!content.contains(&evil_capability));
    let bounded_capability = mez_agent::agent_list_bounded_text(&evil_capability);
    assert_eq!(
        content.matches(&bounded_capability).count(),
        mez_agent::AGENT_LIST_MAX_CAPABILITIES,
        "the injected block must carry at most the documented capability bound"
    );
    assert!(
        content.len()
            <= mez_agent::AGENT_LIST_MAX_STRING_BYTES
                * (mez_agent::AGENT_LIST_MAX_CAPABILITIES + 8)
    );

    let label = crate::runtime::control::runtime_peer_message_block_label(7, &oversized);
    assert!(label.starts_with("peer message sequence 7 id "));
    assert!(!label.contains(&oversized));
    assert!(label.len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES + 32);
}

/// A sparse early runtime identity must reconcile to pane-backed authoritative
/// metadata, while a later conflicting refresh cannot partially mutate either
/// the registered identity or its matching presence projection.
#[test]
fn runtime_identity_reconciliation_fills_placeholder_and_rejects_conflicting_repeat() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let agent_id = AgentId::opaque("agent-%1").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            mez_agent::messaging::SenderIdentity {
                agent_id: agent_id.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: None,
                capabilities: Vec::new(),
                objective: None,
            },
            7,
        )
        .unwrap();

    let authoritative = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &["agent-harness"],
            99,
        )
        .unwrap();
    assert_eq!(
        authoritative.pane_id.as_ref().map(PaneId::as_str),
        Some("%1")
    );
    assert!(authoritative.window_id.is_some());
    assert_eq!(authoritative.role.as_deref(), Some("agent"));
    assert_eq!(authoritative.capabilities, vec!["agent-harness"]);
    let presence_before = service
        .message_service()
        .presence()
        .into_iter()
        .find(|presence| presence.identity.agent_id == agent_id)
        .unwrap();

    let mut conflicting = authoritative.clone();
    conflicting.role = Some("worker".to_string());
    assert!(
        service
            .message_service_mut()
            .ensure_agent_identity(conflicting, 100)
            .is_err()
    );
    assert_eq!(
        service.message_service().registered_identity(&agent_id),
        Some(&authoritative)
    );
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|presence| presence.identity.agent_id == agent_id),
        Some(presence_before)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies user-prompt delivery refuses the 1,025th visible receiver receipt
/// before it changes the prompt ledger, context, or durable delivery cursor.
///
/// The receipt outbox is shared with snapshot persistence. Filling it with
/// reconstructible receipts must therefore leave the next prompt's peer mail
/// pending and keep the retained 1,024-entry snapshot projection valid.
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

/// Verifies a version-5 restore rejects 1,025 acknowledged visible retained
/// deliveries before inserting any reconstructed receipt. Legacy payloads have
/// no durable outbox, so reconstruction must reserve the shared receipt bound
/// atomically rather than truncate transport replay or leave partial state.
#[test]
fn runtime_v5_restore_rejects_oversized_peer_receipt_reconstruction_atomically() {
    let mut service = test_runtime_service();
    let mut message_state = service.message_service().snapshot_state();
    message_state.retention_messages = 1_025;
    *service.message_service_mut() = MessageService::from_snapshot_state(&message_state).unwrap();
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
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
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
                    recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
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

/// Verifies v6 snapshot startup drops an outbox receipt already settled in the
/// durable presentation log. The stale receipt must not consume shared outbox
/// capacity or re-enter the scheduling and rendering paths after restoration.
#[test]
fn runtime_v6_snapshot_restore_filters_durably_settled_peer_receipt() {
    let root = temp_root("runtime-v6-stale-peer-receipt");
    let store = AgentTranscriptStore::new(root);
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let identity = "peer-message recipient=agent-%1 sequence=1 id=stale-v6";
    store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: "conversation".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 80,
            style_names: vec!["user-prompt".to_string()],
            display_lines: vec!["▐ sender> already durable".to_string()],
            copy_lines: vec!["▐ sender> already durable".to_string()],
            ansi_text: None,
            source_text: Some(format!(
                r#"{{"direction":"received","receive_identity":"{identity}","peer":"sender","payload":"already durable","content_type":"text/plain; charset=utf-8","direct_parent":false}}"#
            )),
            source_content_type: Some(
                "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8"
                    .to_string(),
            ),
        })
        .unwrap();
    let receipt = crate::storage::snapshot::SnapshotUnsettledPeerPresentation {
        identity: identity.to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "already durable".to_string(),
        presentation_eligible: true,
        live_rendered: true,
    };

    service
        .restore_snapshot_unsettled_received_peer_message_presentations(&[receipt])
        .unwrap();
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty()
    );
}
