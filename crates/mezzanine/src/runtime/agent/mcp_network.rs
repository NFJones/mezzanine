//! Runtime agent MCP and network action execution helpers.
//!
//! This module owns runtime-executed external integration actions. Keeping MCP
//! and network execution together isolates approval, loop-guard, hook, audit,
//! and provider-continuation context handling from the main runtime agent
//! facade.

#[cfg(test)]
use super::execute_mcp_action_through_runtime;
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, AuditActor, AuditRecord, BTreeMap, HookEvent,
    McpToolCallRequest, MezError, ReqwestProviderHttpTransport, Result,
    RuntimeApprovedExternalActionDispatch, RuntimeApprovedExternalActionOutcome,
    RuntimeApprovedMcpActionDispatch, RuntimeMcpActionExecutor, RuntimeSessionService,
    current_unix_seconds, execute_mcp_action_through_runtime_async,
    execute_network_action_with_transport_async, json_escape, network_action_plan,
    runtime_action_status_name, runtime_agent_action_summary,
    runtime_agent_turn_state_from_action_results,
    runtime_execution_ready_for_provider_continuation, runtime_mcp_error_code,
    runtime_mezzanine_error_code, runtime_post_mcp_hook_payload, runtime_pre_mcp_hook_payload,
};
use mez_agent::McpExecutionRequest;

impl RuntimeSessionService {
    /// Returns approved external actions that are ready for worker dispatch.
    pub(crate) fn pending_approved_external_actions(&self) -> Vec<(String, String)> {
        self.agent
            .pending_approved_external_actions
            .iter()
            .filter(|identity| {
                !self
                    .agent
                    .claimed_approved_external_actions
                    .contains(*identity)
            })
            .cloned()
            .collect()
    }

    /// Returns turns whose approved external work has a queued or active owner.
    pub(crate) fn approved_external_action_progress_turn_ids(&self) -> Vec<String> {
        self.agent
            .pending_approved_external_actions
            .iter()
            .chain(self.agent.claimed_approved_external_actions.iter())
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    }

    /// Claims one approved network or MCP action for async worker execution.
    pub(crate) fn claim_approved_external_action(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> Result<Option<RuntimeApprovedExternalActionDispatch>> {
        let identity = (turn_id.to_string(), action_id.to_string());
        if !self
            .agent
            .pending_approved_external_actions
            .contains(&identity)
            || !self
                .agent
                .claimed_approved_external_actions
                .insert(identity.clone())
        {
            return Ok(None);
        }
        match self.prepare_approved_external_action_dispatch(turn_id, action_id) {
            Ok(Some(dispatch)) => Ok(Some(dispatch)),
            Ok(None) => {
                self.agent
                    .claimed_approved_external_actions
                    .remove(&identity);
                Ok(None)
            }
            Err(error) => {
                self.agent
                    .claimed_approved_external_actions
                    .remove(&identity);
                self.complete_approved_external_action(RuntimeApprovedExternalActionOutcome {
                    turn_id: turn_id.to_string(),
                    action_id: action_id.to_string(),
                    result: Err(error),
                    mcp_transport: None,
                })?;
                Ok(None)
            }
        }
    }

    /// Prepares immutable worker inputs while retaining turn state in the actor.
    fn prepare_approved_external_action_dispatch(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> Result<Option<RuntimeApprovedExternalActionDispatch>> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
            .cloned();
        let Some(turn) = turn else {
            self.agent
                .pending_approved_external_actions
                .remove(&(turn_id.to_string(), action_id.to_string()));
            return Ok(None);
        };
        let execution = self
            .agent_turn_executions()
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("approved external execution is unavailable"))?;
        let action = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.iter().find(|action| action.id == action_id))
            .cloned()
            .ok_or_else(|| MezError::invalid_state("approved external action is unavailable"))?;
        if !execution
            .action_results
            .iter()
            .any(|result| result.action_id == action_id && result.status == ActionStatus::Running)
        {
            self.agent
                .pending_approved_external_actions
                .remove(&(turn_id.to_string(), action_id.to_string()));
            return Ok(None);
        }

        if !self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)? {
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: {}",
                    runtime_agent_action_summary(&action)
                        .unwrap_or_else(|| "external action".to_string())
                ),
            )?;
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} type={} external_worker=started",
                action.id,
                action.action_type()
            ),
        )?;

        let mcp = match &action.payload {
            AgentActionPayload::McpCall {
                server,
                tool,
                arguments_json,
            } => {
                if let Some(block) = self.run_configured_pre_action_hooks(
                    HookEvent::PreMcpToolUse,
                    &runtime_pre_mcp_hook_payload(&turn, &action, server, tool, arguments_json),
                )? {
                    let mut result = ActionResult::failed(
                        &turn,
                        &action,
                        ActionStatus::Denied,
                        "hook_blocked",
                        block.message.clone(),
                    )?;
                    result.structured_content_json = Some(block.structured_json());
                    self.complete_approved_external_action(RuntimeApprovedExternalActionOutcome {
                        turn_id: turn_id.to_string(),
                        action_id: action_id.to_string(),
                        result: Ok(result),
                        mcp_transport: None,
                    })?;
                    return Ok(None);
                }
                let request = McpToolCallRequest {
                    server_id: server.clone(),
                    tool_name: tool.clone(),
                    arguments_json: arguments_json.clone(),
                    timeout_ms: None,
                    approval_bypass: true,
                };
                let plan = self.mcp_registry().plan_tool_call(&request)?;
                let environment = std::env::vars().collect();
                let auth_store = self.auth_store().cloned();
                self.append_approved_mcp_action_audit(&turn, &action, "started")?;
                let transport = self
                    .integration
                    .mcp_transports_mut()
                    .take(server)
                    .ok_or_else(|| {
                        MezError::invalid_state(format!(
                            "MCP server `{server}` has no owned runtime transport"
                        ))
                    })?;
                Some(RuntimeApprovedMcpActionDispatch {
                    plan,
                    transport,
                    environment,
                    auth_store,
                })
            }
            _ if network_action_plan(&action).is_some() => {
                let plan = network_action_plan(&action).ok_or_else(|| {
                    MezError::invalid_state("approved network action has no network plan")
                })?;
                if let Some(result) =
                    self.network_action_loop_guard_failure(&turn, &action, &plan.policy_command)?
                {
                    self.complete_approved_external_action(RuntimeApprovedExternalActionOutcome {
                        turn_id: turn_id.to_string(),
                        action_id: action_id.to_string(),
                        result: Ok(result),
                        mcp_transport: None,
                    })?;
                    return Ok(None);
                }
                self.record_network_action_history(&turn.turn_id, &plan.policy_command);
                None
            }
            _ => {
                return Err(MezError::invalid_state(
                    "approved external action is neither network nor MCP",
                ));
            }
        };
        Ok(Some(RuntimeApprovedExternalActionDispatch {
            turn,
            action,
            mcp,
        }))
    }

    /// Applies one approved external worker result through actor-owned state.
    pub(crate) fn complete_approved_external_action(
        &mut self,
        outcome: RuntimeApprovedExternalActionOutcome,
    ) -> Result<bool> {
        if let Some((server_id, transport)) = outcome.mcp_transport {
            self.integration
                .mcp_transports_mut()
                .insert(server_id, transport);
        }
        let identity = (outcome.turn_id.clone(), outcome.action_id.clone());
        self.agent
            .pending_approved_external_actions
            .remove(&identity);
        self.agent
            .claimed_approved_external_actions
            .remove(&identity);
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == outcome.turn_id && turn.state == AgentTurnState::Running)
            .cloned()
        else {
            return Ok(false);
        };
        let mut execution = self
            .agent_turn_executions()
            .get(&outcome.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("approved external execution is unavailable"))?;
        let action = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == outcome.action_id)
            })
            .cloned()
            .ok_or_else(|| MezError::invalid_state("approved external action is unavailable"))?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == outcome.action_id)
            .ok_or_else(|| MezError::invalid_state("approved external result is unavailable"))?;
        let result = match outcome.result {
            Ok(result) => result,
            Err(error) => {
                if let AgentActionPayload::McpCall { server, .. } = &action.payload {
                    let _ = self.mcp_registry_mut().mark_unavailable(
                        server,
                        format!("runtime tool call failed: {}", error.message()),
                        current_unix_seconds(),
                    );
                }
                let error_code = if matches!(&action.payload, AgentActionPayload::McpCall { .. }) {
                    runtime_mcp_error_code(&error)
                } else {
                    runtime_mezzanine_error_code(error.kind())
                };
                ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    error_code,
                    error.message().to_string(),
                )?
            }
        };
        if matches!(&action.payload, AgentActionPayload::McpCall { .. }) {
            self.append_approved_mcp_action_audit(
                &turn,
                &action,
                if result.is_error {
                    "failed"
                } else {
                    "succeeded"
                },
            )?;
            self.run_configured_completed_hooks(
                HookEvent::PostMcpToolUse,
                &runtime_post_mcp_hook_payload(&turn, &action, &result),
            )?;
        } else {
            if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
                self.append_agent_action_result_text_to_terminal_buffer(
                    &turn.pane_id,
                    &action,
                    &result,
                    &result.content_text(),
                )?;
            }
            self.append_agent_network_action_audit(
                &turn,
                &action,
                if result.is_error {
                    "failed"
                } else {
                    "succeeded"
                },
            )?;
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} {} reason=approved_external_action",
                action.id,
                runtime_action_status_name(result.status)
            ),
        )?;
        execution.action_results[result_index] = result;
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(&execution)
        {
            let observed_result = execution.action_results[result_index].clone();
            self.append_action_result_context_if_absent(&turn.turn_id, &observed_result)?;
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let transcript_execution = execution.clone();
            self.persist_runtime_agent_turn_execution_transcript(&turn, &transcript_execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                execution.terminal_state,
                "approved_external_action_settled",
            )?;
            return Ok(true);
        }
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution);
        Ok(true)
    }

    /// Appends one MCP call audit record while execution remains actor-owned.
    fn append_approved_mcp_action_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        outcome: &str,
    ) -> Result<()> {
        let AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } = &action.payload
        else {
            return Ok(());
        };
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        audit_log.append(AuditRecord::mcp_call(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            server,
            tool,
            format!("{}:{}", turn.turn_id, action.id),
            arguments_json,
            outcome,
        ))?;
        Ok(())
    }

    /// Runs the execute running mcp actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(super) fn execute_running_mcp_actions_for_turn(
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
                || execution.action_results[index].action_type != "mcp_call"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "MCP call".to_string())
                    ),
                )?;
            }
            let permission_policy = self.permission_policy_for_turn(turn);
            let auto_allowed = permission_policy.approval_policy
                == mez_agent::ApprovalPolicy::AutoAllow
                && mez_agent::action_supports_auto_allow(
                    &action,
                    mez_agent::ActionPlanningInput::default(),
                );
            let policy_allowed = permission_policy.approval_policy.bypasses_prompts();
            execution.action_results[index] =
                self.execute_mcp_action_for_turn(turn, &action, auto_allowed || policy_allowed)?;
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
                .filter(|result| result.action_type == "mcp_call")
            {
                self.append_action_result_context_if_absent(&turn.turn_id, result)?;
            }
        }
        Ok(executed)
    }

    /// Runs the execute running mcp actions for turn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_running_mcp_actions_for_turn_async(
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
                || execution.action_results[index].action_type != "mcp_call"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "MCP call".to_string())
                    ),
                )?;
            }
            let permission_policy = self.permission_policy_for_turn(turn);
            let auto_allowed = permission_policy.approval_policy
                == mez_agent::ApprovalPolicy::AutoAllow
                && mez_agent::action_supports_auto_allow(
                    &action,
                    mez_agent::ActionPlanningInput::default(),
                );
            let policy_allowed = permission_policy.approval_policy.bypasses_prompts();
            execution.action_results[index] = self
                .execute_mcp_action_for_turn_async(turn, &action, auto_allowed || policy_allowed)
                .await?;
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
                .filter(|result| result.action_type == "mcp_call")
            {
                self.append_action_result_context_if_absent(&turn.turn_id, result)?;
            }
        }
        Ok(executed)
    }

    pub(super) async fn execute_running_network_actions_for_turn_async(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let can_execute_running_actions = execution.terminal_state == AgentTurnState::Running;
        let mut executed = 0usize;
        let mut preexecuted = 0usize;
        for index in 0..execution.action_results.len() {
            if !matches!(
                execution.action_results[index].action_type,
                "web_search" | "fetch_url"
            ) {
                continue;
            }
            if execution.action_results[index].status != ActionStatus::Running {
                if matches!(
                    execution.action_results[index].status,
                    ActionStatus::Succeeded | ActionStatus::Failed
                ) {
                    let action = batch
                        .actions
                        .iter()
                        .find(|action| action.id == execution.action_results[index].action_id)
                        .cloned()
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "settled network result does not match an action",
                            )
                        })?;
                    self.record_preexecuted_network_action_result(
                        turn,
                        &action,
                        &execution.action_results[index],
                    )?;
                    preexecuted = preexecuted.saturating_add(1);
                }
                continue;
            }
            if !can_execute_running_actions {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running network result does not match an action")
                })?;
            let Some(plan) = network_action_plan(&action) else {
                continue;
            };
            let request_key = plan.policy_command.clone();
            if let Some(result) =
                self.network_action_loop_guard_failure(turn, &action, &request_key)?
            {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} {} reason=network_action_loop_guard",
                        action.id,
                        runtime_action_status_name(result.status)
                    ),
                )?;
                execution.action_results[index] = result;
                continue;
            }
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "network action".to_string())
                    ),
                )?;
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} type={} network_executor=started",
                    action.id,
                    action.action_type()
                ),
            )?;
            let transport = ReqwestProviderHttpTransport;
            self.record_network_action_history(&turn.turn_id, &request_key);
            let result =
                execute_network_action_with_transport_async(turn, &action, &transport).await?;
            if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
                self.append_agent_action_result_text_to_terminal_buffer(
                    &turn.pane_id,
                    &action,
                    &result,
                    &result.content_text(),
                )?;
            }
            let outcome = if result.is_error {
                "failed"
            } else {
                "succeeded"
            };
            self.append_agent_network_action_audit(turn, &action, outcome)?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} {} reason=runtime_network_action",
                    action.id,
                    runtime_action_status_name(result.status)
                ),
            )?;
            execution.action_results[index] = result;
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
                .filter(|result| matches!(result.action_type, "web_search" | "fetch_url"))
            {
                self.append_action_result_context_if_absent(&turn.turn_id, result)?;
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed.saturating_add(preexecuted))
    }

    /// Records presentation and audit side effects for network actions that
    /// were executed by the async provider worker before actor ingress.
    fn record_preexecuted_network_action_result(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        result: &ActionResult,
    ) -> Result<()> {
        let Some(plan) = network_action_plan(action) else {
            return Ok(());
        };
        if !self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)? {
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: {}",
                    runtime_agent_action_summary(action)
                        .unwrap_or_else(|| "network action".to_string())
                ),
            )?;
        }
        self.record_network_action_history(&turn.turn_id, &plan.policy_command);
        if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
            self.append_agent_action_result_text_to_terminal_buffer(
                &turn.pane_id,
                action,
                result,
                &result.content_text(),
            )?;
        }
        let outcome = if result.is_error {
            "failed"
        } else {
            "succeeded"
        };
        self.append_agent_network_action_audit(turn, action, outcome)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} {} reason=provider_worker_network_action",
                action.id,
                runtime_action_status_name(result.status)
            ),
        )?;
        Ok(())
    }

    /// Runs the execute mcp action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(super) fn execute_mcp_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        approved: bool,
    ) -> Result<ActionResult> {
        let AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "MCP execution requires an mcp_call action",
            ));
        };
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::PreMcpToolUse,
            &runtime_pre_mcp_hook_payload(turn, action, server, tool, arguments_json),
        )? {
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Denied,
                "hook_blocked",
                block.message.clone(),
            )?;
            result.structured_content_json = Some(block.structured_json());
            return Ok(result);
        }
        let permission_policy = self.permission_policy_for_turn(turn);
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry().plan_tool_call(&request)?;
        if plan.approval_required && !approved && !permission_policy.approval_bypass() {
            return Ok(ActionResult::blocked(
                turn,
                action,
                vec!["approval required before executing MCP tool call".to_string()],
                format!(
                    r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                    json_escape(&action.id),
                    json_escape(server),
                    json_escape(tool)
                ),
            ));
        }
        let call_id = format!("{}:{}", turn.turn_id, action.id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let execution_request = McpExecutionRequest::from(&plan);
        let audit_log = self.persistence.audit_log_mut();
        let (transports, auth_store) = self.integration.mcp_execution_bindings();
        let mut executor = RuntimeMcpActionExecutor {
            transports,
            audit_log,
            environment,
            auth_store,
            session_id: self.session.id.to_string(),
            actor: AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            call_id,
            plan: &plan,
        };
        let result = match execute_mcp_action_through_runtime(
            turn,
            action,
            &execution_request,
            &mut executor,
        ) {
            Ok(result) => result,
            Err(error) => {
                let _ = self.mcp_registry_mut().mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
                    current_unix_seconds(),
                );
                ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mcp_error_code(&error),
                    error.message().to_string(),
                )?
            }
        };
        self.run_configured_completed_hooks(
            HookEvent::PostMcpToolUse,
            &runtime_post_mcp_hook_payload(turn, action, &result),
        )?;
        Ok(result)
    }

    /// Runs the execute mcp action for turn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_mcp_action_for_turn_async(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        approved: bool,
    ) -> Result<ActionResult> {
        let AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "MCP execution requires an mcp_call action",
            ));
        };
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::PreMcpToolUse,
            &runtime_pre_mcp_hook_payload(turn, action, server, tool, arguments_json),
        )? {
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Denied,
                "hook_blocked",
                block.message.clone(),
            )?;
            result.structured_content_json = Some(block.structured_json());
            return Ok(result);
        }
        let permission_policy = self.permission_policy_for_turn(turn);
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry().plan_tool_call(&request)?;
        if plan.approval_required && !approved && !permission_policy.approval_bypass() {
            return Ok(ActionResult::blocked(
                turn,
                action,
                vec!["approval required before executing MCP tool call".to_string()],
                format!(
                    r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                    json_escape(&action.id),
                    json_escape(server),
                    json_escape(tool)
                ),
            ));
        }
        let call_id = format!("{}:{}", turn.turn_id, action.id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let execution_request = McpExecutionRequest::from(&plan);
        let audit_log = self.persistence.audit_log_mut();
        let (transports, auth_store) = self.integration.mcp_execution_bindings();
        let mut executor = RuntimeMcpActionExecutor {
            transports,
            audit_log,
            environment,
            auth_store,
            session_id: self.session.id.to_string(),
            actor: AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            call_id,
            plan: &plan,
        };
        let result = match execute_mcp_action_through_runtime_async(
            turn,
            action,
            &execution_request,
            &mut executor,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = self.mcp_registry_mut().mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
                    current_unix_seconds(),
                );
                ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mcp_error_code(&error),
                    error.message().to_string(),
                )?
            }
        };
        self.run_configured_completed_hooks(
            HookEvent::PostMcpToolUse,
            &runtime_post_mcp_hook_payload(turn, action, &result),
        )?;
        Ok(result)
    }
}
