//! Runtime tests for actions messaging behavior.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::runtime::ControlIdempotencyCache;
use crate::runtime::{current_unix_millis, current_unix_seconds};
use mez_agent::messaging::Envelope;
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
        .accept_at(&sender.agent_id, envelope, now_ms)
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

/// Verifies `wait` releases provider capacity and model-originated MMP mail
/// resumes the same turn instead of creating a new message-triggered turn.
///
/// The parked action must remain nonterminal, preserve pane and agent claims,
/// settle exactly once after peer mail is committed, and queue one ordinary
/// provider continuation with the original turn identity.
#[test]
fn runtime_wait_parks_and_peer_mail_resumes_same_turn() {
    let mut service = test_runtime_service();
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
    assert!(service.agent_turn_is_waiting_for_peer_message(&started.turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    let parked = service.agent_scheduler().snapshot();
    assert_eq!(parked.running, 0);
    assert_eq!(parked.waiting, 1);
    assert_eq!(parked.active_capacity_used, 0);

    let now_ms = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque(started.agent_id.clone()).unwrap();
    service
        .control
        .message_service_mut()
        .accept_at(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "wait-bridge-status-1".to_string(),
                message_type: "task_status".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
                correlation_id: Some(started.turn_id.clone()),
                ttl_ms: None,
                content_type: "application/json".to_string(),
                payload: mez_agent::messaging::TaskStatusPayload {
                    task_id: "peer-task".to_string(),
                    state: mez_agent::messaging::TaskState::Running,
                    progress_percent: Some(50),
                    summary: "peer task still running".to_string(),
                }
                .to_json(),
                extension_fields: Vec::new(),
            },
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
        service.agent_turn_is_waiting_for_peer_message(&started.turn_id),
        "runtime-owned bridge traffic must not wake a peer wait"
    );
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);

    service
        .control
        .message_service_mut()
        .accept_at(
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
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(!service.agent_turn_is_waiting_for_peer_message(&started.turn_id));
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let resumed = service.agent_scheduler().snapshot();
    assert_eq!(resumed.waiting, 0);
    assert_eq!(resumed.running, 1);
    assert_eq!(resumed.active_capacity_used, 1);
    assert_eq!(service.agent_peer_message_turn_count(&started.agent_id), 0);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
    let settled = service
        .agent_turn_executions()
        .get(&started.turn_id)
        .unwrap();
    assert_eq!(settled.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        settled.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|value| value.contains("peer_message"))
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
        .accept_at(
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
        .accept_at(&sender.agent_id, envelope, now_ms)
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
        .accept_at(
            &sender.agent_id,
            peer_message("peer-echo-1", "alpha beta gamma delta epsilon"),
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
        echoed.iter().any(|line| line == "▐ gamma delta epsilon"),
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
        .accept_at(
            &sender.agent_id,
            peer_message("peer-echo-2", "cwd ok"),
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
    assert_eq!(peer_blocks.len(), 2, "{peer_blocks:#?}");
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

/// Verifies one accepted `send_message` logs a single sent line in the sender
/// pane, while a rejected recipient or a failed transport logs nothing.
///
/// The outbound echo names the recipient at the destination end of the direction
/// arrow using the same recipient label the action result reports, and it only
/// ever describes delivery that happened: an invalid recipient and a transport
/// failure both return before the echo, so an operator never reads a line for a
/// message that was never queued.
#[test]
fn runtime_send_message_echo_logs_only_accepted_delivery() {
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent-%2", "text/plain", "ack, running now");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        sent.iter()
            .filter(|line| line.contains("agent-%2< "))
            .count(),
        1,
        "{sent:#?}"
    );
    assert!(
        sent.iter()
            .any(|line| line == "▐ agent-%2< ack, running now"),
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
        !rejected_lines.iter().any(|line| line.contains("< ")),
        "{rejected_lines:#?}"
    );
    rejected.terminate_all_pane_processes().unwrap();

    let (mut undeliverable, execution, _target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .expect("transport failure")
            .code,
        "transport_error"
    );
    let undeliverable_lines = peer_echo_pane_lines(&undeliverable, "%1");
    assert!(
        !undeliverable_lines.iter().any(|line| line.contains("< ")),
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
        .accept_at(
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
            .accept_at(&sender, envelope, now_ms)
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
        "only the model message logs: both committed bridge notifications stay silent \
         because each one already has its dedicated `subagent ...` line: {committed:#?}"
    );
    let committed_text = compact(committed.clone());
    assert_eq!(
        committed_text.matches("mixedpeerrequest").count(),
        1,
        "the committed model message logs exactly once: {committed_text}"
    );
    for suppressed in ["bridgeevidenceone", "bridgeevidencetwo", "taskid"] {
        assert_eq!(
            committed_text.matches(suppressed).count(),
            0,
            "a bridge echo is suppressed before any row exists, so {suppressed} must not \
             reach the pane log: {committed_text}"
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
        "a suppressed bridge arrival on an active turn logs nothing: {active}"
    );

    // A `task_result` bridge payload does carry an `output` field, so the bridge
    // gate rather than the JSON projection has to keep it out of the log.
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
        "a committed `task_result` bridge notification logs no echo row, so the model \
         message line stays the only one: {projected:#?}"
    );
    let projected_text = compact(projected);
    assert_eq!(
        projected_text.matches("taskcomplete").count(),
        0,
        "the bridge result payload is never projected in normal mode: {projected_text}"
    );
    for omitted in ["bridgeresult", "success"] {
        assert_eq!(
            projected_text.matches(omitted).count(),
            0,
            "a suppressed bridge payload reaches no row, so {omitted} must not be logged: \
             {projected_text}"
        );
    }

    // Verbose mode restores the bridge echo and logs the whole bounded payload
    // instead of the `output` projection.
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
        "verbose mode logs the whole bounded payload rather than its `output` projection: \
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

/// Verifies model-authored peer mail still logs in both directions in the
/// default normal mode, including a child with no subagent display name.
///
/// The bridge gate keys on runtime-authored envelope provenance, never on the
/// optional `subagent_display_name` extension or on delegation lineage, so a
/// model `send_message` between a parent and a child keeps its `{name}> ` and
/// `{name}< ` rows exactly as before.
#[test]
fn runtime_model_peer_mail_without_bridge_provenance_still_logs_both_directions() {
    // Parent -> child: the accepted outbound action echo names the recipient even
    // though the child identity carries no subagent display name.
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent:agent-%2", "text/plain", "parent reply");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert!(
        sent.iter()
            .any(|line| line == "▐ agent:agent-%2< parent reply"),
        "a model-authored outbound message keeps its recipient echo: {sent:#?}"
    );
    service.terminate_all_pane_processes().unwrap();

    // Child -> parent: the committed inbound echo names the sender. The child has
    // no lineage and no display name, and one case carries a
    // `subagent_display_name` field on a `send` envelope, so the gate provably
    // depends on neither.
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
            .accept_at(&sender, envelope, now_ms)
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
        .accept_at(&child_identity.agent_id, status_envelope, now_ms)
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
        .accept_at(&child_identity.agent_id, peer_envelope, now_ms)
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
        .accept_at(&sender.agent_id, envelope, now_ms)
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
        .accept_at(
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
        .accept_at(
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
        payload: mez_agent::AgentActionPayload::ListAgents { agent_type: None },
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
        payload: mez_agent::AgentActionPayload::ListAgents { agent_type: None },
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
        payload: mez_agent::AgentActionPayload::ListAgents { agent_type: None },
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
        .accept_at(
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
            .accept_at(
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
    service
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
    assert_eq!(child.state, AgentTurnState::Running);
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
            "text/markdown",
            "hello worker",
            "MMP text payloads require content_type text/plain; charset=utf-8",
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
) -> serde_json::Value {
    let action = mez_agent::AgentAction {
        id: "list-agents-1".to_string(),

        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: agent_type.map(str::to_string),
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

    let primary = execute_list_agents_action(&mut service, &turn, None);
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

    let internal = execute_list_agents_action(&mut service, &turn, Some("internal"));
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

    let all = execute_list_agents_action(&mut service, &turn, Some("all"));
    let all_ids = all["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for agent_id in ["agent-peer", "agent-internal", turn.agent_id.as_str()] {
        assert!(all_ids.contains(&agent_id.to_string()), "{agent_id}");
    }

    let subagents = execute_list_agents_action(&mut service, &turn, Some("subagent"));
    assert_eq!(subagents["count"], 0);
    assert!(subagents["agents"].as_array().unwrap().is_empty());
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

    let listed = execute_list_agents_action(&mut service, &turn, Some("subagent"));
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

    let all = execute_list_agents_action(&mut service, &turn, Some("all"));
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

    let all = execute_list_agents_action(&mut service, &turn, Some("all"));
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
