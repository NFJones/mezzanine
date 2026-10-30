//! Production native send/wait integration over real local acceptance custody.
//!
//! Offline providers exercise normal execution, conversation, context, scheduler
//! and terminal result owners. No lifecycle envelope or acceptance flag is forged.
//! Actor-owned provider claims and the larger fault matrix are separate gates.

use super::*;
use mez_agent::{AgentAction, AgentActionPayload, SayStatus};

/// Creates a genuine native parent conversation and a registered nonowned peer.
/// A non-live parent has explicitly process-free ownership, rather than an
/// unavailable OS observation being mistaken for an absent process.
fn fixture() -> (RuntimeSessionService, AgentTurnRecord, SenderIdentity) {
    let (service, parent, peer, _) = fixture_with_primary();
    (service, parent, peer)
}

/// Retains the actual writable primary for lifecycle commands in fault tests.
fn fixture_with_primary() -> (
    RuntimeSessionService,
    AgentTurnRecord,
    SenderIdentity,
    mez_core::ids::ClientId,
) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.configure_agent_scheduler_limit(1).unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-request-bound",
    )));
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.session.set_pane_live_state("%1", false).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let started = service
        .start_agent_prompt_turn("%1", "coordinate one exact request")
        .unwrap();
    let parent = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let peer = service
        .ensure_runtime_message_identity(
            "agent-request-peer",
            None,
            "worker",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    (service, parent, peer, primary)
}

/// Uses the same provider application and action planning boundary as native
/// turns, with a deterministic offline action response and no transport replay.
fn execute(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    mut action: AgentAction,
) -> mez_agent::AgentTurnExecution {
    // The MAAP parser assigns response-local action ordinals. Use that real
    // shape so the runtime scopes each new provider execution independently.
    action.id = "action-1".into();
    let raw_text = format!(
        "request integration {:?} context={}",
        action,
        service.agent_turn_contexts()[&turn.turn_id].event_sequence_high_water_mark()
    );
    service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &RuntimeBatchProvider {
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".into(),
                    model: "test".into(),
                    raw_text,
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(mez_agent::MaapBatch {
                        rationale: "perform exact request coordination".into(),
                        actions: vec![action],
                    }),
                    provider_transcript_events: vec![],
                },
            },
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap()
}

/// Sends once through the real native action boundary and returns the actual
/// accepted message ID; tests never guess scoped action IDs or request IDs.
fn send(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    peer: &str,
    tracked: bool,
) -> String {
    let execution = execute(
        service,
        parent,
        AgentAction {
            id: "question".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: tracked,
                recipient: format!("agent:{peer}"),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "please answer this request".into(),
                correlation_id: None,
            },
        },
    );
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Succeeded, "{result:?}");
    let structured: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    structured["message_id"].as_str().unwrap().to_string()
}

/// Parks only on the real accepted ID, optionally selecting one frozen target.
fn wait(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    message: &str,
    from: Option<&str>,
) {
    execute(
        service,
        parent,
        AgentAction {
            id: "targeted-wait".into(),
            payload: AgentActionPayload::Wait {
                for_message: Some(message.into()),
                from_agent: from.map(str::to_owned),
            },
        },
    );
}

/// Accepts actual authenticated peer mail; supplied correlation is only content
/// until the runtime proves its original accepted sender and reverse audience.
fn reply(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    sender: &SenderIdentity,
    id: &str,
    correlation: Option<&str>,
) -> u64 {
    let sequence = accept_reply(service, parent, sender, id, correlation);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    sequence
}

/// Accepts authenticated mail without offering, committing or acknowledging it.
/// This preserves the real acceptance/retirement race for receiver tests.
fn accept_reply(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    sender: &SenderIdentity,
    id: &str,
    correlation: Option<&str>,
) -> u64 {
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: id.into(),
                message_type: "send".into(),
                time: "fixture:reply".into(),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    AgentId::opaque(parent.agent_id.clone()).unwrap(),
                ),
                correlation_id: correlation.map(str::to_owned),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "peer answer or unrelated coordination".into(),
                extension_fields: vec![],
            },
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap()
        .sequence
}

/// Completes the original task after it receives evidence, proving recovery did
/// not replace the task or require automatic resending of the request.
fn finish(service: &mut RuntimeSessionService, parent: &AgentTurnRecord) {
    execute(
        service,
        parent,
        AgentAction {
            id: "finish".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "completed authorized independent work".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
}

/// A tracked request wakes on a genuinely correlated response both before and
/// after park. The same original parent consumes one outcome, releases every
/// reservation, and completes without resending or creating another parent task.
#[test]
fn request_bound_matching_reply_before_and_after_park_completes_same_turn() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if before_park {
            reply(&mut service, &parent, &peer, "early-reply", Some(&message));
        }
        wait(&mut service, &parent, &message, None);
        if !before_park {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            reply(&mut service, &parent, &peer, "late-reply", Some(&message));
        }
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        let wait = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        assert!(
            wait.structured_content_json
                .as_ref()
                .unwrap()
                .contains("\"state\":\"reply\"")
        );
        finish(&mut service, &parent);
    }
}

/// Unrelated mail and a foreign sender's exact correlation may resume inference
/// but cannot consume the selected request. Reparking reuses the same generation
/// and resource charge until the original authenticated recipient replies.
#[test]
fn request_bound_irrelevant_mail_reparks_same_edge_without_progress() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
    wait(&mut service, &parent, &message, None);
    let usage = service.tracked_message_request_usage();
    let stranger = service
        .ensure_runtime_message_identity(
            "agent-request-stranger",
            None,
            "peer",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    reply(
        &mut service,
        &parent,
        &stranger,
        "foreign-correlation",
        Some(&message),
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    wait(&mut service, &parent, &message, None);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    reply(
        &mut service,
        &parent,
        &peer,
        "right-correlation",
        Some(&message),
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// Actual registration retirement, unlike mere offline presence, supplies only
/// opaque unavailability and resumes the same task to choose an authorized
/// alternative. Both pre-park and post-park retirement retain the outcome.
#[test]
fn request_bound_nonowned_retirement_before_and_after_park_allows_alternative() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if before_park {
            service
                .control
                .message_service_mut()
                .retire_observational_identity(&peer.agent_id);
        }
        wait(&mut service, &parent, &message, None);
        if !before_park {
            service
                .control
                .message_service_mut()
                .update_presence(
                    &peer.agent_id,
                    mez_agent::messaging::AgentPresenceStatus::Offline,
                    current_unix_millis(),
                )
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
                AgentTurnState::Blocked
            );
            service
                .control
                .message_service_mut()
                .retire_observational_identity(&peer.agent_id);
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
        }
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        let result: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(result["state"], "peer_unavailable");
        assert_eq!(result["progress_witness"], false);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// Untracked and nonexistent IDs remain bounded same-turn correction rather
/// than creating lifecycle authority, parking forever, or terminating the task.
#[test]
fn request_bound_invalid_or_untracked_selection_is_correctable_without_edge() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), false);
    for selected in [message.as_str(), "invented-message-id"] {
        wait(&mut service, &parent, selected, None);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        assert!(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .content_text()
                .contains("request wait unavailable")
        );
    }
    finish(&mut service, &parent);
}

/// Creates a genuine persistent task through the normal native spawn owner.
fn spawn_child(service: &mut RuntimeSessionService, parent: &AgentTurnRecord) -> AgentTurnRecord {
    let mut spawn = runtime_spawn_agent_action(
        "tracked-child",
        "produce a result without sending a message",
    );
    if let AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut spawn.payload
    {
        *lifetime = mez_agent::SubagentLifetime::Persistent;
        *objective = Some("answer assigned work".into());
    }
    let spawned = service
        .execute_spawn_action_for_turn(parent, &spawn)
        .unwrap();
    let structured: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child_id = structured["spawn"]["agent"]["id"].as_str().unwrap();
    service
        .agent_turn_ledger()
        .latest_turn_for_pane(child_id.strip_prefix("agent-").unwrap())
        .unwrap()
        .clone()
}

/// A genuinely spawned persistent child's exact live task is frozen at send.
/// Its normal terminal execution, without any model-authored reply, supplies an
/// untrusted result reference and lets the same original parent complete.
#[test]
fn request_bound_owned_child_success_without_model_reply_completes_parent() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    execute(
        &mut service,
        &child,
        AgentAction {
            id: "child-result".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "verified child output".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    );
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
    let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
    assert!(
        result
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains("peer_terminal")
    );
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.content.contains("verified child output")
                && block.content.contains(&child.turn_id))
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "retire-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A genuine native provider-failure settlement reuses its original failure
/// diagnostic once and adds only an exact request handoff reference. The selected
/// task wakes its parent without a fabricated peer reply or duplicate output.
#[test]
fn request_bound_owned_failure_reuses_native_source_without_duplicate_diagnostic() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    service
        .fail_agent_turn_for_provider_error(
            &child,
            "runtime-batch",
            &runtime_model_profile("runtime-batch", "test"),
            &MezError::invalid_state("offline tracked child failure"),
        )
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
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.label.starts_with("owned-child-failure:"))
            .count(),
        1
    );
    let handoff = context
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("request-handoff:"))
        .collect::<Vec<_>>();
    assert_eq!(handoff.len(), 1);
    assert!(
        handoff[0]
            .content
            .contains("existing native failure receipt")
    );
    assert!(!handoff[0].content.contains("offline tracked child failure"));
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-failed-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A fanout requires one original non-self recipient before allocating an edge.
/// Correcting the selector resumes the same conversation and consumes only the
/// authenticated selected response; an unselected peer cannot stand in for it.
#[test]
fn request_bound_fanout_requires_exact_selection_and_self_is_rejected() {
    let (mut service, parent, peer) = fixture();
    service
        .ensure_runtime_message_identity(
            "agent-second-worker",
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
                payload: "one selected review".into(),
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
    assert_eq!(accepted["queued_recipients"], 2);
    wait(&mut service, &parent, message, None);
    assert_eq!(service.tracked_message_request_usage().1, 0);
    wait(&mut service, &parent, message, Some(peer.agent_id.as_str()));
    assert_eq!(service.tracked_message_request_usage().1, 1);
    reply(&mut service, &parent, &peer, "fanout-reply", Some(message));
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    let own_message = send(&mut service, &parent, &parent.agent_id, true);
    wait(&mut service, &parent, &own_message, None);
    assert_eq!(service.tracked_message_request_usage().1, 0);
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
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

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

/// A real state-only lifecycle completion has no provider execution to bridge,
/// but it is still successful terminal work of the exact task captured at send.
/// The same parent must receive terminal occurrence and progress, rather than
/// intentional-retirement evidence, and duplicate observations must stay inert.
#[test]
fn request_bound_terminal_state_only_success_is_not_intentional_retirement() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    assert!(!service.agent_turn_executions().contains_key(&child.turn_id));
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "state-only terminal regression",
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    let wait = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
    let outcome: serde_json::Value =
        serde_json::from_str(wait.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        outcome["state"], "peer_terminal",
        "state-only success was mislabeled as intentional retirement"
    );
    assert_eq!(outcome["progress_witness"], true);
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|b| b.label.starts_with("request-handoff:")
                && b.content.contains("completed without provider output"))
    );
    let sources = service.agent_turn_contexts()[&parent.turn_id]
        .blocks()
        .iter()
        .filter(|b| b.label.starts_with("request-handoff:"))
        .count();
    service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Completed)
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .filter(|b| b.label.starts_with("request-handoff:"))
            .count(),
        sources
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-state-only-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A real goal-loop iteration can complete while its logical controller retains
/// the result and creates later work. Only the accepted exact iteration may
/// satisfy the tracked request; its result cannot be lost at the controller's
/// early return or replaced by the new iteration. Plain waits remain parked.
#[test]
fn request_bound_terminal_loop_continuation_preserves_exact_iteration_result() {
    for tracked in [false, true] {
        let (mut service, parent, _) = fixture();
        service.configure_agent_scheduler_limit(2).unwrap();
        let initial = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&initial.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        service
            .complete_running_agent_turn_and_start_ready(
                &initial,
                AgentTurnState::Completed,
                "prepare persistent loop task",
            )
            .unwrap();
        service.execute_agent_shell_loop_command(&initial.pane_id,
            "/loop --limit 3 --goal 'complete the later iteration' produce the first iteration result").unwrap();
        let child = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        assert_ne!(child.turn_id, initial.turn_id);
        let message = send(&mut service, &parent, &child.agent_id, tracked);
        if tracked {
            wait(&mut service, &parent, &message, None);
        } else {
            execute(
                &mut service,
                &parent,
                AgentAction {
                    id: "plain-loop-wait".into(),
                    payload: AgentActionPayload::Wait {
                        for_message: None,
                        from_agent: None,
                    },
                },
            );
        }
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        let execution = execute(
            &mut service,
            &child,
            AgentAction {
                id: "first-loop-result".into(),
                payload: AgentActionPayload::Say {
                    status: SayStatus::Final,
                    text: "EXACT_FIRST_ITERATION_OUTPUT".into(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                },
            },
        );
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        let later = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        assert_ne!(
            later.turn_id, child.turn_id,
            "the real controller must create its next iteration"
        );
        if tracked {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Running,
                "exact first task result was lost at the loop controller early return"
            );
            let outcome: serde_json::Value = serde_json::from_str(
                service.agent_turn_executions()[&parent.turn_id].action_results[0]
                    .structured_content_json
                    .as_deref()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(outcome["state"], "peer_terminal");
            assert_eq!(outcome["progress_witness"], true);
            let source = service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .find(|b| b.label.starts_with("request-handoff:"))
                .unwrap();
            assert!(source.content.contains("EXACT_FIRST_ITERATION_OUTPUT"));
            assert!(source.content.contains(&child.turn_id));
            assert!(!source.content.contains(&later.turn_id));
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
            let before = service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .to_vec();
            service
                .emit_subagent_task_result_for_execution(&child, &execution)
                .unwrap();
            assert_eq!(
                service.agent_turn_contexts()[&parent.turn_id].blocks(),
                before
            );
        } else {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            assert!(
                !service.agent_turn_contexts()[&parent.turn_id]
                    .blocks()
                    .iter()
                    .any(|b| b.label.starts_with("request-handoff:"))
            );
        }
        // Cleanup stays with the fixture's real lifecycle owners.
        service.terminate_all_pane_processes().unwrap();
    }
}

/// A genuine child can finish between accepted send and selected wait. Its
/// retained result must charge actual owned String capacity, not only bounded
/// text length: formatter growth can exceed the already reserved handoff bytes.
/// ASCII and multibyte full-size outputs must fit before park, retain their
/// exact attempt, then consume and release once through the normal wait owner.
#[test]
fn request_bound_terminal_cached_allocation_fits_preaccepted_handoff_headroom() {
    for output in ["x".repeat(3072), "界".repeat(1024), "界".repeat(1025)] {
        let (mut service, parent, _) = fixture();
        service.configure_agent_scheduler_limit(2).unwrap();
        let child = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        let message = send(&mut service, &parent, &child.agent_id, true);
        execute(
            &mut service,
            &child,
            AgentAction {
                id: "budgeted-terminal".into(),
                payload: AgentActionPayload::Say {
                    status: SayStatus::Final,
                    text: output,
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                },
            },
        );
        let allocations = service.tracked_request_outcome_allocations_for_tests();
        assert_eq!(
            allocations.len(),
            1,
            "pre-park result must retain actual request custody"
        );
        let (length, capacity, reserved) = allocations[0];
        assert!(
            capacity <= reserved,
            "cached terminal allocation exceeds its preaccepted headroom: length={length} capacity={capacity} reserved={reserved}"
        );
        assert_eq!(
            capacity, length,
            "cached outcome retains uncharged formatting slack"
        );
        wait(&mut service, &parent, &message, None);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        assert!(
            service
                .tracked_request_outcome_allocations_for_tests()
                .is_empty()
        );
        let outcome: serde_json::Value = serde_json::from_str(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(outcome["state"], "peer_terminal");
        assert_eq!(outcome["progress_witness"], true);
        execute(
            &mut service,
            &parent,
            AgentAction {
                id: "close-budgeted-child".into(),
                payload: AgentActionPayload::CloseAgent {
                    agent_id: child.agent_id.clone(),
                },
            },
        );
        finish(&mut service, &parent);
    }
}

/// A reverse response accepted by the original recipient before retirement
/// remains authentic while awaiting canonical commit. Availability recovery
/// must not consume the receipt ahead of that real reply, both before parking
/// and after an actual targeted wait; IDs and correlation alone grant no proof.
#[test]
fn request_bound_reply_accepted_before_sender_retirement_reaches_selected_wait() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if !before_park {
            wait(&mut service, &parent, &message, None);
        }
        let reply = accept_reply(
            &mut service,
            &parent,
            &peer,
            "original-interval-reply",
            Some(&message),
        );
        assert!(
            service
                .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
                .unwrap()
        );
        assert!(
            service
                .message_service()
                .live_acceptance_proof(reply)
                .is_some()
        );
        if before_park {
            wait(&mut service, &parent, &message, None);
        }
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        let outcome: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            outcome["state"], "reply",
            "registration retirement overtook an authentic accepted reverse reply"
        );
        assert_eq!(outcome["progress_witness"], true);
        let context = &service.agent_turn_contexts()[&parent.turn_id];
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|b| b.label.starts_with("request-handoff:")
                    && b.content
                        .contains("Authenticated matching response committed"))
                .count(),
            1
        );
        assert!(
            context
                .blocks()
                .iter()
                .any(|b| b.content.contains("original-interval-reply"))
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// Receive selection and retirement deferral must use the same trusted delivery
/// clock snapshot. A response still live at that snapshot's inclusive TTL edge
/// cannot be discarded because a later wall-clock read classifies it expired.
/// Only actual canonical commit qualifies the original registration's reply.
#[test]
fn request_bound_reply_retirement_uses_delivery_snapshot_for_ttl() {
    for (now_ms, expected_state, progress, terminal_first) in [
        (2, "reply", true, false),
        (3, "peer_unavailable", false, false),
        (2, "peer_unavailable", false, true),
    ] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        wait(&mut service, &parent, &message, None);
        service
            .message_service_mut()
            .accept_at_with_scope(
                &peer.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: "delivery-clock-reply".into(),
                    message_type: "send".into(),
                    time: "fixture:1".into(),
                    sender: peer.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(
                        AgentId::opaque(parent.agent_id.clone()).unwrap(),
                    ),
                    correlation_id: Some(message),
                    ttl_ms: Some(1),
                    content_type: "text/plain".into(),
                    payload: "reply live at selected delivery clock".into(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                1,
            )
            .unwrap();
        assert!(
            service
                .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
                .unwrap()
        );
        if terminal_first {
            service
                .message_service_mut()
                .settle_delivery_outcomes_at(3)
                .unwrap();
        }
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap();
        let outcome: serde_json::Value = serde_json::from_str(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            outcome["state"], expected_state,
            "retirement recovery used a different clock than receive selection"
        );
        assert_eq!(outcome["progress_witness"], progress);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// Two genuine requests can capture the same live owned attempt before it
/// completes. Each selected request must receive that exact result and release
/// its own reservation, while consumption of the first result prevents the
/// cohort from counting the same terminal work as progress again. A duplicate
/// native terminal observation between selections cannot reset that witness.
#[test]
fn request_bound_terminal_cohort_consumes_progress_once_across_requests() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    let first = send(&mut service, &parent, &child.agent_id, true);
    let second = send(&mut service, &parent, &child.agent_id, true);
    assert_ne!(first, second);
    wait(&mut service, &parent, &first, None);
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "exact cohort task terminal",
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_terminal");
    assert_eq!(outcome["progress_witness"], true);
    let first_generation = outcome["dependency_generation"].clone();
    assert_ne!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Completed)
        .unwrap();
    wait(&mut service, &parent, &second, None);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_terminal");
    assert_eq!(outcome["progress_witness"], false);
    assert_ne!(outcome["dependency_generation"], first_generation);
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    let sources: Vec<_> = context
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("request-handoff:"))
        .collect();
    assert_eq!(sources.len(), 2);
    assert!(
        sources
            .iter()
            .all(|block| block.content.contains(&child.turn_id))
    );
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-cohort-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// Tracking an idle persistent peer freezes registration-only evidence because
/// no task is live at acceptance. Delivery may legitimately start a later
/// receive-triggered task, but that task's successful terminal work must never
/// be adopted by the original request. Only actual registration retirement
/// then settles it as opaque unavailability with no new progress witness.
#[test]
fn request_bound_idle_peer_does_not_adopt_later_receive_task_terminal() {
    let (mut service, parent, _, primary) = fixture_with_primary();
    service.configure_agent_scheduler_limit(2).unwrap();
    let initial = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&initial.pane_id)
            .unwrap()
    );
    service.start_ready_agent_turns().unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &initial,
            AgentTurnState::Completed,
            "leave persistent peer idle before acceptance",
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    let message = send(&mut service, &parent, &initial.agent_id, true);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let later = service
        .agent_turn_ledger()
        .latest_turn_for_pane(&initial.pane_id)
        .unwrap()
        .clone();
    assert_ne!(later.turn_id, initial.turn_id);
    assert_eq!(later.state, AgentTurnState::Running);
    wait(&mut service, &parent, &message, None);
    service
        .complete_running_agent_turn_and_start_ready(
            &later,
            AgentTurnState::Completed,
            "later unbound task must not satisfy earlier request",
        )
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
        AgentTurnState::Blocked,
        "request adopted a task that was not live at acceptance"
    );
    assert_eq!(
        service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
        ActionStatus::Running
    );
    assert!(
        !service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label.starts_with("request-handoff:"))
    );
    assert_ne!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    service
        .dispatch_runtime_pane_close(
            &primary,
            &serde_json::json!({"pane_id":initial.pane_id,"force":true}).to_string(),
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_unavailable");
    assert_eq!(outcome["progress_witness"], false);
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}
