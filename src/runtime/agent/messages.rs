//! Runtime agent MMP message action helpers.
//!
//! This module owns provider-produced `send_message` execution and sender
//! identity management for the runtime agent harness. It keeps envelope
//! validation, message-service delivery, and provider-continuation context
//! handling together.

use super::*;

impl RuntimeSessionService {
    /// Runs the execute running message actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn execute_running_message_actions_for_turn(
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
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "send_message")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
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
        let delivery = match self
            .message_service
            .accept_at(&sender.agent_id, envelope, now_ms)
        {
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
    pub(in crate::runtime) fn runtime_message_sender_identity(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("turn agent id is invalid for MMP"))?;
        let pane_id = PaneId::parse('%', turn.pane_id.clone());
        let window_id = self
            .find_pane_descriptor(&turn.pane_id)
            .map(|descriptor| descriptor.window_id);
        Ok(self.message_service.ensure_agent_identity(
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
