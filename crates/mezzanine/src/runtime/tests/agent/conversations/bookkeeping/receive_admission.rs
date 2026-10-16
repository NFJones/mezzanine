//! Receive admission waits for the exact bound conversation's checked history.
//!
//! These local fixtures retain real terminal candidates and original accepted
//! envelopes. Deferral must not manufacture a turn, acknowledge mail, spend loop
//! budget, or weaken the history fence; settlement admits the same mail once.

use super::*;
use mez_agent::messaging::{Envelope, MessageScope, Recipient};

/// A stopped turn, ordinary terminal failure, and terminal failure after real
/// MAAP repair each leave a genuine candidate. Queued and worker-claimed forms
/// must quietly defer before receive context construction, then commit exactly
/// the originally accepted envelope once after checked acceptance, without any
/// provider call or repetition of the previous execution.
#[test]
fn runtime_peer_receive_defers_pending_bookkeeping_until_checked_acceptance() {
    for terminal in ["stopped", "failed", "repair-failed"] {
        for claimed in [false, true] {
            let (mut service, _, conversation) = terminal_candidate_fixture(terminal, claimed);
            let mut work = claimed.then(|| service.claim_bookkeeping_candidates().pop().unwrap());
            let now = crate::runtime::current_unix_millis();
            let (recipient, sequence) = accept_peer(&mut service, "%1", now, None);
            let turns = service.agent_turn_ledger().turns().len();
            let budget = service.agent_peer_message_turn_count("agent-%1");
            for generation in 1..=3 {
                let deferred = service
                    .apply_peer_message_delivery_timer(now, generation)
                    .unwrap();
                assert!(!deferred.applied);
                assert!(
                    matches!(
                        deferred.side_effects.as_slice(),
                        [RuntimeSideEffect::ScheduleTimer {
                            delay_ms: 1_000,
                            ..
                        }]
                    ),
                    "retained mail keeps the existing bounded maintenance wake"
                );
                assert_eq!(service.agent_turn_ledger().turns().len(), turns);
                assert_eq!(service.agent_peer_message_turn_count("agent-%1"), budget);
                assert_eq!(cursor(&service, &recipient), 0);
                assert!(service.persistence.bookkeeping_pending(&conversation));
            }
            let work = work
                .take()
                .unwrap_or_else(|| service.claim_bookkeeping_candidates().pop().unwrap());
            let duplicate = work.clone();
            let history = work.execute();
            assert!(
                service
                    .complete_bookkeeping_candidate(work, history)
                    .unwrap()
            );
            assert!(
                !service
                    .complete_bookkeeping_candidate(duplicate, Ok((Vec::new(), 1)))
                    .unwrap()
            );
            assert!(!service.persistence.bookkeeping_pending(&conversation));
            assert_eq!(
                service.deliver_pending_runtime_agent_messages(now).unwrap(),
                1
            );
            assert_eq!(
                service.deliver_pending_runtime_agent_messages(now).unwrap(),
                0
            );
            assert_eq!(cursor(&service, &recipient), sequence);
            assert_eq!(service.agent_turn_ledger().turns().len(), turns + 1);
            assert_eq!(
                service.agent_peer_message_turn_count("agent-%1"),
                budget + 1
            );
            service.terminate_all_pane_processes().unwrap();
        }
    }
}

/// A real failed archive check remains observable and fail-closed, rather than
/// being swallowed as successful chronology. Repeated global sweeps defer its
/// recipient without another archive attempt. The existing one-second delivery
/// timer is maintenance, not permission to retry the blocked chronology read.
/// A duplicate stale success cannot remove the fence or advance the inbox.
#[test]
fn runtime_peer_receive_blocked_bookkeeping_remains_fenced_without_archive_retry() {
    let (mut service, store, conversation) = stopped_candidate_fixture("receive-blocked");
    seed_compaction_prefix(&mut service, &store, &conversation);
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let duplicate = work.clone();
    std::fs::remove_file(store.transcript_path(&conversation).unwrap()).unwrap();
    let history = work.execute();
    assert!(
        history.is_err(),
        "required committed history must not become empty history"
    );
    assert!(
        service
            .complete_bookkeeping_candidate(work, history)
            .is_err()
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("bookkeeping blocked")
    );
    let now = crate::runtime::current_unix_millis();
    let (recipient, _) = accept_peer(&mut service, "%1", now, None);
    for generation in 1..=3 {
        let transition = service
            .apply_peer_message_delivery_timer(now, generation)
            .unwrap();
        assert!(!transition.applied);
        assert!(matches!(
            transition.side_effects.as_slice(),
            [RuntimeSideEffect::ScheduleTimer {
                delay_ms: 1_000,
                ..
            }]
        ));
        assert_eq!(cursor(&service, &recipient), 0);
        assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    }
    assert!(service.persistence.bookkeeping_pending(&conversation));
    assert!(service.claim_bookkeeping_candidates().is_empty());
    assert!(
        !service
            .complete_bookkeeping_candidate(duplicate, Ok((Vec::new(), 1)))
            .unwrap()
    );
    assert!(
        service
            .start_agent_prompt_turn("%1", "must remain fenced")
            .is_err()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A pending recipient must not hide another ready recipient in delivery timer
/// eligibility. Finite-TTL mail still keeps bounded expiry service while history
/// is fenced, and expires without receive context or cursor acknowledgement.
#[test]
fn runtime_peer_receive_pending_history_preserves_expiry_and_other_recipients() {
    let (mut service, _, _) = stopped_candidate_fixture("receive-expiry");
    let now = crate::runtime::current_unix_millis();
    let (recipient, _) = accept_peer(&mut service, "%1", now, None);
    accept_peer(&mut service, "%1", now + 1, Some(10));
    assert!(service.has_pending_peer_messages(now));
    let transition = service.apply_peer_message_delivery_timer(now, 1).unwrap();
    assert!(!transition.applied);
    assert_eq!(transition.side_effects.len(), 1);
    assert!(
        !service
            .apply_peer_message_delivery_timer(now + 12, 1)
            .unwrap()
            .applied
    );
    assert_eq!(cursor(&service, &recipient), 0);
    let batch = service
        .message_service_mut()
        .receive_subscribed(&recipient, now + 12, 100)
        .unwrap();
    assert_eq!(
        batch.messages.len(),
        1,
        "only the original non-TTL envelope remains"
    );
    assert!(batch.messages[0].envelope.ttl_ms.is_none());
    // A pane-less recipient has independent transport delivery and must still
    // make the shared timer actionable after the first fenced model recipient.
    accept_peer(&mut service, "independent", now + 13, None);
    for _ in 0..3 {
        assert!(
            service.has_pending_peer_messages(now + 13),
            "fenced mail does not disable ordinary delivery maintenance"
        );
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Creates genuine interruption or failed-provider chronology. Repair scheduling
/// modifies the same real turn before failure; no network provider is invoked.
fn terminal_candidate_fixture(
    terminal: &str,
    claimed: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    if terminal == "stopped" {
        return stopped_candidate_fixture(&format!("receive-{terminal}-{claimed}"));
    }
    let store = AgentTranscriptStore::new(temp_root(&format!("receive-{terminal}-{claimed}")));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.persistence.enable_transcript_adapter();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "retained provider prompt")
        .unwrap();
    if terminal == "repair-failed" {
        assert!(
            service
                .schedule_agent_provider_repair_transition(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &started.turn_id,
                    &MezError::invalid_state("provider MAAP output is malformed: invalid JSON")
                )
                .unwrap()
                .unwrap()
                .applied
        );
    }
    service
        .fail_configured_agent_provider_task(
            &started.turn_id,
            &MezError::invalid_state("terminal provider failure"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    (service, store, conversation)
}

/// Accepts one original model envelope through the real message service and
/// returns its exact recipient and transport sequence for cursor assertions.
fn accept_peer(
    service: &mut RuntimeSessionService,
    pane: &str,
    now: u64,
    ttl_ms: Option<u64>,
) -> (AgentId, u64) {
    let id = if pane.starts_with('%') {
        format!("agent-{pane}")
    } else {
        pane.to_string()
    };
    let recipient = service
        .ensure_runtime_message_identity(
            &id,
            pane.starts_with('%')
                .then(|| mez_core::ids::PaneId::opaque(pane.to_string()).unwrap()),
            "agent",
            &[],
            now,
        )
        .unwrap();
    if service
        .control
        .message_service()
        .subscription(&recipient.agent_id)
        .is_none()
    {
        service
            .control
            .message_service_mut()
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
    }
    let sender = service
        .ensure_runtime_message_identity("agent-receive-sender", None, "agent", &[], now)
        .unwrap();
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: format!("receive-{pane}-{now}"),
                message_type: "send".into(),
                time: format!("runtime:{now}"),
                sender: sender.clone(),
                recipient: Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "original accepted peer evidence".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    (recipient.agent_id, delivery.sequence)
}

/// Reads the durable receive cursor without mutating delivery or admission.
fn cursor(service: &RuntimeSessionService, recipient: &AgentId) -> u64 {
    service
        .control
        .message_service()
        .subscription(recipient)
        .unwrap()
        .last_sequence
}
