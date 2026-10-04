//! Peer wait, scheduler reacquisition and provider-generation regressions.
//!
//! Model mail resumes the original parked turn; runtime status traffic does not.
//! Committed newer context invalidates an older provider generation before dispatch.

use super::*;

/// Mail may settle a peer wait and request fair reacquisition, but it cannot
/// remove human inhibition. Explicit resume queues exactly one continuation.
#[test]
fn runtime_human_pause_peer_mail_cannot_unpause_wait() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "wait while human paused")
        .unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "wait".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "wait for mail".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "wait-paused".into(),
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
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let generation = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let now = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "paused-mail".into(),
                message_type: "send".into(),
                time: format!("runtime:{now}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    AgentId::opaque(started.agent_id.clone()).unwrap(),
                ),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "Continue now".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    service.deliver_pending_runtime_agent_messages(now).unwrap();
    assert_eq!(service.agent_human_pause_status("%1"), Some("paused"));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    assert!(
        service
            .resume_agent_lifecycle_target(&primary, &target, generation)
            .unwrap()
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    service.terminate_all_pane_processes().unwrap();
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
