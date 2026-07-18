//! Macro-managed message routing into persistent child turns.

use super::super::{
    ActionResult, ActionStatus, AgentAction, AgentTurnRecord, AgentTurnState,
    JoinedSubagentDependency, MezError, Result, RuntimeSessionService, ScheduledWork,
    current_unix_seconds, json_escape, runtime_mezzanine_error_code, runtime_pane_by_id,
};
use super::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, RuntimeAgentLoopCompletion,
    ScheduledWorkKind, execute_agent_shell_command_with_context, macro_message_recipient_agent_id,
};

impl RuntimeSessionService {
    /// Starts a normal child agent-shell turn for a macro step message.
    ///
    /// The bridge is intentionally limited to macro-managed child agents and
    /// text payloads. When it applies, the message payload is queued through the
    /// same scheduler and provider path as an ordinary prompt submitted in the
    /// child subagent shell, which preserves slash-command behavior such as
    /// `/loop` while keeping the parent action result tied to the child task
    /// result route.
    ///
    /// # Parameters
    /// - `parent_turn`: Parent turn that emitted the `send_message` action.
    /// - `action`: Parent action whose result should wait for the child step.
    /// - `recipient`: Model-supplied recipient string from the action.
    /// - `content_type`: Canonical MMP content type for the payload.
    /// - `payload`: Text prompt to queue in the child agent shell.
    pub(crate) fn queue_macro_managed_message_step(
        &mut self,
        parent_turn: &AgentTurnRecord,
        action: &AgentAction,
        recipient: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<Option<ActionResult>> {
        if content_type != "text/plain; charset=utf-8" {
            return Ok(None);
        }
        let Some(child_agent_id) = macro_message_recipient_agent_id(recipient) else {
            return Ok(None);
        };
        let Some(macro_owner) = self
            .agent
            .macro_managed_subagent_agents
            .get(child_agent_id.as_str())
        else {
            return Ok(None);
        };
        let Some(child_lineage) = self.subagent_lineage(child_agent_id.as_str()) else {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent lineage is missing",
            )?));
        };
        let child_parent_agent_id = child_lineage.parent_agent_id.clone();
        let child_display_name = child_lineage.display_name.clone();
        if child_parent_agent_id != parent_turn.agent_id
            || macro_owner.parent_agent_id != parent_turn.agent_id
            || macro_owner.parent_turn_id != parent_turn.turn_id
        {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent step recipient does not belong to this macro run",
            )?));
        }
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("macro-managed child agent id is invalid"))?;
        runtime_pane_by_id(&self.session, child_pane_id)?;
        // --- Idempotency guard: retried parent actions reuse the original
        // step result instead of creating another child turn. Check this before
        // the generic in-flight guard so retries of the same accepted action
        // remain safe while the child turn is still running. ---
        if let Some(existing) = self
            .agent
            .joined_subagent_dependencies
            .values()
            .find(|dep| {
                dep.parent_turn_id == parent_turn.turn_id && dep.parent_action_id == action.id
            })
        {
            if self.joined_subagent_dependency_has_live_child(existing) {
                // Still in progress — return the same running result.
                return Ok(Some(ActionResult::running(
                    parent_turn,
                    action,
                    vec![format!(
                        "macro step already in progress for {child_agent_id}; waiting for subagent result"
                    )],
                    Some(format!(
                        r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                        json_escape(recipient),
                        json_escape(&child_agent_id),
                        json_escape(&existing.child_turn_id)
                    )),
                )));
            }
            // Child turn already reached a terminal state — return idempotent
            // terminal result.
            let child_state = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|t| t.turn_id == existing.child_turn_id)
                .map(|t| t.state);
            match child_state {
                Some(AgentTurnState::Completed) => {
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already completed by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"completed","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
                Some(AgentTurnState::Failed) | Some(AgentTurnState::Interrupted) => {
                    return Ok(Some(ActionResult::failed(
                        parent_turn,
                        action,
                        ActionStatus::Failed,
                        "macro_step_failed",
                        "macro step previously failed; cannot retry",
                    )?));
                }
                _ => {
                    // Other terminal state — treat as resolved.
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already resolved by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"resolved","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
            }
        }
        // --- Ordering guard: reject if a different macro step is already
        // in-flight for this parent turn + child agent pair. ---
        let macro_step_in_flight = self.agent.joined_subagent_dependencies.values().any(|dep| {
            dep.parent_turn_id == parent_turn.turn_id
                && dep.child_agent_id == child_agent_id
                && self.joined_subagent_dependency_has_live_child(dep)
        }) || self.agent.agent_loops_by_id.values().any(|state| {
            state.completion.as_ref().is_some_and(|completion| {
                completion.parent_turn_id == parent_turn.turn_id
                    && completion.child_agent_id == child_agent_id
            })
        });
        if macro_step_in_flight {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_step_ordering",
                "a macro step is already in flight for this subagent; wait for it to complete before sending the next step",
            )?));
        }
        let mcp_summary = self.mcp_registry().agent_shell_summary();
        let permission_summary = self.permission_policy().agent_shell_summary();
        let parsed_command = execute_agent_shell_command_with_context(
            self.agent_shell_store_mut(),
            child_pane_id,
            payload,
            AgentShellRuntimeContext {
                mcp_summary: Some(&mcp_summary),
                permission_summary: Some(&permission_summary),
            },
        );
        if matches!(
            parsed_command.as_ref(),
            Ok(Some(AgentShellCommandOutcome::RequiresRuntime { command, .. })) if command == "loop"
        ) {
            let loop_outcome = match self.execute_agent_shell_loop_command(child_pane_id, payload) {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Ok(Some(ActionResult::failed(
                        parent_turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?));
                }
            };
            let child_turn_id = self
                .agent
                .agent_loop_turns
                .iter()
                .find(|(_, loop_turn)| loop_turn.pane_id == child_pane_id)
                .map(|(turn_id, _)| turn_id.clone())
                .ok_or_else(|| MezError::invalid_state("macro loop did not create a work turn"))?;
            self.agent
                .subagent_task_routes
                .insert(child_turn_id.clone(), parent_turn.agent_id.clone());
            let loop_state = self
                .agent_loop_state_mut(child_pane_id)
                .ok_or_else(|| MezError::invalid_state("macro loop controller is unavailable"))?;
            if loop_state.completion.is_some() {
                return Err(MezError::invalid_state(
                    "macro loop controller already has a parent completion",
                ));
            }
            loop_state.completion = Some(RuntimeAgentLoopCompletion {
                parent_turn_id: parent_turn.turn_id.clone(),
                parent_action_id: action.id.clone(),
                child_turn_id: child_turn_id.clone(),
                child_agent_id: child_agent_id.to_string(),
                child_display_name: Some(child_display_name.clone()),
            });
            return Ok(Some(ActionResult::running(
                parent_turn,
                action,
                vec![format!(
                    "macro loop step delivered to {child_agent_id}; waiting for loop result"
                )],
                Some(format!(
                    r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","command":"loop","outcome":"{}","error":null}}"#,
                    json_escape(recipient),
                    json_escape(&child_agent_id),
                    json_escape(&child_turn_id),
                    json_escape(&format!("{loop_outcome:?}"))
                )),
            )));
        }
        let context = self.agent_context_for_pane_prompt(child_pane_id, payload, 100)?;
        let context = self.apply_agent_shell_preference_context(child_pane_id, context)?;
        let turn_id = self.next_agent_turn_id();
        let created_at_unix_seconds = current_unix_seconds();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(child_pane_id, &child_agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: child_pane_id.to_string(),
            trigger: mez_agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: created_at_unix_seconds,
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: Some(parent_turn.turn_id.clone()),
            cooperation_mode: Some("macro-step".to_string()),
            state: AgentTurnState::Queued,
            initial_capability: None,
        };
        self.agent_turn_ledger_mut().queue_turn(turn.clone())?;
        self.agent_turn_contexts_mut()
            .insert(turn_id.clone(), context);
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.agent
            .subagent_task_routes
            .insert(turn_id.clone(), parent_turn.agent_id.clone());
        self.agent.joined_subagent_dependencies.insert(
            turn_id.clone(),
            JoinedSubagentDependency {
                parent_turn_id: parent_turn.turn_id.clone(),
                parent_action_id: action.id.clone(),
                child_turn_id: turn_id.clone(),
                child_agent_id: child_agent_id.to_string(),
                child_display_name: Some(child_display_name.clone()),
            },
        );
        self.append_agent_user_prompt_to_terminal_buffer(child_pane_id, payload)?;
        self.agent.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: Some(child_pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "created state=queued reason=macro_message_step",
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={}",
                self.agent_turn_contexts()
                    .get(&turn_id)
                    .map(|context| context.blocks().len())
                    .unwrap_or_default(),
                model_profile_name
            ),
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable reason=macro_message_step",
        )?;
        self.start_ready_agent_turns()?;
        Ok(Some(ActionResult::running(
            parent_turn,
            action,
            vec![format!(
                "macro step delivered to {child_agent_id}; waiting for subagent result"
            )],
            Some(format!(
                r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","error":null}}"#,
                json_escape(recipient),
                json_escape(&child_agent_id),
                json_escape(&turn_id)
            )),
        )))
    }
}
