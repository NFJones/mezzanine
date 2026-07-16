//! Runtime agent approval and permission-resume helpers.
//!
//! This module owns blocked-action approval queueing, permission-change
//! reconciliation, pre-approval hooks, and approved/denied action resumption.
//! Keeping these policy transitions together makes the runtime agent facade
//! less coupled to approval-store internals.

use super::*;

impl RuntimeSessionService {
    /// Runs the apply permission request hooks for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn apply_permission_request_hooks_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut blocked_by_hooks = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Blocked {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("blocked result does not match an action")
                })?;
            if let Some(block) = self.run_configured_pre_action_hooks(
                HookEvent::PermissionRequest,
                &runtime_permission_request_hook_payload(
                    turn,
                    action,
                    &execution.action_results[index],
                ),
            )? {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Denied,
                    "hook_blocked",
                    block.message.clone(),
                )?;
                result.structured_content_json = Some(block.structured_json());
                execution.action_results[index] = result;
                blocked_by_hooks = blocked_by_hooks.saturating_add(1);
            }
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(blocked_by_hooks)
    }

    /// Runs the queue blocked approvals for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn queue_blocked_approvals_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<Vec<String>> {
        let mut approval_ids = Vec::new();
        let subagent_scope = self.subagent_scope_declaration_for_turn(turn);
        for result in execution
            .action_results
            .iter()
            .filter(|result| result.status == ActionStatus::Blocked)
        {
            let approval_id = self.queue_blocked_approval(runtime_blocked_approval_request(
                turn,
                result,
                subagent_scope.as_ref(),
            ))?;
            self.blocked_agent_approval_refs.insert(
                approval_id.clone(),
                BlockedAgentApprovalRef {
                    turn_id: turn.turn_id.clone(),
                    action_id: result.action_id.clone(),
                },
            );
            if let Some(approval) = self.blocked_approvals.get(&approval_id).cloned() {
                let log_line = runtime_agent_pending_approval_log_line(&approval);
                self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &log_line)?;
            }
            approval_ids.push(approval_id);
        }
        if approval_ids.is_empty() {
            return Err(MezError::invalid_state(
                "blocked agent turn did not include blocked action results",
            ));
        }
        Ok(approval_ids)
    }

    /// Reconciles pending blocked agent approvals after permission policy changes.
    ///
    /// # Parameters
    /// - `caller_client_id`: Client that caused the policy update, when known.
    /// - `previous`: Permission policy before the update.
    /// - `source`: Human-readable source of the policy update for lifecycle events.
    pub(in crate::runtime) fn reconcile_pending_agent_approvals_after_permission_change(
        &mut self,
        caller_client_id: Option<&mez_core::ids::ClientId>,
        previous: &PermissionPolicy,
        source: &str,
    ) -> Result<usize> {
        if previous.preset == self.permission_policy.preset
            && previous.approval_policy == self.permission_policy.approval_policy
            && previous.approval_bypass() == self.permission_policy.approval_bypass()
            && previous.rules() == self.permission_policy.rules()
        {
            return Ok(0);
        }
        let pending_ids = self
            .blocked_approvals
            .pending()
            .into_iter()
            .map(|approval| approval.id.clone())
            .collect::<Vec<_>>();
        let mut resumed = 0usize;
        for approval_id in pending_ids {
            let Some(approval) = self.blocked_approvals.get(&approval_id).cloned() else {
                continue;
            };
            if !self
                .pending_agent_approval_is_satisfied_by_current_policy(&approval_id, &approval)?
            {
                continue;
            }
            let controller = caller_client_id
                .cloned()
                .or_else(|| self.session.primary_client_id().cloned())
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "policy-resolved blocked approval requires an attached primary client",
                    )
                })?;
            let decided = self
                .blocked_approvals
                .decide_with_client_at(
                    &approval_id,
                    mez_agent::permissions::ApprovalDecision::Approve,
                    None,
                    Some(controller.to_string()),
                    current_unix_seconds(),
                )?
                .clone();
            let count =
                self.resume_approved_blocked_agent_action(&approval_id, &decided, &controller)?;
            let count = count.unwrap_or(0);
            resumed = resumed.saturating_add(count);
            self.append_primary_lifecycle_event(
                EventKind::ApprovalChanged,
                format!(
                    r#"{{"approval_id":"{}","decision":"approve","state":"decided","source":"{}","agent_actions_resumed":{}}}"#,
                    json_escape(&approval_id),
                    json_escape(source),
                    count
                ),
            )?;
        }
        Ok(resumed)
    }

    /// Reports whether a pending blocked approval is now satisfied by policy.
    ///
    /// # Parameters
    /// - `approval_id`: Identifier of the pending blocked approval.
    /// - `approval`: Pending blocked approval metadata.
    fn pending_agent_approval_is_satisfied_by_current_policy(
        &self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
    ) -> Result<bool> {
        if approval.state != mez_agent::permissions::BlockedApprovalState::Pending {
            return Ok(false);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id) else {
            return Ok(false);
        };
        let execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let permission_policy = self.permission_policy_for_turn(turn);
        if permission_policy.approval_bypass()
            || permission_policy.approval_policy == mez_agent::ApprovalPolicy::FullAccess
        {
            return Ok(true);
        }
        match &action.payload {
            _ if local_action_plan(action)?.is_some() => {
                let Some(plan) = local_action_plan(action)? else {
                    return Ok(false);
                };
                let subagent_scope = self.subagent_scope_declaration_for_turn(turn);
                if let Some(scope) = subagent_scope.as_ref()
                    && let Some(_message) =
                        mez_agent::SubagentScopeEnforcement::shell_command_violation(
                            &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                            scope,
                            &plan.policy_command,
                        )
                        .map_err(MezError::invalid_args)?
                {
                    return Ok(false);
                }
                let path_scopes = if subagent_scope.is_some() {
                    None
                } else {
                    self.path_scopes_for_pane(&turn.pane_id)
                };
                Ok(matches!(
                    permission_policy.evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        &self.session_approvals,
                        path_scopes.as_ref(),
                    ),
                    RuleDecision::Allow
                ) || (permission_policy.approval_policy == mez_agent::ApprovalPolicy::AutoAllow
                    && mez_agent::action_supports_auto_allow(
                        action,
                        mez_agent::ActionPlanningInput {
                            local_plan: Some(&plan),
                            ..mez_agent::ActionPlanningInput::default()
                        },
                    )))
            }
            _ if network_action_plan(action).is_some() => {
                let Some(plan) = network_action_plan(action) else {
                    return Ok(false);
                };
                Ok(matches!(
                    permission_policy.evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        &self.session_approvals,
                        None,
                    ),
                    RuleDecision::Allow
                ) || (permission_policy.approval_policy == mez_agent::ApprovalPolicy::AutoAllow
                    && mez_agent::action_supports_auto_allow(
                        action,
                        mez_agent::ActionPlanningInput {
                            network_plan: Some(&plan),
                            ..mez_agent::ActionPlanningInput::default()
                        },
                    )))
            }
            AgentActionPayload::McpCall { .. } => Ok(permission_policy.approval_policy
                == mez_agent::ApprovalPolicy::AutoAllow
                && mez_agent::action_supports_auto_allow(
                    action,
                    mez_agent::ActionPlanningInput::default(),
                )),
            AgentActionPayload::ConfigChange { .. } => Ok(permission_policy.approval_policy
                == mez_agent::ApprovalPolicy::AutoAllow
                && mez_agent::action_supports_auto_allow(
                    action,
                    mez_agent::ActionPlanningInput::default(),
                )),
            _ => Ok(false),
        }
    }

    /// Runs the resume approved blocked agent action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn resume_approved_blocked_agent_action(
        &mut self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<Option<usize>> {
        if !matches!(
            approval.action_kind.as_str(),
            "shell_command"
                | "apply_patch"
                | "mcp_call"
                | "config_change"
                | "web_search"
                | "fetch_url"
        ) {
            return Ok(None);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id).cloned() else {
            return Ok(None);
        };
        let mut execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an action result")
            })?;
        if execution.action_results[result_index].status != ActionStatus::Blocked {
            return Ok(None);
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "approval {} decision=approved action={} kind={}",
                approval_id, action.id, approval.action_kind
            ),
        )?;
        let _ = self.agent_scheduler.resume_blocked(&turn.turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "scheduler blocked -> running reason=approval_approved",
        )?;
        if turn.state == AgentTurnState::Blocked {
            self.agent_turn_ledger.resume_blocked_turn(&turn.turn_id)?;
            self.append_agent_trace_turn_transition(
                &turn,
                AgentTurnState::Blocked,
                AgentTurnState::Running,
                "approval_approved",
            )?;
        }
        match &action.payload {
            _ if local_action_plan(&action)?.is_some() => {
                let Some(plan) = local_action_plan(&action)? else {
                    return Err(MezError::invalid_state(
                        "approved shell-backed action has no local action plan",
                    ));
                };
                let command = plan.command.as_str();
                let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
                let permission_policy = self.permission_policy_for_turn(&turn);
                if let Some(scope) = subagent_scope.as_ref()
                    && let Some(message) = mez_agent::subagent_action_scope_violation(
                        &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                        scope,
                        &action,
                        &plan.policy_command,
                    )
                    .map_err(MezError::invalid_args)?
                {
                    return Err(MezError::forbidden(message));
                }
                let path_scopes = if subagent_scope.is_some() {
                    None
                } else {
                    self.path_scopes_for_pane(&turn.pane_id)
                };
                match permission_policy.evaluate_shell_command_with_approvals_scoped(
                    &plan.policy_command,
                    &self.session_approvals,
                    path_scopes.as_ref(),
                ) {
                    RuleDecision::Allow => {}
                    RuleDecision::Prompt
                        if approval.state
                            == mez_agent::permissions::BlockedApprovalState::Approved
                            && approval.decision
                                == Some(mez_agent::permissions::ApprovalDecision::Approve) => {}
                    RuleDecision::Prompt => {
                        return Err(MezError::conflict(
                            "approved shell action still requires approval",
                        ));
                    }
                    RuleDecision::Forbid => {
                        return Err(MezError::forbidden(
                            "approved shell action is forbidden by current permission policy",
                        ));
                    }
                }
                execution.action_results[result_index] = ActionResult::running(
                    &turn,
                    &action,
                    vec!["approved local action accepted for local dispatch".to_string()],
                    Some(mez_agent::shell_action_structured_content_json(
                        &action,
                        &plan,
                        Some("pending_local_dispatch"),
                        false,
                        serde_json::json!({
                            "state": "approved",
                            "kind": action.action_type(),
                            "action_id": action.id.as_str(),
                            "command": runtime_agent_context_command(&action, command)
                        }),
                        &[],
                        serde_json::json!({"state":"pending_dispatch"}),
                    )),
                );
                execution.terminal_state = AgentTurnState::Running;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} blocked -> running reason=approval_approved",
                        action.id
                    ),
                )?;
                self.dispatch_running_shell_actions_to_panes(&turn, &mut execution)?;
            }
            _ if network_action_plan(&action).is_some() => {
                let Some(plan) = network_action_plan(&action) else {
                    return Err(MezError::invalid_state(
                        "approved network action has no network action plan",
                    ));
                };
                let permission_policy = self.permission_policy_for_turn(&turn);
                match permission_policy.evaluate_shell_command_with_approvals_scoped(
                    &plan.policy_command,
                    &self.session_approvals,
                    None,
                ) {
                    RuleDecision::Allow => {}
                    RuleDecision::Prompt
                        if approval.state
                            == mez_agent::permissions::BlockedApprovalState::Approved
                            && approval.decision
                                == Some(mez_agent::permissions::ApprovalDecision::Approve) => {}
                    RuleDecision::Prompt => {
                        return Err(MezError::conflict(
                            "approved network action still requires approval",
                        ));
                    }
                    RuleDecision::Forbid => {
                        return Err(MezError::forbidden(
                            "approved network action is forbidden by current permission policy",
                        ));
                    }
                }
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} blocked -> running reason=approval_approved_network",
                        action.id
                    ),
                )?;
                execution.action_results[result_index] =
                    self.execute_network_action_for_turn_blocking(&turn, &action)?;
            }
            AgentActionPayload::McpCall { .. } => {
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
                execution.action_results[result_index] =
                    self.execute_mcp_action_for_turn(&turn, &action, true)?;
            }
            AgentActionPayload::ConfigChange { .. } => {
                if !self
                    .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
                {
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: {}",
                            runtime_agent_action_summary(&action)
                                .unwrap_or_else(|| "config change".to_string())
                        ),
                    )?;
                }
                let result = self.execute_config_change_action_for_turn(
                    &turn,
                    &action,
                    caller_client_id,
                    "approved",
                )?;
                if result.is_error {
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: configuration change failed: {}",
                            result.content_text()
                        ),
                    )?;
                }
                execution.action_results[result_index] = result;
            }
            _ => return Ok(None),
        }
        self.blocked_agent_approval_refs.remove(approval_id);
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "approval_resume recalculated terminal_state={}",
                runtime_agent_turn_state_name(execution.terminal_state)
            ),
        )?;
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(&execution)
        {
            let observed_result = execution.action_results[result_index].clone();
            self.agent_turn_contexts
                .get_mut(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running agent turn context is unavailable")
                })?
                .blocks
                .push(ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: format!("action result {}", observed_result.action_id),
                    content: action_result_context_content(&observed_result),
                });
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "provider_task queued reason=approval_resume_ready_for_provider_continuation",
            )?;
        }
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let transcript_execution = execution.clone();
            let _ =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &transcript_execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                execution.terminal_state,
                "approval_resume_settled",
            )?;
            return Ok(Some(1));
        }
        self.agent_turn_executions
            .insert(turn.turn_id.clone(), execution);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "execution stored reason=approval_resume",
        )?;
        Ok(Some(1))
    }

    /// Runs the settle decided blocked agent action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn settle_decided_blocked_agent_action(
        &mut self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
    ) -> Result<Option<usize>> {
        let Some(decision) = approval.decision else {
            return Ok(None);
        };
        if !matches!(
            decision,
            mez_agent::permissions::ApprovalDecision::Disapprove
                | mez_agent::permissions::ApprovalDecision::Redirect
        ) {
            return Ok(None);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id).cloned() else {
            return Ok(None);
        };
        let mut execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an action result")
            })?;
        if execution.action_results[result_index].status != ActionStatus::Blocked {
            return Ok(None);
        }

        match decision {
            mez_agent::permissions::ApprovalDecision::Disapprove => {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "approval {} decision=disapprove action={} kind={}",
                        approval_id, action.id, approval.action_kind
                    ),
                )?;
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Denied,
                    "approval_disapproved",
                    format!(
                        "user denied {} {}",
                        approval.action_kind,
                        runtime_agent_terminal_preview(&approval.action_summary)
                    ),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"approval":{{"state":"disapproved","kind":"{}","approval_id":"{}","action_id":"{}"}}}}"#,
                    json_escape(&approval.action_kind),
                    json_escape(approval_id),
                    json_escape(&action.id)
                ));
                execution.action_results[result_index] = result;
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                let transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                let _ = self.agent_scheduler.cancel(&turn.turn_id);
                self.blocked_agent_approval_refs.remove(approval_id);
                self.append_agent_error_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: approval {} denied: {} {}",
                        approval_id,
                        approval.action_kind,
                        runtime_agent_terminal_preview(&approval.action_summary)
                    ),
                )?;
                if self
                    .agent_shell_store
                    .get(&turn.pane_id)
                    .and_then(|session| session.running_turn_id.as_deref())
                    == Some(turn.turn_id.as_str())
                {
                    self.finish_agent_turn(&turn.pane_id, &turn.turn_id, AgentTurnState::Failed)?;
                } else {
                    self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Failed)?;
                }
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","approval_id":"{}","decision":"disapprove","transcript_entries":{}}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&turn.turn_id),
                        json_escape(approval_id),
                        transcript_entries
                    ),
                )?;
                Ok(Some(1))
            }
            mez_agent::permissions::ApprovalDecision::Redirect => {
                let instruction = approval.redirect_instruction.as_deref().ok_or_else(|| {
                    MezError::invalid_state("redirect approval has no instruction")
                })?;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "approval {} decision=redirect action={} kind={}",
                        approval_id, action.id, approval.action_kind
                    ),
                )?;
                let _ = self.agent_scheduler.resume_blocked(&turn.turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "scheduler blocked -> running reason=approval_redirected",
                )?;
                if turn.state == AgentTurnState::Blocked {
                    self.agent_turn_ledger.resume_blocked_turn(&turn.turn_id)?;
                    self.append_agent_trace_turn_transition(
                        &turn,
                        AgentTurnState::Blocked,
                        AgentTurnState::Running,
                        "approval_redirected",
                    )?;
                }
                execution.action_results[result_index] = ActionResult::succeeded(
                    &turn,
                    &action,
                    vec![format!("user redirected action: {instruction}")],
                    Some(format!(
                        r#"{{"approval":{{"state":"redirected","kind":"{}","approval_id":"{}","action_id":"{}"}},"redirect_instruction":"{}"}}"#,
                        json_escape(&approval.action_kind),
                        json_escape(approval_id),
                        json_escape(&action.id),
                        json_escape(instruction)
                    )),
                );
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                self.blocked_agent_approval_refs.remove(approval_id);
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: approval {} redirected: {}",
                        approval_id,
                        runtime_agent_terminal_preview(instruction)
                    ),
                )?;
                if execution.terminal_state == AgentTurnState::Running
                    && runtime_execution_ready_for_provider_continuation(&execution)
                {
                    let observed_result = execution.action_results[result_index].clone();
                    self.agent_turn_contexts
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("running agent turn context is unavailable")
                        })?
                        .blocks
                        .push(ContextBlock {
                            source: ContextSourceKind::ActionResult,
                            label: format!("action result {}", observed_result.action_id),
                            content: action_result_context_content(&observed_result),
                        });
                    self.pending_agent_provider_tasks
                        .insert(turn.turn_id.clone());
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task queued reason=approval_redirect_ready_for_provider_continuation",
                    )?;
                }
                if matches!(
                    execution.terminal_state,
                    AgentTurnState::Completed
                        | AgentTurnState::Failed
                        | AgentTurnState::Interrupted
                ) {
                    let transcript_execution = execution.clone();
                    let _ = self.persist_runtime_agent_turn_execution_transcript(
                        &turn,
                        &transcript_execution,
                    )?;
                    self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                    self.complete_running_agent_turn_and_start_ready(
                        &turn,
                        execution.terminal_state,
                        "approval_redirect_settled",
                    )?;
                    return Ok(Some(1));
                }
                self.agent_turn_executions
                    .insert(turn.turn_id.clone(), execution);
                Ok(Some(1))
            }
            mez_agent::permissions::ApprovalDecision::Approve => Ok(None),
        }
    }
}
