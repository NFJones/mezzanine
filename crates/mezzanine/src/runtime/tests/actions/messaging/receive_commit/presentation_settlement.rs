//! Receipt retirement, persistence and presentation settlement regressions.
//!
//! A durable receipt belongs only to its exact committing recipient and remains
//! pending until the visible presentation is durably settled.

use super::super::fixtures::peer_echo_pane_lines;
use super::*;

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
    service.set_subagent_lineage(
        sender_agent_id.to_string(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "captured sender name".to_string(),
            terminal: false,
        },
    );
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
    service.set_subagent_lineage(
        sender_agent_id.to_string(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "changed sender name".to_string(),
            terminal: false,
        },
    );
    let captured_receipt = service
        .snapshot_unsettled_received_peer_message_presentations()
        .into_iter()
        .find(|receipt| receipt.identity == identity)
        .expect("committed receiver receipt");
    assert_eq!(captured_receipt.peer_label, "captured sender name");
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
    let recovered_entries = store.inspect_presentation(&conversation_id).unwrap();
    let recovered_peer_entry = recovered_entries
        .iter()
        .find(|entry| {
            entry
                .source_text
                .as_deref()
                .is_some_and(|source| source.contains(identity.as_str()))
        })
        .expect("recovered receiver presentation entry");
    let recovered_source = recovered_peer_entry.source_text.as_deref().unwrap();
    assert!(recovered_source.contains("captured sender name"));
    assert!(!recovered_source.contains("changed sender name"));
    assert_eq!(
        recovered_entries
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
