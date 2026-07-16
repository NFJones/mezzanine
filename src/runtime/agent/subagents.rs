//! Runtime agent subagent orchestration helpers.
//!
//! This module owns MAAP subagent spawning, joined-child dependencies, MMP task
//! status/result delivery, and terminal subagent cleanup. It keeps parent-child
//! lifecycle coordination out of the main runtime agent facade.

use super::*;
use mez_agent::{MacroRunPhase, MacroStepTaskResult, normalize_subagent_spawn_role};

impl RuntimeSessionService {
    /// Clears joined-subagent dependencies owned by or waiting on a turn.
    pub(in crate::runtime) fn clear_joined_subagent_dependencies_for_turn(
        &mut self,
        turn_id: &str,
    ) {
        let active_loop_agent = self
            .agent
            .agent_loop_turns
            .get(turn_id)
            .map(|loop_turn| format!("agent-{}", loop_turn.pane_id));
        self.agent
            .joined_subagent_dependencies
            .retain(|child_turn_id, dependency| {
                active_loop_agent.as_ref().is_some_and(|agent_id| {
                    dependency.child_turn_id == turn_id
                        && dependency.child_agent_id == *agent_id
                        && self
                            .agent
                            .agent_loops_by_pane
                            .contains_key(agent_id.strip_prefix("agent-").unwrap_or_default())
                }) || (child_turn_id != turn_id
                    && dependency.parent_turn_id != turn_id
                    && dependency.child_turn_id != turn_id)
            });
    }

    /// Reports whether one joined-subagent dependency still has a live child
    /// turn that can make progress.
    pub(in crate::runtime) fn joined_subagent_dependency_has_live_child(
        &self,
        dependency: &JoinedSubagentDependency,
    ) -> bool {
        let turn_is_live = self.agent_turn_ledger().turns().iter().any(|turn| {
            turn.turn_id == dependency.child_turn_id
                && matches!(
                    turn.state,
                    AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                )
        });
        let loop_is_live = dependency
            .child_agent_id
            .strip_prefix("agent-")
            .is_some_and(|pane_id| {
                self.agent.agent_loops_by_pane.contains_key(pane_id)
                    || self
                        .agent
                        .agent_loop_turns
                        .values()
                        .any(|turn| turn.pane_id == pane_id)
            });
        turn_is_live || loop_is_live
    }

    /// Reports whether a running parent execution is waiting on a live joined
    /// subagent dependency.
    ///
    /// A running `spawn_agent` action only represents progress when it still
    /// maps to the child turn that was created for that specific parent action.
    /// Stale action results without a live dependency must not mask a stranded
    /// parent turn.
    pub(in crate::runtime) fn execution_waiting_for_live_joined_subagents(
        &self,
        parent_turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        execution.terminal_state == AgentTurnState::Running
            && execution.action_results.iter().any(|result| {
                result.action_type == "spawn_agent"
                    && result.status == ActionStatus::Running
                    && self
                        .agent
                        .joined_subagent_dependencies
                        .values()
                        .any(|dependency| {
                            dependency.parent_turn_id == parent_turn_id
                                && dependency.parent_action_id == result.action_id
                                && self.joined_subagent_dependency_has_live_child(dependency)
                        })
            })
    }

    /// Executes any provider-produced MAAP `spawn_agent` actions in a running
    /// turn and rewrites their planned running results to terminal action
    /// results. Successful spawns create the child pane, child turn, MMP task
    /// status, and audit record through the shared runtime spawn helper;
    /// failures are returned as action-level errors so the parent turn can be
    /// transcripted normally.
    pub(in crate::runtime) fn execute_running_spawn_actions_for_turn(
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
                || execution.action_results[index].action_type != "spawn_agent"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running spawn result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    "agent: spawn agent",
                )?;
            }
            execution.action_results[index] = match self
                .execute_spawn_action_for_turn(turn, &action)
            {
                Ok(result) => result,
                Err(error) => {
                    let status = if error.kind() == crate::error::MezErrorKind::Forbidden {
                        ActionStatus::Denied
                    } else {
                        ActionStatus::Failed
                    };
                    let mut result = ActionResult::failed(
                        turn,
                        &action,
                        status,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?;
                    result.structured_content_json = Some(format!(
                        r#"{{"spawn":null,"delivery_status":"failed","error":{{"code":"{}","message":"{}"}}}}"#,
                        runtime_mezzanine_error_code(error.kind()),
                        json_escape(error.message())
                    ));
                    result
                }
            };
            executed = executed.saturating_add(1);
        }
        if execution.action_results.iter().any(|result| {
            result.action_type == "spawn_agent" && result.status == ActionStatus::Running
        }) {
            execution.final_turn = false;
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
                .filter(|result| result.action_type == "spawn_agent")
            {
                self.agent_turn_contexts_mut()
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

    /// Executes one MAAP `spawn_agent` action through the runtime subagent
    /// creation path.
    ///
    /// The action's simple MAAP placement string is parsed through the same
    /// control schema helper used by `agent/spawn`. Unsupported placements,
    /// invalid cooperation modes, scope inheritance errors, or audit failures are
    /// returned to the caller before child state can leak.
    pub(in crate::runtime) fn execute_spawn_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let AgentActionPayload::SpawnAgent {
            role,
            placement,
            cooperation_mode,
            read_scopes,
            write_scopes,
            task_prompt,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "subagent execution requires a spawn_agent action",
            ));
        };
        let normalized_cooperation_mode = runtime_cooperation_mode(cooperation_mode)?;
        let normalized_role = normalize_subagent_spawn_role(
            role,
            self.subagent_profiles.contains_key(role),
            normalized_cooperation_mode,
            write_scopes,
        );
        let prompt = if normalized_role != *role {
            format!(
                "[requested role alias: {}; using built-in profile: {}]\n{}",
                role, normalized_role, task_prompt
            )
        } else {
            task_prompt.clone()
        };
        let normalized_cooperation_mode_name =
            runtime_cooperation_mode_name(normalized_cooperation_mode);
        let params = serde_json::json!({
            "parent_agent": {
                "agent_id": turn.agent_id,
            },
            "placement": placement,
            "role": normalized_role,
            "cooperation_mode": normalized_cooperation_mode_name,
            "read_scopes": read_scopes,
            "write_scopes": write_scopes,
            "prompt": prompt,
        })
        .to_string();
        let spawn = runtime_subagent_spawn_request(&params, false)?;
        let placement_mode = runtime_subagent_placement_mode(&params)?;
        let spawn_json = self.spawn_runtime_subagent_session_owned(spawn, placement_mode)?;
        if self.agent.subagent_wait_policy == SubagentWaitPolicy::Join {
            let (child_agent_id, child_display_name, child_turn_id) =
                runtime_spawn_json_agent_and_turn(&spawn_json)?;
            let child_turn_id = child_turn_id.ok_or_else(|| {
                MezError::invalid_state("subagent spawn response missing turn id")
            })?;
            self.agent.joined_subagent_dependencies.insert(
                child_turn_id.clone(),
                JoinedSubagentDependency {
                    parent_turn_id: turn.turn_id.clone(),
                    parent_action_id: action.id.clone(),
                    child_turn_id: child_turn_id.clone(),
                    child_agent_id: child_agent_id.clone(),
                    child_display_name: child_display_name.clone(),
                },
            );
            let child_label =
                runtime_subagent_display_label(&child_agent_id, child_display_name.as_deref());
            let task_summary = runtime_agent_terminal_preview(task_prompt);
            return Ok(ActionResult::running(
                turn,
                action,
                vec![format!(
                    "subagent {child_label} spawn accepted for {placement} placement; waiting for task result: {task_summary}"
                )],
                Some(format!(
                    r#"{{"spawn":{},"placement":"{}","delivery_status":"accepted","join_policy":"join","join_state":"waiting","child_agent_id":"{}","child_display_name":{},"child_turn_id":"{}","error":null}}"#,
                    spawn_json,
                    json_escape(placement),
                    json_escape(&child_agent_id),
                    child_display_name
                        .as_deref()
                        .map(|name| format!(r#""{}""#, json_escape(name)))
                        .unwrap_or_else(|| "null".to_string()),
                    json_escape(&child_turn_id)
                )),
            ));
        }
        Ok(ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "subagent spawn accepted for {} placement: {}",
                placement,
                runtime_agent_terminal_preview(task_prompt)
            )],
            Some(format!(
                r#"{{"spawn":{},"placement":"{}","delivery_status":"accepted","join_policy":"detach","error":null}}"#,
                spawn_json,
                json_escape(placement)
            )),
        ))
    }

    /// Runs the append subagent spawn audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_subagent_spawn_audit(
        &mut self,
        spawn: &SubagentSpawnRequest,
        child_agent_id: &str,
        pane_id: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let record = AuditRecord::subagent_spawn(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: spawn.parent_agent_id.clone(),
            },
            spawn.parent_agent_id.clone(),
            child_agent_id.to_string(),
            spawn.requested_role.clone(),
            runtime_cooperation_mode_name(spawn.cooperation_mode),
            "accepted",
        )
        .with_pane_id(pane_id.to_string());
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the emit subagent task result for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn emit_subagent_task_result_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        if self.agent_loop_execution_will_continue(turn, execution) {
            return Ok(());
        }
        let success = execution.terminal_state == AgentTurnState::Completed;
        let summary = if success {
            "subagent task completed"
        } else {
            "subagent task failed"
        };
        let output = subagent_task_output_for_execution(execution);
        let loop_dependency = self.take_agent_loop_dependency_for_turn(&turn.turn_id);
        self.emit_subagent_task_result_with_dependency(
            turn,
            loop_dependency,
            success,
            summary,
            &output,
        )
    }

    /// Runs the emit cancelled subagent task result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn emit_cancelled_subagent_task_result(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let loop_dependency = self.take_agent_loop_dependency_for_turn(&turn.turn_id);
        self.emit_subagent_task_result_with_dependency(
            turn,
            loop_dependency,
            false,
            "subagent task cancelled",
            "cancelled by runtime request",
        )
    }

    /// Runs the emit subagent task result for state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn emit_subagent_task_result_for_state(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<()> {
        let loop_dependency = self.take_agent_loop_dependency_for_turn(&turn.turn_id);
        match state {
            AgentTurnState::Completed => self.emit_subagent_task_result_with_dependency(
                turn,
                loop_dependency,
                true,
                "subagent task completed",
                "completed without provider output",
            ),
            AgentTurnState::Failed => self.emit_subagent_task_result_with_dependency(
                turn,
                loop_dependency,
                false,
                "subagent task failed",
                "failed without provider output",
            ),
            AgentTurnState::Interrupted => self.emit_subagent_task_result_with_dependency(
                turn,
                loop_dependency,
                false,
                "subagent task interrupted",
                "interrupted by snapshot resume",
            ),
            _ => Ok(()),
        }
    }

    /// Takes the controller-owned macro join for one terminal loop work turn.
    ///
    /// Taking the record before parent delivery gives cancellation, failure,
    /// and normal completion paths the same exactly-once behavior even when a
    /// later lifecycle helper observes the same terminal turn again.
    fn take_agent_loop_dependency_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Option<JoinedSubagentDependency> {
        let pane_id = self.agent.agent_loop_turns.get(turn_id)?.pane_id.clone();
        let completion = self
            .agent
            .agent_loops_by_pane
            .get_mut(&pane_id)?
            .completion
            .take()?;
        Some(JoinedSubagentDependency {
            parent_turn_id: completion.parent_turn_id,
            parent_action_id: completion.parent_action_id,
            child_turn_id: completion.child_turn_id,
            child_agent_id: completion.child_agent_id,
            child_display_name: completion.child_display_name,
        })
    }

    /// Emits an intermediate MMP task-status update for a spawned subagent
    /// without closing its task route or releasing its active scope.
    ///
    /// Status delivery is best-effort after spawn setup: an offline parent is
    /// recorded as an undelivered runtime event, but the child turn keeps its
    /// normal lifecycle so approval or provider work can continue.
    pub(in crate::runtime) fn emit_subagent_task_status(
        &mut self,
        turn: &AgentTurnRecord,
        state: TaskState,
        progress_percent: Option<u8>,
        summary: &str,
    ) -> Result<()> {
        let Some(parent_agent_id) = self.subagent_task_parent(&turn.turn_id) else {
            return Ok(());
        };
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let parent_identity = self.control.message_service_mut().ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque(parent_agent_id.clone()).ok_or_else(|| {
                    MezError::invalid_args("subagent parent agent id is invalid for MMP")
                })?,
                pane_id: None,
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
            },
            now_ms,
        )?;
        if self
            .control
            .message_service()
            .subscription(&parent_identity.agent_id)
            .is_none()
        {
            self.control
                .message_service_mut()
                .subscribe(&parent_identity.agent_id)?;
        }
        let child_identity = self.runtime_message_sender_identity(turn)?;
        let payload = TaskStatusPayload {
            task_id: turn.turn_id.clone(),
            state,
            progress_percent,
            summary: summary.to_string(),
        };
        let child_display_name = self
            .subagent_lineage(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!(
                "{}:task_status:{}",
                turn.turn_id,
                runtime_task_state_suffix(state)
            ),
            message_type: "task_status".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: child_identity.clone(),
            recipient: Recipient::Agent(parent_identity.agent_id),
            correlation_id: Some(turn.turn_id.clone()),
            ttl_ms: None,
            content_type: "application/json".to_string(),
            payload: payload.to_json(),
            extension_fields: child_display_name
                .as_deref()
                .map(|name| {
                    vec![(
                        "subagent_display_name".to_string(),
                        format!(r#""{}""#, json_escape(name)),
                    )]
                })
                .unwrap_or_default(),
        };
        let delivery = self.control.message_service_mut().accept_at(
            &child_identity.agent_id,
            envelope,
            now_ms,
        );
        let child_label =
            runtime_subagent_display_label(&turn.agent_id, child_display_name.as_deref());
        self.append_subagent_parent_status_line(
            &parent_agent_id,
            &format!(
                "subagent {} {}: {}",
                child_label,
                runtime_task_state_suffix(state),
                summary
            ),
        )?;
        if let Err(error) = delivery {
            let error = MezError::from(error);
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_status":"undelivered","error_code":"{}","error":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(turn.state),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ),
            )?;
        }
        Ok(())
    }

    /// Emits a terminal task result with an optional controller-owned join.
    fn emit_subagent_task_result_with_dependency(
        &mut self,
        turn: &AgentTurnRecord,
        dependency: Option<JoinedSubagentDependency>,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<()> {
        let parent_agent_id = dependency
            .as_ref()
            .and_then(|dependency| {
                self.agent_turn_ledger()
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == dependency.parent_turn_id)
                    .map(|turn| turn.agent_id.clone())
            })
            .or_else(|| self.subagent_task_result_parent_agent_id(turn));
        let has_subagent_runtime_state =
            parent_agent_id.is_some() || self.has_subagent_authority_state(&turn.agent_id);
        if !has_subagent_runtime_state {
            return Ok(());
        }

        let now_ms = current_unix_seconds().saturating_mul(1000);
        let child_display_name = self
            .subagent_lineage(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let child_label =
            runtime_subagent_display_label(&turn.agent_id, child_display_name.as_deref());
        let delivery = match parent_agent_id.clone() {
            Some(parent_agent_id) => self.deliver_subagent_task_result_message(
                turn,
                &parent_agent_id,
                success,
                summary,
                output,
                now_ms,
            ),
            None => {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_result":"undelivered","error_code":"not_found","error":"subagent parent route not found"}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&turn.turn_id),
                        runtime_agent_turn_state_name(turn.state),
                    ),
                )?;
                Ok(())
            }
        };
        if let Some(parent_agent_id) = parent_agent_id.as_deref() {
            self.append_subagent_parent_status_line(
                parent_agent_id,
                &format!(
                    "subagent {} {}: {}",
                    child_label,
                    runtime_subagent_result_status_label(success, summary),
                    summary
                ),
            )?;
        }
        self.agent.subagent_task_routes.remove(&turn.turn_id);
        let is_macro_step =
            dependency.is_some() || turn.cooperation_mode.as_deref() == Some("macro-step");
        let terminal_macro_step_failure = is_macro_step && !success;
        if !is_macro_step || terminal_macro_step_failure {
            self.remove_subagent_authority_state(&turn.agent_id);
        }
        if let Some(dependency) = dependency {
            self.resolve_joined_subagent_dependency_record(
                turn, dependency, success, summary, output,
            )?;
        } else {
            self.resolve_joined_subagent_dependency(turn, success, summary, output)?;
        }
        if !is_macro_step || terminal_macro_step_failure {
            self.agent
                .pending_terminal_subagent_pane_closes
                .insert(turn.pane_id.clone());
        }
        if let Err(error) = delivery {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_result":"undelivered","error_code":"{}","error":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(turn.state),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ),
            )?;
        }
        Ok(())
    }

    /// Returns the parent agent that should receive a terminal child task result.
    ///
    /// Normal subagent delivery uses `subagent_task_routes`, but joined parent
    /// continuations must still be resolved if that route was already cleaned up.
    /// In that case the parent turn recorded in the join dependency is the
    /// fallback source of truth.
    fn subagent_task_result_parent_agent_id(&self, turn: &AgentTurnRecord) -> Option<String> {
        self.subagent_task_parent(&turn.turn_id)
            .or_else(|| {
                let dependency = self.agent.joined_subagent_dependencies.get(&turn.turn_id)?;
                self.agent_turn_ledger()
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == dependency.parent_turn_id)
                    .map(|turn| turn.agent_id.clone())
            })
            .or_else(|| {
                self.subagent_lineage(&turn.agent_id)
                    .map(|lineage| lineage.parent_agent_id.clone())
                    .filter(|parent_agent_id| !parent_agent_id.is_empty())
            })
    }

    /// Delivers the terminal `task_result` envelope for a spawned subagent.
    ///
    /// Delivery is best-effort from the caller's perspective: the caller records
    /// and resolves terminal child state even when this function returns an MMP
    /// identity, subscription, or accept error.
    fn deliver_subagent_task_result_message(
        &mut self,
        turn: &AgentTurnRecord,
        parent_agent_id: &str,
        success: bool,
        summary: &str,
        output: &str,
        now_ms: u64,
    ) -> Result<()> {
        let parent_identity = self.control.message_service_mut().ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque(parent_agent_id.to_string()).ok_or_else(|| {
                    MezError::invalid_args("subagent parent agent id is invalid for MMP")
                })?,
                pane_id: None,
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
            },
            now_ms,
        )?;
        if self
            .control
            .message_service()
            .subscription(&parent_identity.agent_id)
            .is_none()
        {
            self.control
                .message_service_mut()
                .subscribe(&parent_identity.agent_id)?;
        }
        let child_identity = self.runtime_message_sender_identity(turn)?;
        let child_display_name = self
            .subagent_lineage(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let payload = TaskResultPayload {
            task_id: turn.turn_id.clone(),
            success,
            summary: summary.to_string(),
            output: output.to_string(),
        };
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!("{}:task_result:final", turn.turn_id),
            message_type: "task_result".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: child_identity.clone(),
            recipient: Recipient::Agent(parent_identity.agent_id),
            correlation_id: Some(turn.turn_id.clone()),
            ttl_ms: None,
            content_type: "application/json".to_string(),
            payload: payload.to_json(),
            extension_fields: child_display_name
                .as_deref()
                .map(|name| {
                    vec![(
                        "subagent_display_name".to_string(),
                        format!(r#""{}""#, json_escape(name)),
                    )]
                })
                .unwrap_or_default(),
        };
        Ok(self
            .control
            .message_service_mut()
            .accept_at(&child_identity.agent_id, envelope, now_ms)
            .map(|_| ())?)
    }

    /// Resolves a parent `spawn_agent` action that joined a child task result.
    ///
    /// Joined child task results are delivered through MMP for observability and
    /// also converted into the parent turn's MAAP action result so the next
    /// provider request receives the child output as tool context. The parent
    /// stays blocked until every joined child action in its current execution
    /// has settled, then it is resumed and queued for provider continuation.
    fn resolve_joined_subagent_dependency(
        &mut self,
        turn: &AgentTurnRecord,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<()> {
        let Some(dependency) = self
            .agent
            .joined_subagent_dependencies
            .get(&turn.turn_id)
            .cloned()
            .or_else(|| {
                self.agent
                    .joined_subagent_dependencies
                    .values()
                    .find(|dependency| dependency.child_agent_id == turn.agent_id)
                    .cloned()
            })
        else {
            return Ok(());
        };
        self.resolve_joined_subagent_dependency_record(turn, dependency, success, summary, output)
    }

    /// Resolves one explicit joined dependency against a terminal child turn.
    fn resolve_joined_subagent_dependency_record(
        &mut self,
        _turn: &AgentTurnRecord,
        dependency: JoinedSubagentDependency,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<()> {
        let Some(parent_turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == dependency.parent_turn_id)
            .cloned()
        else {
            return Ok(());
        };
        let parent_previous_state = parent_turn.state;
        let (observed_result, ready_for_continuation) = {
            let Some(execution) = self
                .agent_turn_executions_mut()
                .get_mut(&dependency.parent_turn_id)
            else {
                return Ok(());
            };
            let Some(batch) = execution.response.action_batch.as_ref() else {
                return Ok(());
            };
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == dependency.parent_action_id)
                .cloned()
            else {
                return Ok(());
            };
            let Some(result_index) = execution
                .action_results
                .iter()
                .position(|result| result.action_id == dependency.parent_action_id)
            else {
                return Ok(());
            };
            let child_label = runtime_subagent_display_label(
                &dependency.child_agent_id,
                dependency.child_display_name.as_deref(),
            );
            let result_summary = if success {
                format!("subagent {child_label} completed: {summary}")
            } else {
                format!("subagent {child_label} failed: {summary}")
            };
            let structured_result = format!(
                r#"{{"join_policy":"join","join_state":"completed","child_agent_id":"{}","child_display_name":{},"child_turn_id":"{}","task_result":{{"success":{},"summary":"{}","output":"{}"}}}}"#,
                json_escape(&dependency.child_agent_id),
                dependency
                    .child_display_name
                    .as_deref()
                    .map(|name| format!(r#""{}""#, json_escape(name)))
                    .unwrap_or_else(|| "null".to_string()),
                json_escape(&dependency.child_turn_id),
                success,
                json_escape(summary),
                json_escape(output)
            );
            let observed_result = if success {
                ActionResult::succeeded(
                    &parent_turn,
                    &action,
                    vec![result_summary],
                    Some(structured_result),
                )
            } else {
                let mut result = ActionResult::failed(
                    &parent_turn,
                    &action,
                    ActionStatus::Failed,
                    "macro_step_failed",
                    result_summary,
                )?;
                result.structured_content_json = Some(structured_result);
                result
            };
            execution.action_results[result_index] = observed_result.clone();
            execution.final_turn = false;
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
            let ready_for_continuation =
                runtime_execution_ready_for_provider_continuation(execution);
            (observed_result, ready_for_continuation)
        };
        if let Some(context) = self
            .agent_turn_contexts_mut()
            .get_mut(&dependency.parent_turn_id)
        {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: format!("action result {}", observed_result.action_id),
                content: action_result_context_content(&observed_result),
            });
        }
        let mut failed_macro_parent_turn = None;
        let mut macro_result_status = None;
        if let Some(parent_run_id) = self
            .agent
            .macro_run_by_child_turn
            .remove(&dependency.child_turn_id)
            && parent_run_id == dependency.parent_turn_id
            && let Some(run) = self
                .agent
                .macro_runs_by_parent_turn
                .get_mut(parent_run_id.as_str())
            && let Some(step) = run.steps.iter_mut().find(|step| {
                step.child_turn_id.as_deref() == Some(dependency.child_turn_id.as_str())
            })
        {
            step.task_result = Some(MacroStepTaskResult {
                success,
                summary: summary.to_string(),
                output: output.to_string(),
            });
            run.current_step = step.index;
            run.phase = MacroRunPhase::WaitingForJudge {
                step_index: step.index,
            };
            macro_result_status =
                Some((run.macro_name.clone(), step.index, run.steps.len(), success));
            if !success {
                failed_macro_parent_turn = Some(parent_run_id);
            }
        }
        self.agent
            .joined_subagent_dependencies
            .remove(&dependency.child_turn_id);
        if let Some((macro_name, step_index, total_steps, true)) = macro_result_status.as_ref() {
            self.append_agent_macro_status_to_terminal_buffer(
                &parent_turn.pane_id,
                macro_name,
                Some(*step_index),
                *total_steps,
                "result received; evaluating",
            )?;
        }
        self.append_agent_trace_turn_event(
            &parent_turn.pane_id,
            &parent_turn.turn_id,
            &format!(
                "joined_subagent_result child_turn={} child_agent={} success={}",
                dependency.child_turn_id, dependency.child_agent_id, success
            ),
        )?;
        if let Some(parent_turn_id) = failed_macro_parent_turn {
            self.agent.macro_runs_by_parent_turn.remove(&parent_turn_id);
            let _ = self.agent.agent_scheduler.complete(&parent_turn_id);
            self.agent_turn_ledger_mut()
                .finish_turn(&parent_turn_id, AgentTurnState::Failed)?;
            self.append_agent_trace_turn_transition(
                &parent_turn,
                parent_previous_state,
                AgentTurnState::Failed,
                "macro_step_failed",
            )?;
            let (macro_name, step_index, total_steps, _) = macro_result_status
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("failed macro step lost lifecycle state"))?;
            self.append_agent_macro_error_to_terminal_buffer(
                &parent_turn.pane_id,
                macro_name,
                *step_index,
                *total_steps,
                &format!("worker failed: {summary}"),
            )?;
            self.start_ready_agent_turns()?;
            return Ok(());
        }
        if ready_for_continuation {
            let _ = self
                .agent
                .agent_scheduler
                .resume_blocked(&parent_turn.turn_id);
            self.append_agent_trace_turn_event(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                "scheduler blocked -> running reason=joined_subagent_result_ready",
            )?;
            if parent_previous_state == AgentTurnState::Blocked {
                self.agent_turn_ledger_mut()
                    .resume_blocked_turn(&parent_turn.turn_id)?;
                self.append_agent_trace_turn_transition(
                    &parent_turn,
                    AgentTurnState::Blocked,
                    AgentTurnState::Running,
                    "joined_subagent_result_ready",
                )?;
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(parent_turn.turn_id.clone());
            self.append_agent_trace_turn_event(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                "provider_task queued reason=joined_subagent_result_ready",
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &parent_turn.pane_id,
                "agent: subagent results received; continuing",
            )?;
        } else {
            self.start_ready_agent_turns()?;
        }
        Ok(())
    }

    /// Appends a subagent status update into the controlling pane buffer.
    ///
    /// MMP remains the structured delivery mechanism; this visible line gives
    /// the user a copyable, in-context event stream in the parent window.
    pub(in crate::runtime) fn append_subagent_parent_status_line(
        &mut self,
        parent_agent_id: &str,
        text: &str,
    ) -> Result<()> {
        let Some(parent_pane_id) = runtime_agent_pane_id(parent_agent_id) else {
            return Ok(());
        };
        if runtime_pane_by_id(&self.session, parent_pane_id.as_str()).is_err() {
            return Ok(());
        }
        self.append_agent_status_text_to_terminal_buffer(parent_pane_id.as_str(), text)
    }

    /// Closes a terminal subagent pane after final turn cleanup has run.
    pub(in crate::runtime) fn close_terminal_subagent_pane_if_pending(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if !self
            .agent
            .pending_terminal_subagent_pane_closes
            .remove(&turn.pane_id)
        {
            return Ok(());
        }
        if self.pane_is_closing(&turn.pane_id) {
            return Ok(());
        }
        if self.find_pane_descriptor(&turn.pane_id).is_none() {
            return Ok(());
        }
        let Some(primary) = self.session.primary_client_id().cloned() else {
            return Ok(());
        };
        self.dispatch_runtime_pane_close(
            &primary,
            &format!(
                r#"{{"pane_id":"{}","force":true}}"#,
                json_escape(&turn.pane_id)
            ),
        )?;
        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        self.agent
            .subagent_window_ids
            .retain(|window_id| live_windows.contains(window_id));
        self.refresh_subagent_window_names(&primary)?;
        Ok(())
    }
}

/// Derives the pane identity encoded by runtime-created agent ids.
pub(in crate::runtime) fn runtime_agent_pane_id(agent_id: &str) -> Option<PaneId> {
    agent_id
        .strip_prefix("agent-")
        .and_then(|pane_id| PaneId::parse('%', pane_id.to_string()))
}
