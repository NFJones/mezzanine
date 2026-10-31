//! Bounded request admission and exact retained-source recovery under faults.
//! Queue pressure, restore and recipient pivots cannot discard or alias native
//! source custody; unchanged canonical occurrence identity remains mandatory.

use super::*;

/// The actual native send path rejects capped tracking before lower acceptance.
/// It gives bounded same-turn feedback and preserves the first accepted request,
/// proving the cap is not a post-send best-effort bookkeeping failure.
#[test]
fn request_bound_native_capacity_rejection_is_preaccept_and_correctable() {
    let (mut service, parent, peer) = fixture();
    service
        .set_tracked_message_request_limits_for_test(mez_agent::messaging::MessageRequestLimits {
            per_conversation: 1,
            ..Default::default()
        })
        .unwrap();
    send(&mut service, &parent, peer.agent_id.as_str(), true);
    let sequence = service.message_service().next_message_sequence();
    let usage = service.tracked_message_request_usage();
    let execution = execute(
        &mut service,
        &parent,
        AgentAction {
            id: "over-cap".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: true,
                recipient: format!("agent:{}", peer.agent_id),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "this must not be sent".into(),
                correlation_id: None,
            },
        },
    );
    assert_eq!(execution.action_results[0].status, ActionStatus::Failed);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains("request_tracking_capacity")
    );
    assert_eq!(service.message_service().next_message_sequence(), sequence);
    assert_eq!(service.tracked_message_request_usage(), usage);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
    finish(&mut service, &parent);
}

/// A full scheduler queue retains both the original matching response and the
/// typed handoff event as exact canonical sources after transport ACK. Compaction
/// cannot remove them. Once fair admission commits, all pins and request charges
/// release and the same parent continues exactly once.
#[test]
fn request_bound_queue_full_retains_exact_context_sources_until_handoff() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
    wait(&mut service, &parent, &message, None);
    service
        .configure_agent_scheduler_queue_limits(1, usize::MAX)
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "request-queue-holder".into(),
            conversation_id: "request-queue-holder".into(),
            agent_id: "agent-request-queue-holder".into(),
            pane_id: None,
            kind: mez_agent::ScheduledWorkKind::BackgroundTask,
        })
        .unwrap();
    reply(
        &mut service,
        &parent,
        &peer,
        "queue-full-reply",
        Some(&message),
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.tracked_message_request_usage().1, 1);
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    let protected = context
        .chronology()
        .iter()
        .enumerate()
        .filter(|(_, event)| context.peer_message_is_pending(event.sequence()))
        .map(|(index, event)| (index, event.sequence()))
        .collect::<Vec<_>>();
    assert_eq!(protected.len(), 2);
    for (index, _) in &protected {
        let summary = mez_agent::ContextBlock::reference_event(
            mez_agent::ContextSourceKind::CompactionSelective,
            "summary",
            "discarded",
        );
        assert!(
            service
                .agent_turn_contexts_mut()
                .get_mut(&parent.turn_id)
                .unwrap()
                .compact_execution_ranges(vec![(*index..*index + 1, summary)])
                .is_err()
        );
    }
    service
        .agent_scheduler_mut()
        .cancel("request-queue-holder")
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    assert!(protected.iter().all(|(_, sequence)| {
        !service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(*sequence)
    }));
    finish(&mut service, &parent);
}

/// Actual transport snapshot restoration resets local dependency generations,
/// while this conversation retains its earlier canonical handoff history. A new
/// valid request must commit its own retirement evidence rather than reusing the
/// old generation-one reply. No transport acceptance or actor lease is forged.
#[test]
fn request_bound_restored_handoff_history_does_not_alias_new_outcome() {
    let (mut service, parent, peer) = fixture();
    let first = send(&mut service, &parent, peer.agent_id.as_str(), true);
    wait(&mut service, &parent, &first, None);
    reply(&mut service, &parent, &peer, "before-restore", Some(&first));
    let old = service.agent_turn_contexts()[&parent.turn_id]
        .blocks()
        .iter()
        .find(|block| {
            block.label.starts_with("request-dependency:")
                || block.label.starts_with("request-handoff:")
        })
        .unwrap()
        .clone();
    let snapshot = service.message_service().snapshot_state();
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
    *service.message_service_mut() = MessageService::from_snapshot_state(&snapshot).unwrap();
    let second = send(&mut service, &parent, peer.agent_id.as_str(), true);
    assert_ne!(first, second);
    wait(&mut service, &parent, &second, None);
    service
        .message_service_mut()
        .retire_observational_identity(&peer.agent_id);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let blocks = service.agent_turn_contexts()[&parent.turn_id].blocks();
    assert!(
        blocks
            .iter()
            .any(|block| block.label == old.label && block.content == old.content)
    );
    assert!(
        blocks.iter().any(|block| block.label != old.label
            && block
                .content
                .contains("The selected original dependency is unavailable")),
        "new native retirement was consumed without its own canonical reference"
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// QueueFull retains A's committed handoff. Ordinary mail during a real human
/// pause permits the normal explicit Resume path without consuming the tracked
/// request. A later explicit B selection must never reuse A's canonical body.
/// This uses real lifecycle commands and scheduler fault pressure, not claims.
#[test]
fn request_bound_pivot_after_retained_handoff_uses_selected_occurrence() {
    let (mut service, parent, first_peer, primary) = fixture_with_primary();
    let second_peer = service
        .ensure_runtime_message_identity(
            "agent-pivot-second",
            None,
            "worker",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    let execution = execute(
        &mut service,
        &parent,
        AgentAction {
            id: "fanout".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: true,
                recipient: "role:worker".into(),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "select one review".into(),
                correlation_id: None,
            },
        },
    );
    let accepted: serde_json::Value = serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let message = accepted["message_id"].as_str().unwrap();
    wait(
        &mut service,
        &parent,
        message,
        Some(first_peer.agent_id.as_str()),
    );
    service
        .configure_agent_scheduler_queue_limits(1, usize::MAX)
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "pivot-queue-holder".into(),
            conversation_id: "pivot-queue-holder".into(),
            agent_id: "agent-pivot-queue-holder".into(),
            pane_id: None,
            kind: mez_agent::ScheduledWorkKind::BackgroundTask,
        })
        .unwrap();
    let first_sequence = reply(
        &mut service,
        &parent,
        &first_peer,
        "pivot-first",
        Some(message),
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let pause = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    service
        .agent_scheduler_mut()
        .cancel("pivot-queue-holder")
        .unwrap();
    reply(&mut service, &parent, &second_peer, "pivot-unrelated", None);
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    assert!(
        service
            .resume_agent_lifecycle_target(&primary, &target, pause)
            .unwrap()
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(service.tracked_message_request_usage().0.0, 1);
    let second_sequence = reply(
        &mut service,
        &parent,
        &second_peer,
        "pivot-second",
        Some(message),
    );
    assert_ne!(first_sequence, second_sequence);
    wait(
        &mut service,
        &parent,
        message,
        Some(second_peer.agent_id.as_str()),
    );
    let expected = format!("message sequence {second_sequence}.");
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block
                .content
                .contains("Authenticated matching response committed")
                && block.content.contains(&expected)),
        "B consumed the request using A's earlier handoff body"
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// A stale local event pointer cannot be repaired by finding a matching-looking
/// label elsewhere in history. The runtime rejects without wake/consumption,
/// retains the source, and succeeds once its original exact reference is restored.
/// Acceptance, context commits, pressure and recovery all use their real owners.
#[test]
fn request_bound_mismatched_retained_reference_fails_closed_without_consumption() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
    wait(&mut service, &parent, &message, None);
    service
        .configure_agent_scheduler_queue_limits(1, usize::MAX)
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "identity-queue-holder".into(),
            conversation_id: "identity-queue-holder".into(),
            agent_id: "agent-identity-queue-holder".into(),
            pane_id: None,
            kind: mez_agent::ScheduledWorkKind::BackgroundTask,
        })
        .unwrap();
    reply(
        &mut service,
        &parent,
        &peer,
        "identity-reply",
        Some(&message),
    );
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    let wrong = context
        .chronology()
        .iter()
        .find(|event| {
            event
                .block()
                .content
                .starts_with("peer message: untrusted data")
        })
        .unwrap()
        .sequence();
    let correct = context
        .chronology()
        .iter()
        .find(|event| {
            event
                .block()
                .content
                .contains("Authenticated matching response committed")
        })
        .unwrap()
        .sequence();
    assert_ne!(wrong, correct);
    let usage = service.tracked_message_request_usage();
    service
        .replace_request_handoff_reference_for_test(&parent.turn_id, wrong)
        .unwrap();
    service
        .agent_scheduler_mut()
        .cancel("identity-queue-holder")
        .unwrap();
    let error = service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("request handoff reference identity changed"),
        "{error}"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    service
        .replace_request_handoff_reference_for_test(&parent.turn_id, correct)
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}
