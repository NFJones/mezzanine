//! Canonical peer-context commit, fault recovery and expiry regressions.
//!
//! A cursor advances only after its event is committed. Recovery cannot replay
//! that event or admit provider work based on an expired pending occurrence.

use super::super::fixtures::peer_echo_pane_lines;
use super::*;

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
