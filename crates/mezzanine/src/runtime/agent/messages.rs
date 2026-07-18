//! Runtime agent MMP message action helpers.
//!
//! This module owns provider-produced `send_message` execution and sender
//! identity management for the runtime agent harness. It keeps envelope
//! validation, message-service delivery, and provider-continuation context
//! handling together.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, Envelope, MezError, PaneId, Result, RuntimeSessionService,
    SenderIdentity, current_unix_seconds, json_escape,
    runtime_agent_turn_state_from_action_results,
    runtime_execution_ready_for_provider_continuation, runtime_maap_message_content_type,
    runtime_message_recipient, runtime_mezzanine_error_code, validate_mmp_payload_metadata,
};

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
    pub(crate) fn deliver_pending_runtime_agent_messages(&mut self, now_ms: u64) -> Result<usize> {
        let ready = self
            .control
            .message_service()
            .fanout_ready(now_ms, usize::MAX);
        let mut committed = 0usize;
        for fanout in ready {
            let recipient = fanout.recipient;
            if !recipient.as_str().starts_with("agent-") {
                continue;
            }
            let Some(turn) = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .rev()
                .find(|turn| {
                    turn.agent_id == recipient.as_str()
                        && matches!(
                            turn.state,
                            AgentTurnState::Queued
                                | AgentTurnState::Running
                                | AgentTurnState::Blocked
                        )
                })
                .cloned()
            else {
                continue;
            };
            if !self.agent_turn_contexts().contains_key(&turn.turn_id) {
                continue;
            }

            for message in fanout.batch.messages {
                let label = format!(
                    "local message sequence {} id {}",
                    message.sequence, message.envelope.id
                );
                let already_committed =
                    self.agent_turn_contexts()
                        .get(&turn.turn_id)
                        .is_some_and(|context| {
                            context.blocks().iter().any(|block| {
                                block.source == mez_agent::ContextSourceKind::LocalMessage
                                    && block.label == label
                            })
                        });
                if !already_committed {
                    let content = crate::runtime::control::runtime_local_message_context_content(
                        &message.envelope,
                    );
                    self.agent_turn_contexts_mut()
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn context is unavailable")
                        })?
                        .append_reference_event(
                            mez_agent::ContextSourceKind::LocalMessage,
                            label,
                            content,
                        )?;
                    committed = committed.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "local_message committed sequence={} event_high_water={}",
                            message.sequence,
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
        let recipient_target = runtime_message_recipient(recipient)?;
        let message_id = format!("{}:{}", turn.turn_id, action.id);
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let envelope = Envelope {
            protocol: "mmp/1",
            id: message_id.clone(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender.clone(),
            recipient: recipient_target,
            correlation_id: Some(turn.turn_id.clone()),
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
        Ok(self.control.message_service_mut().ensure_agent_identity(
            SenderIdentity {
                agent_id,
                pane_id,
                window_id,
                role: Some("agent".to_string()),
                capabilities: vec!["agent-harness".to_string()],
            },
            current_unix_seconds().saturating_mul(1000),
        )?)
    }
}
