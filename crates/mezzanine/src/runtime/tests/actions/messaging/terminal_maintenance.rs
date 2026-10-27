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
            .apply_peer_message_delivery_timer(now_ms + 1001, 1)
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

/// Uses the one-occurrence custody budget for ordinary terminal controls.
fn parked_terminal_sender() -> (
    crate::runtime::RuntimeSessionService,
    mez_agent::AgentTurnRecord,
    AgentId,
) {
    parked_terminal_sender_with_capacity(1)
}

/// Creates a real native accepted send followed by a parked wait. The external
/// recipient has no native context and cannot accidentally commit its inbox.
/// A one-occurrence budget makes release observable through real admission.
fn parked_terminal_sender_with_capacity(
    capacity: usize,
) -> (
    crate::runtime::RuntimeSessionService,
    mez_agent::AgentTurnRecord,
    AgentId,
) {
    let mut service = test_runtime_service();
    *service.control.message_service_mut() = MessageService::with_pending_limits(
        1,
        4096,
        mez_agent::messaging::PendingDeliveryLimits {
            messages: capacity,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let target = service
        .ensure_runtime_message_identity(
            "external-target",
            None,
            "agent",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "send and wait")
        .unwrap();
    let mut provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "send and wait".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "await peer response".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "send-terminal".into(),
                    payload: mez_agent::AgentActionPayload::SendMessage {
                        recipient: format!("agent:{}", target.agent_id),
                        scope: Some("session".into()),
                        content_type: "text/plain".into(),
                        payload: "private original request".into(),
                        correlation_id: None,
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .unwrap()
            .action_results[0]
            .status,
        mez_agent::ActionStatus::Succeeded
    );
    provider.response.raw_text = "wait for the accepted request".into();
    provider.response.action_batch.as_mut().unwrap().actions = vec![mez_agent::AgentAction {
        id: "wait-terminal".into(),
        payload: mez_agent::AgentActionPayload::Wait,
    }];
    service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .unwrap()
            .action_results
            .len(),
        1
    );
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .unwrap()
            .action_results[0]
            .action_type,
        "wait"
    );
    let turn = super::fixtures::messaging_test_turn(&service, &turn.turn_id);
    (service, turn, target.agent_id)
}

/// Authoritative recipient retirement must arm the production timer even when
/// live fanout is empty. Its payload-free fact commits once to the original
/// parked send owner before custody is released; it cannot resume the wait or
/// alter accepted send success. The tiny budget then admits another real send.
#[test]
fn runtime_terminal_timer_hands_off_original_action_without_wake_and_releases_capacity() {
    let (mut service, turn, target) = parked_terminal_sender();
    let now = current_unix_millis();
    let sender = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let action = mez_agent::AgentAction {
        id: "capacity-probe".into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: format!("agent:{target}"),
            scope: Some("session".into()),
            content_type: "text/plain".into(),
            payload: "capacity probe".into(),
            correlation_id: None,
        },
    };
    // Capacity is provably unaccepted, not an ambiguous transport failure.
    let before_rejection = service.message_service().snapshot_state();
    let rejected = terminal_probe_send(&mut service, &turn, &action);
    assert_eq!(rejected.status, mez_agent::ActionStatus::Failed);
    assert_eq!(rejected.error.as_ref().unwrap().code, "capacity");
    let rejected_json: serde_json::Value =
        serde_json::from_str(rejected.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(rejected_json["delivery_status"], "rejected");
    assert_eq!(rejected_json["accepted"], false);
    assert_eq!(rejected_json["delivery_applied"], false);
    assert!(rejected_json["message_id"].is_null());
    assert_eq!(service.message_service().snapshot_state(), before_rejection);
    service
        .control
        .message_service_mut()
        .retire_agent_identity(&target);
    assert!(
        !service
            .peer_message_delivery_timer_transition(false, 1, now)
            .side_effects
            .is_empty()
    );
    let transition = service.apply_peer_message_delivery_timer(now, 1).unwrap();
    assert!(transition.applied);
    assert!(transition.side_effects.is_empty());
    for _ in 0..2 {
        service
            .apply_peer_message_delivery_timer(now + 1, 1)
            .unwrap();
    }
    let blocks = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .blocks();
    let outcomes = blocks
        .iter()
        .filter(|block| block.label.starts_with("mmp-delivery-outcome "))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].source,
        mez_agent::ContextSourceKind::RuntimeHint
    );
    assert!(outcomes[0].content.contains("recipient_retired"));
    assert!(!outcomes[0].content.contains("private original request"));
    assert!(
        service
            .control
            .message_service()
            .delivery_outcomes_for(&sender, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(
                |block| block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.label == "action result send-terminal"
                    && block.content.contains("accepted")
            )
    );
    let replacement = service
        .ensure_runtime_message_identity("external-next", None, "agent", &[], now + 2)
        .unwrap();
    let mut next = action;
    if let mez_agent::AgentActionPayload::SendMessage { recipient, .. } = &mut next.payload {
        *recipient = format!("agent:{}", replacement.agent_id);
    }
    assert_eq!(
        terminal_probe_send(&mut service, &turn, &next).status,
        mez_agent::ActionStatus::Succeeded
    );
}

/// Runs a separate native action attempt without replacing the parked owner's
/// accepted result, making one-slot admission/refusal observable.
fn terminal_probe_send(
    service: &mut crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
    action: &mez_agent::AgentAction,
) -> mez_agent::ActionResult {
    let pending = mez_agent::ActionResult::running(turn, action, Vec::new(), None);
    let mut execution =
        super::fixtures::messaging_test_execution(turn, action, pending, AgentTurnState::Running);
    service
        .execute_running_message_actions_for_turn(turn, &mut execution)
        .unwrap();
    execution.action_results.remove(0)
}

/// A missing original canonical context or a replaced current
/// conversation cannot inherit terminal evidence. The production timer retains
/// that unprovable owner's receipt through the finite horizon, then releases
/// custody without adding context, waking a provider or altering send success.
#[test]
fn runtime_terminal_timer_unprovable_owners_expire_without_cross_task_handoff() {
    for replaced_conversation in [false, true] {
        let (mut service, turn, target) = parked_terminal_sender();
        let sender = AgentId::opaque(turn.agent_id.clone()).unwrap();
        if replaced_conversation {
            service
                .agent_shell_store_mut()
                .ensure_session("%1")
                .unwrap()
                .session_id = "replacement-conversation".into();
        } else {
            service.agent_turn_contexts_mut().remove(&turn.turn_id);
        }
        service
            .control
            .message_service_mut()
            .retire_agent_identity(&target);
        let observed = current_unix_millis();
        let first = service
            .apply_peer_message_delivery_timer(observed, 1)
            .unwrap();
        assert!(!first.side_effects.is_empty());
        assert_eq!(
            service
                .control
                .message_service()
                .delivery_outcomes_for(&sender, 100)
                .unwrap()
                .len(),
            1
        );
        service
            .apply_peer_message_delivery_timer(observed + 599_999, 1)
            .unwrap();
        assert_eq!(
            service
                .control
                .message_service()
                .delivery_outcomes_for(&sender, 100)
                .unwrap()
                .len(),
            1
        );
        let last = service
            .apply_peer_message_delivery_timer(observed + 600_000, 1)
            .unwrap();
        assert!(last.side_effects.is_empty());
        assert!(
            service
                .control
                .message_service()
                .delivery_outcomes_for(&sender, 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            !service
                .control
                .message_service()
                .needs_delivery_outcome_maintenance()
                .unwrap()
        );
        assert!(
            service
                .agent_turn_contexts()
                .get(&turn.turn_id)
                .is_none_or(|context| context
                    .blocks()
                    .iter()
                    .all(|block| !block.label.starts_with("mmp-delivery-outcome ")))
        );
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
        assert_eq!(service.agent_scheduler().snapshot().running, 0);
    }
}

/// Conflicting canonical evidence cannot be treated as a committed terminal
/// handoff. The timer must fail closed with the exact lower receipt retained,
/// and the parked wait/provider fence must remain intact for later repair.
#[test]
fn runtime_terminal_conflicting_context_does_not_consume_receipt() {
    let (mut service, turn, target) = parked_terminal_sender();
    let sender = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = current_unix_millis();
    service
        .control
        .message_service_mut()
        .retire_agent_identity(&target);
    service
        .control
        .message_service_mut()
        .settle_delivery_outcomes_at(now)
        .unwrap();
    let receipt = service
        .control
        .message_service()
        .delivery_outcomes_for(&sender, 100)
        .unwrap()
        .remove(0);
    use sha2::Digest;
    let recipient_hash = sha2::Sha256::digest(receipt.recipient.recipient.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let label = format!(
        "mmp-delivery-outcome sequence={} recipient={recipient_hash} floor={}",
        receipt.sequence, receipt.recipient.registered_after_sequence
    );
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_reference_event(
            mez_agent::ContextSourceKind::RuntimeHint,
            label,
            "conflicting canonical evidence",
        )
        .unwrap();
    let error = service
        .apply_peer_message_delivery_timer(now + 1, 1)
        .unwrap_err();
    assert!(error.message().contains("canonical evidence conflicts"));
    assert_eq!(
        service
            .control
            .message_service()
            .delivery_outcomes_for(&sender, 100)
            .unwrap(),
        vec![receipt]
    );
    assert!(
        !service
            .peer_message_delivery_timer_transition(false, 1, now + 1)
            .side_effects
            .is_empty()
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
}

/// A settled terminal handoff must not become a permanent exact barrier in a
/// long-lived sender's context. Both canonical compaction paths must summarize
/// it, and subsequent timer ticks must neither recreate the consumed event nor
/// wake the parked task. This exercises the actual product-created event.
#[test]
fn runtime_terminal_review_handoff_can_be_compacted_without_reappearing() {
    for rolling in [false, true] {
        let (mut service, turn, target) = parked_terminal_sender();
        let now = current_unix_millis();
        service
            .control
            .message_service_mut()
            .retire_agent_identity(&target);
        service.apply_peer_message_delivery_timer(now, 1).unwrap();
        let context = service
            .agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .unwrap();
        let index = context
            .chronology()
            .iter()
            .position(|event| event.block().label.starts_with("mmp-delivery-outcome "))
            .unwrap();
        let summary = mez_agent::ContextBlock::reference_event(
            mez_agent::ContextSourceKind::Memory,
            "context compaction summary",
            "Original accepted peer delivery ended without recipient commit; no effects were retried.",
        );
        if rolling {
            context
                .compact_execution_ranges_into_summary(
                    std::iter::once(index..index + 1).collect(),
                    summary,
                )
                .unwrap();
        } else {
            context
                .compact_execution_ranges(vec![(index..index + 1, summary)])
                .unwrap();
        }
        context.validate_durable().unwrap();
        service
            .apply_peer_message_delivery_timer(now + 1, 1)
            .unwrap();
        let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
        assert!(
            context
                .blocks()
                .iter()
                .all(|block| !block.label.starts_with("mmp-delivery-outcome "))
        );
        assert!(
            context
                .blocks()
                .iter()
                .any(|block| block.label == "context compaction summary")
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
        assert_eq!(service.agent_scheduler().snapshot().active_capacity_used, 0);
    }
}

/// Earlier receipts without a live original native action cannot fill a broad
/// first page and hide a later accepted live action's terminal fact until both
/// expire. Exact-occurrence queries must hand off both native accepted sends
/// while preserving every one of the 101 earlier unowned receipts untouched.
#[test]
fn runtime_terminal_review_earlier_unowned_receipts_cannot_hide_current_action() {
    let (mut service, turn, target) = parked_terminal_sender_with_capacity(256);
    let sender_id = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let sender = service
        .control
        .message_service()
        .registered_identity(&sender_id)
        .unwrap()
        .clone();
    let now = current_unix_millis();
    let mut unowned = Vec::new();
    for index in 0..101 {
        let delivery = service
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender_id,
                Envelope {
                    protocol: "mmp/1",
                    id: format!("unowned-earlier-{index}"),
                    message_type: "send".into(),
                    time: format!("runtime:{now}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(target.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain".into(),
                    payload: String::new(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                now,
            )
            .unwrap();
        unowned.push(delivery.sequence);
    }
    let action = mez_agent::AgentAction {
        id: "later-owned".into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: format!("agent:{target}"),
            scope: Some("session".into()),
            content_type: "text/plain".into(),
            payload: "later current-task request".into(),
            correlation_id: None,
        },
    };
    assert_eq!(
        terminal_probe_send(&mut service, &turn, &action).status,
        mez_agent::ActionStatus::Succeeded
    );
    service
        .control
        .message_service_mut()
        .retire_agent_identity(&target);
    service
        .apply_peer_message_delivery_timer(current_unix_millis(), 1)
        .unwrap();
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let handoffs = context
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("mmp-delivery-outcome "))
        .collect::<Vec<_>>();
    assert_eq!(handoffs.len(), 2);
    assert!(
        handoffs
            .iter()
            .any(|block| block.content.contains("later-owned"))
    );
    assert!(
        handoffs
            .iter()
            .all(|block| !block.content.contains("unowned-earlier-"))
    );
    for sequence in unowned {
        assert_eq!(
            service
                .control
                .message_service()
                .delivery_outcomes_for_occurrence(&sender_id, sequence, 1)
                .unwrap()
                .len(),
            1
        );
    }
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(service.agent_scheduler().snapshot().active_capacity_used, 0);
}

/// One genuine canonical native receive interrupted before cursor advancement.
struct NativeReceiveFault {
    /// Runtime owns the bounded post-context receipt and exact lower offer.
    service: crate::runtime::RuntimeSessionService,
    /// Original task, never a replacement context or provider claim.
    turn: mez_agent::AgentTurnRecord,
    /// Exact registered sender entitled to terminal delivery facts.
    sender: AgentId,
    /// Native recipient registration before any lifecycle retirement.
    recipient: AgentId,
    /// Canonically committed occurrence distinct from an earlier uncommitted one.
    sequence: u64,
    /// Optional earlier offer that never reached a canonical context.
    earlier: Option<u64>,
    /// Acceptance clock shared by the deterministic TTL controls.
    now: u64,
}

/// Builds real user-turn receive admission. An optional older offered input
/// expires before this prompt; it must not acquire a fake cumulative ACK when
/// the later input commits. Fault injection follows real canonical storage.
fn native_receive_fault(prefix: bool, inject_fault: bool) -> NativeReceiveFault {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now = current_unix_seconds().saturating_mul(1000);
    let recipient = service
        .ensure_runtime_message_identity("agent-%1", PaneId::opaque("%1"), "agent", &[], now)
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-proof-sender", None, "agent", &[], now)
        .unwrap();
    let envelope = |id: &str, accepted: u64, ttl| Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: format!("runtime:{accepted}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient.agent_id.clone()),
        correlation_id: None,
        ttl_ms: ttl,
        content_type: "text/plain".into(),
        payload: id.into(),
        extension_fields: Vec::new(),
    };
    let earlier = if prefix {
        let accepted = now.saturating_sub(20);
        let row = service
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                envelope("native-proof-uncommitted", accepted, Some(1)),
                MessageScope::Session,
                accepted,
            )
            .unwrap();
        service
            .control
            .message_service()
            .receive_subscribed(&recipient.agent_id, accepted, 1)
            .unwrap();
        Some(row.sequence)
    } else {
        None
    };
    let row = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            envelope("native-proof-canonical", now, (!prefix).then_some(60_000)),
            MessageScope::Session,
            now,
        )
        .unwrap();
    if inject_fault {
        service.fail_next_peer_message_receive_after_context_storage_for_tests();
    }
    assert_eq!(
        service
            .start_agent_prompt_turn("%1", "receive original input")
            .is_err(),
        inject_fault
    );
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient.agent_id.as_str())
        .unwrap()
        .clone();
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(
                |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.label.contains("native-proof-canonical")
            )
    );
    NativeReceiveFault {
        service,
        turn,
        sender: sender.agent_id,
        recipient: recipient.agent_id,
        sequence: row.sequence,
        earlier,
        now,
    }
}

/// Stop may remove canonical context before the next delivery sweep. Its sealed
/// same-runtime receive receipt must still authorize only the original ACK,
/// before expiry can turn genuine committed input into a false failure. Stop
/// remains terminal and no replacement task/provider work is created.
#[test]
fn runtime_native_commit_survives_stop_before_cursor_reconciliation() {
    let NativeReceiveFault {
        mut service,
        turn,
        sender,
        recipient,
        sequence,
        now,
        ..
    } = native_receive_fault(false, true);
    service
        .finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)
        .unwrap();
    assert!(!service.agent_turn_contexts().contains_key(&turn.turn_id));
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    service
        .apply_peer_message_delivery_timer(now + 60_001, 1)
        .unwrap();
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        sequence
    );
    assert!(
        service
            .control
            .message_service()
            .delivery_outcomes_for(&sender, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
}

/// Pane cleanup discards display ownership and ends the native registration.
/// Canonical input must acquire an exact positive ACK atomically with that
/// retirement, even though its context/cursor fault preceded cleanup. Captured
/// lower state stays valid, and later TTL maintenance cannot report nondelivery.
#[test]
fn runtime_native_commit_survives_pane_retirement_before_timer() {
    let NativeReceiveFault {
        mut service,
        sender,
        recipient,
        sequence,
        now,
        ..
    } = native_receive_fault(false, true);
    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert!(
        service
            .control
            .message_service()
            .registered_identity(&recipient)
            .is_none()
    );
    let snapshot = service.control.message_service().snapshot_state();
    assert!(
        snapshot
            .retained_messages
            .iter()
            .find(|row| row.sequence == sequence)
            .unwrap()
            .acknowledged_recipients
            .as_ref()
            .unwrap()
            .contains(&recipient.to_string())
    );
    MessageService::from_snapshot_state(&snapshot).unwrap();
    service
        .apply_peer_message_delivery_timer(now + 60_001, 1)
        .unwrap();
    assert!(
        service
            .control
            .message_service()
            .delivery_outcomes_for(&sender, 100)
            .unwrap()
            .is_empty()
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// A later live input cannot cumulatively acknowledge an older expired offer
/// that never committed. The later canonical input stays protected while the
/// timer records the older Expired disposition, then its honest cursor ACK can
/// cross that terminal prefix without turning the earlier input into success.
#[test]
fn runtime_native_ack_does_not_claim_uncommitted_expired_offer_prefix() {
    let NativeReceiveFault {
        mut service,
        sender,
        recipient,
        sequence,
        earlier,
        now,
        ..
    } = native_receive_fault(true, false);
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    let queued = service.pending_agent_provider_tasks().len();
    service
        .apply_peer_message_delivery_timer(now + 1000, 1)
        .unwrap();
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        sequence
    );
    let outcomes = service
        .control
        .message_service()
        .delivery_outcomes_for(&sender, 100)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].sequence, earlier.unwrap());
    assert_eq!(
        outcomes[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::Expired
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), queued);
}

/// A restored presentation projection is not a native application commit. Even
/// a matching recipient/sequence/ID and claimed display payload cannot ACK an
/// offered input absent canonical context. Expiry must still produce its honest
/// terminal fact, without creating a peer event or trusting the restored text.
#[test]
fn runtime_restored_projection_cannot_forge_native_commit_proof() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "active native task")
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    let sender = service
        .ensure_runtime_message_identity(
            "agent-projection-sender",
            None,
            "agent",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    let now = current_unix_millis();
    let message_id = "uncommitted-restored-claim";
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: message_id.into(),
                message_type: "send".into(),
                time: format!("runtime:{now}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
                correlation_id: None,
                ttl_ms: Some(10),
                content_type: "text/plain".into(),
                payload: "never committed".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    service
        .control
        .message_service()
        .receive_subscribed(&recipient, now, 100)
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .restore_unsettled_received_peer_message_presentations(&[
            crate::storage::snapshot::SnapshotUnsettledPeerPresentation {
                identity: format!(
                    "peer-message recipient={recipient} sequence={} id={message_id}",
                    delivery.sequence
                ),
                occurrence: Default::default(),
                recipient_agent_id: recipient.to_string(),
                pane_id: "%1".into(),
                conversation_id: conversation,
                turn_id: started.turn_id.clone(),
                sequence: delivery.sequence,
                peer_label: "projection-only".into(),
                direct_parent: false,
                content_type: "text/plain".into(),
                payload: "claim of committed input is not authority".into(),
                presentation_eligible: true,
                live_rendered: false,
            },
        ])
        .unwrap();
    service
        .apply_peer_message_delivery_timer(now + 11, 1)
        .unwrap();
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    let outcomes = service
        .control
        .message_service()
        .delivery_outcomes_for(&sender.agent_id, 100)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::Expired
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&started.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .all(|block| !block.label.contains(message_id))
    );
}

/// A trusted conversation/project rebind can leave an old offered obligation
/// outside the new audience while a later session-wide input canonically
/// commits. Stop must retain that proof without ACKing the old input, replaying
/// the committed input as a new task, or keeping a permanent expiry timer.
/// Fresh mail still reaches the native actor; eventual exact retirement ACKs
/// only the two real commits and reports the old unsatisfied obligation.
#[test]
fn runtime_native_blocked_commit_is_quiet_not_replayed_and_does_not_hide_fresh_mail() {
    use mez_agent::messaging::ProjectMembership;
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let project_a = temp_root("native-proof-project-a");
    let project_b = temp_root("native-proof-project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let membership_a =
        ProjectMembership::from_canonical_root(std::fs::canonicalize(&project_a).unwrap());
    let membership_b =
        ProjectMembership::from_canonical_root(std::fs::canonicalize(&project_b).unwrap());
    service.set_pane_current_working_directory("%1", project_a);
    service
        .agent_shell_store_mut()
        .install_project_scope("%1", membership_a.clone())
        .unwrap();
    let now = current_unix_seconds().saturating_mul(1000);
    let native = service
        .ensure_runtime_message_identity("agent-%1", PaneId::opaque("%1"), "agent", &[], now)
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&native.agent_id)
        .unwrap();
    let sender = SenderIdentity {
        agent_id: AgentId::opaque("external-prefix-source").unwrap(),
        project_scope: Some(membership_a.scope_id()),
        pane_id: None,
        window_id: None,
        role: Some("peer".into()),
        capabilities: Vec::new(),
        objective: None,
    };
    service
        .control
        .message_service_mut()
        .ensure_agent_identity(sender.clone(), now)
        .unwrap();
    let envelope = |id: &str, ttl| Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: format!("runtime:{now}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(native.agent_id.clone()),
        correlation_id: None,
        ttl_ms: ttl,
        content_type: "text/plain".into(),
        payload: id.into(),
        extension_fields: Vec::new(),
    };
    let older = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            envelope("old-project-uncommitted", None),
            MessageScope::Project,
            now,
        )
        .unwrap();
    service
        .control
        .message_service()
        .receive_subscribed(&native.agent_id, now, 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "11111111-1111-4111-8111-111111111111", 0)
        .unwrap();
    service
        .agent_shell_store_mut()
        .install_project_scope("%1", membership_b.clone())
        .unwrap();
    service.set_pane_current_working_directory("%1", project_b);
    service
        .rebind_runtime_message_project_scope("%1", Some(membership_b.scope_id()))
        .unwrap();
    let committed = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            envelope("new-project-canonical", Some(60_000)),
            MessageScope::Session,
            now,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "receive the session-wide input")
        .unwrap();
    let turn = super::fixtures::messaging_test_turn(&service, &started.turn_id);
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    service
        .finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)
        .unwrap();
    for observed in [now + 1, now + 60_001] {
        assert!(!service.has_pending_peer_messages(observed));
        let result = service
            .apply_peer_message_delivery_timer(observed, 1)
            .unwrap();
        assert!(result.side_effects.is_empty());
        assert_eq!(service.agent_turn_ledger().turns().len(), 1);
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(
            service
                .control
                .message_service()
                .delivery_outcomes_for(&sender.agent_id, 100)
                .unwrap()
                .is_empty()
        );
    }
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            envelope("genuinely-fresh-input", None),
            MessageScope::Session,
            now + 60_002,
        )
        .unwrap();
    assert!(service.has_pending_peer_messages(now + 60_002));
    service
        .apply_peer_message_delivery_timer(now + 60_002, 1)
        .unwrap();
    assert_eq!(service.agent_turn_ledger().turns().len(), 2);
    let current = service.agent_turn_ledger().turns().last().unwrap();
    let context = service.agent_turn_contexts().get(&current.turn_id).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(
                |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.label.contains("genuinely-fresh-input")
            )
            .count(),
        1
    );
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    service
        .apply_peer_message_delivery_timer(now + 60_003, 1)
        .unwrap();
    let outcomes = service
        .control
        .message_service()
        .delivery_outcomes_for(&sender.agent_id, 100)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].sequence, older.sequence);
    assert_eq!(
        outcomes[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::RecipientRetired
    );
    assert!(
        service
            .control
            .message_service()
            .delivery_outcomes_for_occurrence(&sender.agent_id, committed.sequence, 100)
            .unwrap()
            .is_empty()
    );
}

/// Replacing message state ends the previous private commit namespace. A
/// captured pending occurrence may reuse public IDs while old historical
/// context is still present during staging; neither that context nor a prior
/// sealed receipt can ACK it in the restored service generation.
#[test]
fn runtime_message_restore_cannot_reuse_old_native_commit_authority() {
    let NativeReceiveFault {
        mut service,
        turn,
        sender,
        recipient,
        now,
        ..
    } = native_receive_fault(false, true);
    let mut payload =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    payload.message_state = Some(service.control.message_service().snapshot_state());
    service
        .restore_message_state_for_restored_snapshot(&payload)
        .unwrap();
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(
                |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.label.contains("native-proof-canonical")
            )
    );
    service
        .apply_peer_message_delivery_timer(now + 60_001, 1)
        .unwrap();
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    let outcomes = service
        .control
        .message_service()
        .delivery_outcomes_for(&sender, 100)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::Expired
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Outbound action ownership is same-generation private state as well. A
/// restored pending envelope with identical public sequence/task/action fields
/// cannot inherit an old action owner's handoff capability from live memory.
/// Its terminal fact remains bounded lower custody without waking the old wait.
#[test]
fn runtime_message_restore_cannot_reuse_old_outbound_action_owner() {
    let (mut service, turn, target) = parked_terminal_sender();
    let mut payload =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    payload.message_state = Some(service.control.message_service().snapshot_state());
    service
        .restore_message_state_for_restored_snapshot(&payload)
        .unwrap();
    service
        .control
        .message_service_mut()
        .retire_agent_identity(&target);
    service
        .apply_peer_message_delivery_timer(current_unix_millis(), 1)
        .unwrap();
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .all(|block| !block.label.starts_with("mmp-delivery-outcome "))
    );
    let sender = AgentId::opaque(turn.agent_id).unwrap();
    assert_eq!(
        service
            .control
            .message_service()
            .delivery_outcomes_for(&sender, 100)
            .unwrap()
            .len(),
        1
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
}
