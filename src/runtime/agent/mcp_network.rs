//! Runtime agent MCP and network action execution helpers.
//!
//! This module owns runtime-executed external integration actions. Keeping MCP
//! and network execution together isolates approval, loop-guard, hook, audit,
//! and provider-continuation context handling from the main runtime agent
//! facade.

use super::*;
use crate::agent::McpExecutionRequest;

impl RuntimeSessionService {
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
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(&action);
            let policy_allowed =
                permission_policy.approval_policy == crate::permissions::ApprovalPolicy::FullAccess;
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
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(&action);
            let policy_allowed =
                permission_policy.approval_policy == crate::permissions::ApprovalPolicy::FullAccess;
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
            let Some(plan) = network_action_plan(&action)? else {
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
            self.pending_agent_provider_tasks
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
        let Some(plan) = network_action_plan(action)? else {
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

    /// Executes one approved runtime-owned network action from a legacy
    /// synchronous approval path.
    ///
    /// The control dispatcher is still synchronous, so the network future runs
    /// on a short-lived helper thread with its own Tokio runtime instead of
    /// trying to nest a runtime inside the actor thread.
    pub(super) fn execute_network_action_for_turn_blocking(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let Some(plan) = network_action_plan(action)? else {
            return Err(MezError::invalid_args(
                "network action execution requires a network-backed action",
            ));
        };
        let request_key = plan.policy_command;
        if let Some(result) = self.network_action_loop_guard_failure(turn, action, &request_key)? {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} {} reason=network_action_loop_guard",
                    action.id,
                    runtime_action_status_name(result.status)
                ),
            )?;
            return Ok(result);
        }
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
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} type={} network_executor=started",
                action.id,
                action.action_type()
            ),
        )?;
        let turn_for_thread = turn.clone();
        let action_for_thread = action.clone();
        self.record_network_action_history(&turn.turn_id, &request_key);
        let result = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    MezError::invalid_state(format!("network action runtime setup failed: {error}"))
                })?;
            let transport = ReqwestProviderHttpTransport;
            runtime.block_on(execute_network_action_with_transport_async(
                &turn_for_thread,
                &action_for_thread,
                &transport,
            ))
        })
        .join()
        .map_err(|_| MezError::invalid_state("network action worker panicked"))??;
        if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
            self.append_agent_action_result_text_to_terminal_buffer(
                &turn.pane_id,
                action,
                &result,
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
                "action {} {} reason=runtime_network_action",
                action.id,
                runtime_action_status_name(result.status)
            ),
        )?;
        Ok(result)
    }

    /// Runs the execute mcp action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
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
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: self.permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry.plan_tool_call(&request)?;
        if plan.approval_required && !approved && !self.permission_policy.approval_bypass() {
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
        let audit_log = self.audit_log.as_mut();
        let mut executor = RuntimeMcpActionExecutor {
            transports: &mut self.mcp_transports,
            audit_log,
            environment,
            auth_store: self.auth_store.as_ref(),
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
                let _ = self.mcp_registry.mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
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
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: self.permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry.plan_tool_call(&request)?;
        if plan.approval_required && !approved && !self.permission_policy.approval_bypass() {
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
        let audit_log = self.audit_log.as_mut();
        let mut executor = RuntimeMcpActionExecutor {
            transports: &mut self.mcp_transports,
            audit_log,
            environment,
            auth_store: self.auth_store.as_ref(),
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
                let _ = self.mcp_registry.mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
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
