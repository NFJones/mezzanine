//! Runtime agent MMP message action helpers.
//!
//! This module owns provider-produced `send_message` execution and sender
//! identity management for the runtime agent harness. It keeps envelope
//! validation, message-service delivery, and provider-continuation context
//! handling together.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, Envelope, EventKind, MezError, PaneId, Result,
    RuntimeSessionService, RuntimeSideEffect, ScheduledWork, SenderIdentity, current_unix_seconds,
    json_escape, runtime_agent_turn_state_from_action_results,
    runtime_execution_ready_for_provider_continuation, runtime_maap_message_content_type,
    runtime_message_recipient, runtime_mezzanine_error_code, validate_mmp_payload_metadata,
};
use crate::runtime::{RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition};

/// Cadence of the dedicated peer-message delivery and expiry sweep.
///
/// The interval matches the runtime's short status-refresh cadence so pending
/// peer delivery and TTL expiry stay prompt without adding a permanent busy
/// tick. This timer is independent of the runtime actor tick.
pub(crate) const PEER_MESSAGE_DELIVERY_INTERVAL_MS: u64 = 1_000;

/// Maximum characters carried by one prompt-derived fallback objective.
///
/// The fallback exists only for a turn whose response carried no
/// model-authored objective. It summarizes the turn's own text into one bounded
/// factual status line instead of republishing that text, and the shared
/// objective bounds still apply to the result.
pub(crate) const RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS: usize = 160;

impl RuntimeSessionService {
    /// Commits unread runtime-agent messages at the actor boundary in delivery
    /// sequence order and acknowledges each message only after its canonical
    /// reference event exists.
    ///
    /// Messages for agents without an active queued, running, or blocked turn
    /// remain behind the durable delivery cursor. A later prompt builder places
    /// them before that task's prelude and prompt. Messages for an active turn
    /// append at arrival time; an older in-flight provider claim will therefore
    /// be rejected by the canonical event high-water check.
    ///
    /// Only model-originated peer mail starts a turn for an idle recipient.
    /// Runtime-owned bridge and lifecycle notifications keep their
    /// pre-idle-turn behavior: they wait behind the durable cursor and are
    /// injected with the recipient's next turn.
    ///
    /// Every committed message is echoed once in the recipient pane log, and
    /// nothing else is. The echo tracks the canonical peer-message blocks this
    /// pass commits through either path, so it never describes a
    /// budget-limited fanout batch and never filters by sender type.
    pub(crate) fn deliver_pending_runtime_agent_messages(&mut self, now_ms: u64) -> Result<usize> {
        let ready = self
            .control
            .message_service_mut()
            .fanout_ready(now_ms, usize::MAX);
        let mut committed = 0usize;
        for fanout in ready {
            let recipient = fanout.recipient;
            if !recipient.as_str().starts_with("agent-") {
                continue;
            }
            let Some(turn) = self.runtime_agent_active_turn(recipient.as_str()) else {
                if fanout.batch.messages.iter().all(|message| {
                    crate::runtime::control::runtime_owned_bridge_message(&message.envelope)
                }) {
                    continue;
                }
                let pane_id = recipient.as_str().trim_start_matches("agent-").to_string();
                // The echo follows the commit rather than this batch. Starting
                // the turn commits every unread message through
                // `peer_message_turn_context`, which logs each committed
                // message, so a message this budget-limited batch omitted is
                // still operator-visible. The loop limit, a missing session,
                // and any other refusal commit nothing and log nothing.
                let started = self.start_runtime_peer_message_turn(&pane_id)?;
                committed = committed.saturating_add(started);
                continue;
            };
            if !self.agent_turn_contexts().contains_key(&turn.turn_id) {
                continue;
            }

            let mut model_message_count = 0usize;
            for message in fanout.batch.messages {
                let label = crate::runtime::control::runtime_peer_message_block_label(
                    message.sequence,
                    message.envelope.id.as_str(),
                );
                let already_committed =
                    self.agent_turn_contexts()
                        .get(&turn.turn_id)
                        .is_some_and(|context| {
                            context.blocks().iter().any(|block| {
                                block.source == mez_agent::ContextSourceKind::PeerMessage
                                    && block.label == label
                            })
                        });
                if !already_committed {
                    if !crate::runtime::control::runtime_owned_bridge_message(&message.envelope) {
                        model_message_count = model_message_count.saturating_add(1);
                    }
                    let content = crate::runtime::control::runtime_peer_message_context_content(
                        &message.envelope,
                    );
                    self.agent_turn_contexts_mut()
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn context is unavailable")
                        })?
                        .append_peer_message_event(label, content)?;
                    self.echo_received_peer_message_to_pane(&turn.pane_id, &message.envelope);
                    committed = committed.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "peer_message committed sequence={} message_id={} event_high_water={}",
                            message.sequence,
                            message.envelope.id,
                            self.agent_turn_contexts()
                                .get(&turn.turn_id)
                                .map(mez_agent::AgentContext::event_sequence_high_water_mark)
                                .unwrap_or(0)
                        ),
                    )?;
                }
                self.control
                    .message_service_mut()
                    .advance_subscription(&recipient, message.sequence)?;
            }

            if model_message_count > 0 && self.resume_agent_peer_wait(&turn, model_message_count)? {
                continue;
            }
            if turn.state == AgentTurnState::Running
                && !self.agent_provider_task_is_owned(&turn.turn_id)
                && self
                    .agent_turn_executions()
                    .get(&turn.turn_id)
                    .is_none_or(runtime_execution_ready_for_provider_continuation)
            {
                self.queue_agent_provider_task(turn.turn_id.clone());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "provider_task queued reason=local_message_arrival",
                )?;
            }
        }
        Ok(committed)
    }

    /// Logs one committed received peer message in the recipient pane's log.
    ///
    /// Every path that commits a canonical peer-message block calls this once
    /// per committed message, so the logged set equals the committed set rather
    /// than a budget-limited fanout batch. Interagent traffic becomes
    /// operator-visible the same way a user prompt does, with the originating
    /// agent named at the destination end of the direction arrow, and
    /// runtime-owned bridge traffic follows the same commit rule so the log
    /// never depends on whether the recipient happened to be busy. The echo is
    /// presentation-only: it reuses the peer payload bound, appends no context
    /// block, and can never start a turn.
    pub(crate) fn echo_received_peer_message_to_pane(
        &mut self,
        pane_id: &str,
        envelope: &Envelope,
    ) {
        // Bridge provenance comes from runtime-authored envelope metadata, so a
        // model `send_message` always passes `false` and keeps logging unchanged.
        let runtime_bridge = crate::runtime::control::runtime_bridge_peer_message(envelope);
        let _ = self.append_agent_received_peer_message_to_terminal_buffer(
            pane_id,
            envelope.sender.agent_id.as_str(),
            envelope.content_type.as_str(),
            envelope.payload.as_str(),
            runtime_bridge,
        );
    }

    /// Starts one peer-message-triggered turn for an idle agent.
    ///
    /// A peer message starts one turn for an idle agent, including in ask mode,
    /// so agent pipelines can make progress. An agent that already has a queued,
    /// running, or blocked turn keeps the arrival-time append path instead. The
    /// configured peer-message loop limit bounds runaway message-triggered
    /// iterations; it never caps injected message counts, payload bytes, or
    /// per-window peer turns. The durable cursor advances only after the new turn
    /// stores the canonical peer-message events.
    fn start_runtime_peer_message_turn(&mut self, pane_id: &str) -> Result<usize> {
        let agent_id = format!("agent-{pane_id}");
        if self.agent_shell_store().get(pane_id).is_none() {
            return Ok(0);
        }
        if self.subagent_descendant_is_fenced(&agent_id) {
            return Ok(0);
        }
        let loop_limit = self.agent_peer_message_loop_limit();
        let started_turns = self.agent_peer_message_turn_count(&agent_id);
        if started_turns >= loop_limit {
            // The limit is a stable episode state rather than a per-tick event:
            // report it once, and stay silent until direct user input resets the
            // counter and clears the episode marker.
            if !self.agent_peer_message_limit_reported(&agent_id) {
                self.mark_agent_peer_message_limit_reported(&agent_id);
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: peer message loop limit {loop_limit} reached; leaving inbox mail pending"
                    ),
                )?;
                let _ = self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","kind":"peer_message_loop_limit","agent_id":"{}","started_turns":{},"loop_limit":{},"message":"peer-message loop limit reached; no new message-triggered turn was started"}}"#,
                        json_escape(pane_id),
                        json_escape(&agent_id),
                        started_turns,
                        loop_limit
                    ),
                );
            }
            return Ok(0);
        }
        let crate::runtime::control::RuntimePeerMessageTurnContext {
            context,
            delivered_message_sequence,
            delivered_message_count,
            imported_history_events,
        } = self.peer_message_turn_context(pane_id)?;
        let Some(delivered_message_sequence) = delivered_message_sequence else {
            return Ok(0);
        };
        let context = self.apply_agent_shell_preference_context(pane_id, context)?;
        let context = self.apply_persisted_context_documents(pane_id, context)?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("agent turn conversation is unavailable"))?;
        let turn_id = self.next_agent_turn_id();
        let context_blocks = context.blocks().len();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            conversation_id: conversation_id.clone(),
            agent_id: agent_id.clone(),
            pane_id: pane_id.to_string(),
            trigger: mez_agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: current_unix_seconds(),
            deadline_at_unix_millis: crate::runtime::current_unix_millis()
                .saturating_add(self.agent_turn_timeout_ms()),
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
            initial_capability: None,
        };
        self.agent_turn_ledger_mut().queue_turn(turn.clone())?;
        self.snapshot_agent_native_shell_timeout_for_turn(&turn_id);
        self.agent_turn_contexts_mut()
            .insert(turn_id.clone(), context);
        self.set_agent_turn_imported_history_events(turn_id.clone(), imported_history_events);
        self.set_agent_turn_model_profile(turn_id.clone(), model_profile);
        let recipient = AgentId::opaque(agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("runtime agent id is invalid for MMP"))?;
        self.control
            .message_service_mut()
            .advance_subscription(&recipient, delivered_message_sequence)?;
        self.set_agent_peer_message_turn_count(&agent_id, started_turns.saturating_add(1));
        self.clear_agent_peer_message_limit_reported(&agent_id);
        self.enqueue_agent_work(ScheduledWork {
            turn_id: turn_id.clone(),
            conversation_id,
            agent_id: agent_id.clone(),
            pane_id: Some(pane_id.to_string()),
            kind: mez_agent::ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "created state=queued reason=peer_message_arrival",
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={} delivered_messages={}",
                context_blocks, model_profile_name, delivered_message_count
            ),
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable reason=peer_message_arrival",
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!("agent: started turn {turn_id} from pending peer mail"),
        )?;
        self.start_ready_agent_turns()?;
        Ok(delivered_message_count)
    }

    /// Returns whether any recipient currently has deliverable peer mail.
    ///
    /// Mail that is waiting only on the peer-message loop limit is not
    /// deliverable: the limit is a stable terminal state for that agent until
    /// direct user input resets the counter, so it must not keep re-arming the
    /// delivery timer (and re-reporting the limit) once per tick. Runtime-owned
    /// bridge traffic never starts a turn, so it keeps the ordinary pending
    /// behavior that also drives TTL expiry.
    pub(crate) fn has_pending_peer_messages(&mut self, now_ms: u64) -> bool {
        let loop_limit = self.agent_peer_message_loop_limit();
        let ready = self.control.message_service_mut().fanout_ready(now_ms, 1);
        ready.into_iter().any(|fanout| {
            let recipient = fanout.recipient.as_str().to_string();
            if !recipient.starts_with("agent-") {
                return true;
            }
            if fanout.batch.messages.iter().all(|message| {
                crate::runtime::control::runtime_owned_bridge_message(&message.envelope)
            }) {
                return true;
            }
            if self.runtime_agent_active_turn(&recipient).is_some() {
                return true;
            }
            self.agent_peer_message_turn_count(&recipient) < loop_limit
        })
    }

    /// Returns the queued, running, or blocked turn owned by one agent.
    ///
    /// Delivery treats an agent with such a turn as active: peer mail appends at
    /// arrival time instead of starting a new message-triggered turn.
    fn runtime_agent_active_turn(&self, agent_id: &str) -> Option<AgentTurnRecord> {
        self.agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| {
                turn.agent_id == agent_id
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
    }

    /// Builds the desired dedicated peer-message delivery timer transition.
    ///
    /// The dedicated timer is armed only while deliverable peer mail exists and
    /// no delivery timer is already active, so it stays an event-driven wakeup
    /// rather than a permanent tick.
    pub(crate) fn peer_message_delivery_timer_transition(
        &mut self,
        timer_active: bool,
        generation: u64,
        now_ms: u64,
    ) -> RuntimeTransition {
        if timer_active || !self.has_pending_peer_messages(now_ms) {
            return RuntimeTransition::default();
        }
        RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::PeerMessageDelivery,
                    "peer-message-delivery",
                    generation,
                ),
                delay_ms: PEER_MESSAGE_DELIVERY_INTERVAL_MS,
            }],
        }
    }

    /// Applies one dedicated peer-message delivery and expiry pass.
    ///
    /// Returns the transition for the actor so it can re-arm the periodic sweep
    /// while deliverable mail remains. Expired or undeliverable envelopes keep
    /// their existing sender-visible message-service semantics.
    pub(crate) fn apply_peer_message_delivery_timer(
        &mut self,
        now_ms: u64,
        generation: u64,
    ) -> Result<RuntimeTransition> {
        let committed = self.deliver_pending_runtime_agent_messages(now_ms)?;
        let side_effects = if self.has_pending_peer_messages(now_ms) {
            vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::PeerMessageDelivery,
                    "peer-message-delivery",
                    generation,
                ),
                delay_ms: PEER_MESSAGE_DELIVERY_INTERVAL_MS,
            }]
        } else {
            Vec::new()
        };
        Ok(RuntimeTransition {
            applied: committed > 0,
            side_effects,
        })
    }

    /// Runs the execute running message actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn execute_running_message_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "send_message"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running message result does not match an action")
                })?;
            execution.action_results[index] =
                self.execute_message_action_for_turn(turn, &action)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(executed)
    }

    /// Runs the execute message action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_message_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let AgentActionPayload::SendMessage {
            recipient,
            content_type,
            payload,
            correlation_id,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "message execution requires a send_message action",
            ));
        };
        let content_type = runtime_maap_message_content_type(content_type);
        if let Err(error) = validate_mmp_payload_metadata("send", &content_type, payload, None) {
            let error = MezError::from(error);
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "invalid_message_payload",
                error.message().to_string(),
            )?;
            result.structured_content_json = Some(format!(
                r#"{{"recipient":"{}","content_type":"{}","message_id":null,"delivery_status":"rejected","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                json_escape(recipient),
                json_escape(&content_type),
                runtime_mezzanine_error_code(error.kind()),
                json_escape(error.message())
            ));
            return Ok(result);
        }
        if let Some(result) =
            self.queue_macro_managed_message_step(turn, action, recipient, &content_type, payload)?
        {
            return Ok(result);
        }
        let sender = self.runtime_message_sender_identity(turn)?;
        let recipient_target = match runtime_message_recipient(recipient) {
            Ok(target) => target,
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "invalid_message_recipient",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(
                    serde_json::json!({
                        "recipient": recipient,
                        "message_id": null,
                        "delivery_status": "rejected",
                        "delivery_applied": false,
                        "accepted_recipient_forms": [
                            "session", "agent:<id>", "pane:<id>", "window:<id>",
                            "role:<name>", "capability:<name>", "group:<name>"
                        ]
                    })
                    .to_string(),
                );
                return Ok(result);
            }
        };
        let message_id = format!("{}:{}", turn.turn_id, action.id);
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let envelope = Envelope {
            protocol: "mmp/1",
            id: message_id.clone(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender.clone(),
            recipient: recipient_target,
            correlation_id: correlation_id
                .clone()
                .or_else(|| Some(turn.turn_id.clone())),
            ttl_ms: None,
            content_type: content_type.clone(),
            payload: payload.clone(),
            extension_fields: Vec::new(),
        };
        let delivery = match self.control.message_service_mut().accept_at(
            &sender.agent_id,
            envelope,
            now_ms,
        ) {
            Ok(delivery) => delivery,
            Err(error) => {
                let error = MezError::from(error);
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "transport_error",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"recipient":"{}","message_id":null,"delivery_status":"failed","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                    json_escape(recipient),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ));
                return Ok(result);
            }
        };
        // The accepted delivery is operator-visible with the same recipient
        // label the action result reports, so a pane log pairs the outbound
        // request with the peer reply that follows it. A rejected recipient or
        // failed transport returns before this point and logs nothing.
        let _ = self.append_agent_sent_peer_message_to_terminal_buffer(
            &turn.pane_id,
            recipient.as_str(),
            content_type.as_str(),
            payload.as_str(),
            false,
        );
        self.deliver_pending_runtime_agent_messages(now_ms)?;
        Ok(ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "message {} delivered to {} recipient(s)",
                delivery.message_id, delivery.queued_recipients
            )],
            Some(format!(
                r#"{{"recipient":"{}","message_id":"{}","delivery_status":"accepted","queued_recipients":{},"sequence":{},"protocol_error":null}}"#,
                json_escape(recipient),
                json_escape(&delivery.message_id),
                delivery.queued_recipients,
                delivery.sequence
            )),
        ))
    }

    /// Runs the runtime message sender identity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn runtime_message_sender_identity(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("turn agent id is invalid for MMP"))?;
        let pane_id = PaneId::parse('%', turn.pane_id.clone());
        let window_id = self
            .find_pane_descriptor(&turn.pane_id)
            .map(|descriptor| descriptor.window_id);
        let objective = self.runtime_agent_effective_objective(
            &turn.conversation_id,
            self.runtime_agent_turn_objective(turn).as_deref(),
        );
        let identity = self.control.message_service_mut().ensure_agent_identity(
            SenderIdentity {
                agent_id,
                pane_id,
                window_id,
                role: Some("agent".to_string()),
                capabilities: vec!["agent-harness".to_string()],
                objective: objective.clone().flatten(),
            },
            current_unix_seconds().saturating_mul(1000),
        )?;
        if let Some(objective) = objective {
            self.publish_prepared_runtime_agent_objective(&turn.agent_id, objective.as_deref());
            self.mirror_runtime_agent_objective(&turn.conversation_id, objective.as_deref());
        }
        self.control
            .message_service()
            .registered_identity(&identity.agent_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("runtime MMP identity disappeared after refresh")
            })
    }

    /// Publishes one bounded agent objective for peer discovery.
    ///
    /// Returns true only when the published value changed. The objective is
    /// untrusted discovery data: it is normalized through the shared objective
    /// bounds, is never logged raw, and never authorizes anything. A failed
    /// refresh keeps the previous objective and never fails a turn.
    pub(crate) fn publish_prepared_runtime_agent_objective(
        &mut self,
        agent_id: &str,
        objective: Option<&str>,
    ) -> bool {
        let Some(agent_id) = AgentId::opaque(agent_id.to_string()) else {
            return false;
        };
        let now_ms = current_unix_seconds().saturating_mul(1000);
        self.control
            .message_service_mut()
            .update_agent_objective(&agent_id, objective, now_ms)
            .unwrap_or(false)
    }

    /// Resolves the objective that may be published for one conversation.
    ///
    /// A user-selected durable objective is authoritative until explicitly
    /// cleared. Persistence failures deliberately preserve automatic behavior
    /// for existing runtime-only conversations; the slash mutation boundary
    /// rejects unavailable persistence before accepting a user selection.
    pub(crate) fn runtime_agent_effective_objective(
        &self,
        conversation_id: &str,
        automatic: Option<&str>,
    ) -> Option<Option<String>> {
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return Some(automatic.map(ToOwned::to_owned));
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Some(automatic.map(ToOwned::to_owned));
        };
        match store.user_objective(conversation_id) {
            Ok(Some(objective)) => Some(Some(objective)),
            Ok(None) => match store.parent_objective(conversation_id) {
                Ok(Some(objective)) => Some(Some(objective)),
                Ok(None) => Some(automatic.map(ToOwned::to_owned)),
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    /// Synchronizes a pane agent's MMP identity to its currently bound conversation.
    ///
    /// Durable user objectives are restored immediately. Conversations without
    /// a user selection explicitly clear the previous identity value so a
    /// resume, fresh conversation, or fork cannot expose another conversation's
    /// automatic or user-selected objective.
    pub(crate) fn sync_runtime_agent_objective_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        let objective = if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            None
        } else if let Some(store) = self.persistence.cloned_transcript_store() {
            store.effective_persisted_objective(conversation_id)?
        } else {
            None
        };
        self.sync_runtime_agent_objective_for_pane(pane_id, conversation_id, objective.as_deref())
    }

    /// Synchronizes one pane identity after that pane changes conversation bindings.
    fn sync_runtime_agent_objective_for_pane(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        let agent_id = format!("agent-{pane_id}");
        self.ensure_runtime_message_identity(
            &agent_id,
            None,
            "agent",
            &["agent-harness"],
            current_unix_seconds().saturating_mul(1000),
        )?;
        let agent_id = mez_core::ids::AgentId::opaque(agent_id)
            .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
        if let Some(objective) = objective {
            self.control.message_service_mut().update_agent_objective(
                &agent_id,
                Some(objective),
                current_unix_seconds().saturating_mul(1000),
            )?;
        } else {
            self.control
                .message_service_mut()
                .clear_agent_objective(&agent_id, current_unix_seconds().saturating_mul(1000))?;
        }
        self.mirror_runtime_agent_objective(conversation_id, objective);
        Ok(())
    }

    /// Synchronizes every live durable pane bound to one conversation.
    ///
    /// A conversation may remain live in more than one pane. An explicit user
    /// set or clear therefore updates every authenticated pane identity rather
    /// than leaving peers to observe a stale objective from a sibling pane.
    pub(crate) fn sync_runtime_agent_objectives_for_conversation(
        &mut self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        let pane_ids = self
            .agent_shell_store()
            .sessions()
            .filter(|session| !session.ephemeral && session.session_id == conversation_id)
            .map(|session| session.pane_id.clone())
            .collect::<Vec<_>>();
        for pane_id in pane_ids {
            let agent_id = format!("agent-{pane_id}");
            self.ensure_runtime_message_identity(
                &agent_id,
                None,
                "agent",
                &["agent-harness"],
                current_unix_seconds().saturating_mul(1000),
            )?;
            let agent_id = mez_core::ids::AgentId::opaque(agent_id)
                .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
            if let Some(objective) = objective {
                self.control.message_service_mut().update_agent_objective(
                    &agent_id,
                    Some(objective),
                    current_unix_seconds().saturating_mul(1000),
                )?;
            } else {
                self.control.message_service_mut().clear_agent_objective(
                    &agent_id,
                    current_unix_seconds().saturating_mul(1000),
                )?;
            }
        }
        self.mirror_runtime_agent_objective(conversation_id, objective);
        Ok(())
    }

    /// Synchronizes one pane's already validated objective during a staged
    /// restore or resume commit.
    pub(crate) fn sync_prepared_runtime_agent_objective_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        self.sync_runtime_agent_objective_for_pane(pane_id, conversation_id, objective)
    }

    /// Persists one bounded display mirror of a published agent objective.
    ///
    /// The mirror exists so archived and offline conversations can still resolve
    /// a policy-derived title. It is written only from the published objective
    /// value, is bounded by the shared title rules, and never authorizes
    /// anything. The mirror is a cache: a missing, unchanged, or unreadable
    /// mirror is not an error and never fails a turn. A title change also
    /// refreshes the cached prompt-selector candidates and any open saved-session
    /// browser so every surface renders the same row title.
    ///
    /// Ephemeral conversations (routed workers) never persist a transcript and
    /// never enter the saved-session catalog, so they are skipped entirely rather
    /// than growing the bounded mirror index. A published objective that bounds
    /// to nothing retires any retained mirror so the row resolves its title from
    /// the first prompt again.
    pub(crate) fn mirror_runtime_agent_objective(
        &mut self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> bool {
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return false;
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return false;
        };
        let changed = store
            .mirror_session_objective(
                conversation_id,
                objective.unwrap_or_default(),
                current_unix_seconds(),
            )
            .unwrap_or(false);
        if changed {
            self.invalidate_agent_prompt_selector_extra_candidates();
            let _ = self.refresh_saved_session_overlay_after_title_change();
        }
        // Publishing the objective is where a conversation becomes eligible for
        // turn-less generated-title work. Admission is idempotent, never creates
        // a turn, and never fails the objective refresh.
        let _ = self.schedule_runtime_agent_session_title(conversation_id, objective);
        changed
    }

    /// Reports whether one conversation belongs to an ephemeral agent session.
    ///
    /// Routed workers bind fresh runtime-only conversation ids such as
    /// `routed-<parent>-<turn>-worker`. They never persist a transcript, never
    /// enter the saved-session catalog, and can never resolve a mirrored title,
    /// so mirroring them would only grow the bounded index. Title scheduling
    /// shares this guard, because a generated title for such a conversation would
    /// spend a provider call and an index write that no surface can ever render.
    pub(crate) fn runtime_agent_conversation_is_ephemeral(&self, conversation_id: &str) -> bool {
        self.agent_shell_store()
            .sessions()
            .any(|session| session.ephemeral && session.session_id == conversation_id)
    }

    /// Publishes the objective implied by one provider response for a turn.
    ///
    /// A model-authored objective carried in the turn response envelope wins; a
    /// turn that yields none falls back to the bounded, non-verbatim derivation
    /// in [`Self::runtime_agent_objective_from_prompt`] so the objective still
    /// refreshes at least once per turn. Identical values publish nothing, and a
    /// turn that produces neither stays a no-op that keeps the previously
    /// published objective.
    pub(crate) fn publish_runtime_agent_objective_for_response(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> bool {
        let automatic = mez_agent::parse_maap_batch_objective(&execution.response.raw_text)
            .or_else(|| self.runtime_agent_turn_objective(turn));
        let Some(objective) =
            self.runtime_agent_effective_objective(&turn.conversation_id, automatic.as_deref())
        else {
            return false;
        };
        let changed =
            self.publish_prepared_runtime_agent_objective(&turn.agent_id, objective.as_deref());
        self.mirror_runtime_agent_objective(&turn.conversation_id, objective.as_deref());
        changed
    }

    /// Returns the bounded fallback objective implied by one turn's own prompt.
    pub(crate) fn runtime_agent_turn_objective(&self, turn: &AgentTurnRecord) -> Option<String> {
        let context = self.agent_turn_contexts().get(&turn.turn_id)?;
        let prompt = context
            .blocks()
            .iter()
            .rev()
            .find(|block| block.source == mez_agent::ContextSourceKind::UserInstruction)
            .or_else(|| context.blocks().last())?
            .content
            .as_str();
        Self::runtime_agent_objective_from_prompt(prompt)
    }

    /// Derives one bounded fallback objective from turn prompt or task text.
    ///
    /// This is the bounded last-resort source for a turn whose response carried
    /// no model-authored objective: the model-generated `objective` turn field is
    /// the primary source (see `maap_action_batch_schema` and the peer-messaging
    /// prompt section). The derivation is deliberately non-verbatim, because a
    /// published objective is a factual statement of the agent's current work and
    /// never raw prompt text: it keeps the first task-bearing line, drops leading
    /// heading, quote, and list markers, collapses whitespace, and truncates at
    /// [`RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS`] on a word boundary. Prompt
    /// ingestion rejects nothing for spanning lines, so an ordinary multi-line
    /// prompt still yields a value; text that still cannot satisfy the shared
    /// objective bounds yields nothing and the previous objective stays in place.
    pub(crate) fn runtime_agent_objective_from_prompt(prompt: &str) -> Option<String> {
        let first_line = prompt.lines().find(|line| !line.trim().is_empty())?;
        let stripped = first_line.trim().trim_start_matches(|character: char| {
            character.is_ascii_digit()
                || matches!(
                    character,
                    '#' | '*' | '-' | '>' | '`' | '|' | '.' | ')' | ' '
                )
        });
        let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return None;
        }
        let bounded = match collapsed
            .char_indices()
            .nth(RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS)
        {
            None => collapsed,
            Some((cut, _)) => {
                let head = &collapsed[..cut];
                let word_end = head.rfind(' ').unwrap_or(head.len());
                format!("{}...", head[..word_end].trim_end())
            }
        };
        mez_agent::messaging::normalize_objective(&bounded).ok()
    }
}
