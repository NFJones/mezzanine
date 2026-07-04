//! Runtime agent turn lifecycle and scheduler-drain helpers.
//!
//! This module owns service methods for starting turns, finishing turns,
//! draining ready scheduler work, and cleaning up turns when panes or parent
//! subagent trees close. Provider execution, action dispatch, and presentation
//! remain in their narrower runtime-agent modules.

use super::*;

impl RuntimeSessionService {
    /// Runs the start agent turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_agent_turn(&mut self, turn: AgentTurnRecord) -> Result<AgentShellSession> {
        self.require_live()?;
        runtime_pane_by_id(&self.session, &turn.pane_id)?;
        self.agent_shell_store
            .ensure_session(turn.pane_id.as_str())?;
        if self
            .agent_shell_store
            .get(turn.pane_id.as_str())
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
        {
            return Err(MezError::conflict(
                "agent shell session already has a running turn",
            ));
        }

        self.agent_turn_ledger.start_turn(turn.clone())?;
        self.runtime_metrics.record_agent_turn_started();
        self.agent_shell_store
            .start_turn(turn.pane_id.as_str(), turn.turn_id.clone())?;
        self.append_agent_trace_turn_transition(
            &turn,
            turn.state,
            AgentTurnState::Running,
            "runtime_start_agent_turn",
        )?;
        self.checkpoint_agent_session_metadata()?;
        self.agent_shell_store
            .get(turn.pane_id.as_str())
            .cloned()
            .ok_or_else(|| MezError::invalid_state("started agent shell session was not retained"))
    }

    /// Updates agent prompt display lines only while the pane still exists.
    ///
    /// Terminal subagent completion can close the child pane as part of final
    /// cleanup after the parent receives a task result. Late presentation
    /// updates for that child are no longer meaningful and must not turn a
    /// successful terminal cleanup path into a `pane not found` failure.
    pub(in crate::runtime) fn set_agent_prompt_display_lines_if_pane_present(
        &mut self,
        pane_id: &str,
        display_lines: Vec<String>,
    ) -> Result<()> {
        if self.find_pane_descriptor(pane_id).is_none() {
            return Ok(());
        }
        self.set_agent_prompt_display_lines(pane_id, display_lines)
    }

    /// Runs the finish agent turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn finish_agent_turn(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        state: AgentTurnState,
    ) -> Result<AgentShellSession> {
        self.require_live()?;
        runtime_pane_by_id(&self.session, pane_id)?;
        let running_turn = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .ok_or_else(|| MezError::invalid_state("agent shell session has no running turn"))?;
        if running_turn != turn_id {
            return Err(MezError::invalid_args(
                "finished turn does not match running agent shell turn",
            ));
        }
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;

        if state == AgentTurnState::Failed
            && let Some(execution) = self.agent_turn_executions.get(turn_id).cloned()
            && execution.terminal_state == AgentTurnState::Failed
        {
            let reason = runtime_unrecovered_failure_reason(
                turn_id,
                &execution,
                self.agent_action_failure_retry_limit,
                &self.agent_turn_failure_feedback_attempts,
            );
            self.present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
                pane_id, &execution, &reason,
            )?;
        }
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.emit_subagent_task_result_for_state(&turn, state)?;
        }
        if let Some(footer) = runtime_agent_finished_footer_line(&turn, state) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, &footer)?;
        }
        let previous_state = turn.state;
        self.runtime_metrics.record_agent_turn_finished(state);
        self.agent_turn_ledger.finish_turn(turn_id, state)?;
        self.append_agent_trace_turn_transition(&turn, previous_state, state, "finish_agent_turn")?;
        self.agent_turn_contexts.remove(turn_id);
        self.agent_turn_executions.remove(turn_id);
        self.agent_turn_pending_steering.remove(turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
        self.agent_turn_shell_dispatch_history.remove(turn_id);
        self.agent_turn_network_action_history.remove(turn_id);
        self.clear_joined_subagent_dependencies_for_turn(turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(turn_id);
        self.agent_turn_model_profiles.remove(turn_id);
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
        self.blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn_id);
        let finished = self
            .agent_shell_store
            .finish_turn(pane_id, turn_id)?
            .clone();
        if finished.visibility == AgentShellVisibility::Hidden {
            self.advance_pane_shell_prompt_after_agent_exit(pane_id)?;
        }
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.close_terminal_subagent_pane_if_pending(&turn)?;
        }
        self.start_ready_agent_turns()?;
        self.checkpoint_agent_session_metadata()?;
        Ok(finished)
    }

    /// Cleans up a queued or blocked turn that is not currently bound as the
    /// pane shell's running turn.
    ///
    /// Some scheduler states, especially queued work and blocked turns whose
    /// scheduler slot has been released, need terminal cleanup without going
    /// through `AgentShellStore::finish_turn`. Callers are responsible for
    /// emitting any transcript or task-result records before using this helper.
    pub(in crate::runtime) fn finish_agent_turn_without_shell_session(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<AgentShellSession> {
        if state == AgentTurnState::Failed
            && let Some(execution) = self.agent_turn_executions.get(&turn.turn_id).cloned()
            && execution.terminal_state == AgentTurnState::Failed
        {
            let reason = runtime_unrecovered_failure_reason(
                &turn.turn_id,
                &execution,
                self.agent_action_failure_retry_limit,
                &self.agent_turn_failure_feedback_attempts,
            );
            self.present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
                &turn.pane_id,
                &execution,
                &reason,
            )?;
        }
        if let Some(footer) = runtime_agent_finished_footer_line(turn, state) {
            self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &footer)?;
        }
        self.runtime_metrics.record_agent_turn_finished(state);
        self.agent_turn_ledger.finish_turn(&turn.turn_id, state)?;
        self.append_agent_trace_turn_transition(
            turn,
            turn.state,
            state,
            "finish_agent_turn_without_shell_session",
        )?;
        self.agent_turn_contexts.remove(&turn.turn_id);
        self.agent_turn_executions.remove(&turn.turn_id);
        self.agent_turn_pending_steering.remove(&turn.turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(&turn.turn_id);
        self.agent_turn_shell_dispatch_history.remove(&turn.turn_id);
        self.agent_turn_network_action_history.remove(&turn.turn_id);
        self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
        self.agent_turn_model_profiles.remove(&turn.turn_id);
        self.pending_agent_provider_tasks.remove(&turn.turn_id);
        self.claimed_agent_provider_tasks.remove(&turn.turn_id);
        self.blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn.turn_id);
        let session = self
            .agent_shell_store
            .ensure_session(&turn.pane_id)?
            .clone();
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.close_terminal_subagent_pane_if_pending(turn)?;
        }
        self.start_ready_agent_turns()?;
        self.checkpoint_agent_session_metadata()?;
        Ok(session)
    }

    /// Completes the scheduler entry for one running turn and finishes the
    /// terminal runtime turn through the cleanup path that matches its current
    /// pane shell binding.
    ///
    /// # Parameters
    /// - `turn`: The running turn being settled.
    /// - `state`: The terminal turn state to record.
    /// - `reason`: Trace reason attached to the scheduler transition.
    pub(in crate::runtime) fn complete_running_agent_turn_and_start_ready(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
        reason: &str,
    ) -> Result<AgentShellSession> {
        let _ = self.agent_scheduler.complete(&turn.turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "scheduler running -> {} reason={}",
                runtime_agent_turn_state_name(state),
                reason
            ),
        )?;
        if self
            .agent_shell_store
            .get(&turn.pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            == Some(turn.turn_id.as_str())
        {
            self.finish_agent_turn(&turn.pane_id, &turn.turn_id, state)
        } else {
            self.finish_agent_turn_without_shell_session(turn, state)
        }
    }

    /// Starts all scheduler work that is runnable in the current runtime state.
    pub(in crate::runtime) fn start_ready_agent_turns(&mut self) -> Result<usize> {
        self.start_ready_agent_turns_suppressing_status_for(None)
    }

    /// Starts all scheduler work that is runnable in the current runtime
    /// state while suppressing the scheduler-start status event for a selected
    /// turn.
    ///
    /// The prompt-submission path uses this to preserve the existing
    /// model-profile-bearing status event for the newly submitted turn while
    /// still draining older runnable scheduler entries.
    pub(in crate::runtime) fn start_ready_agent_turns_suppressing_status_for(
        &mut self,
        suppressed_turn_id: Option<&str>,
    ) -> Result<usize> {
        let mut started = 0usize;
        while let Some(running) = self.agent_scheduler.start_ready() {
            let turn = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == running.turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("scheduled turn is missing from runtime ledger")
                })?;
            self.agent_turn_ledger.mark_turn_running(&running.turn_id)?;
            self.agent_shell_store
                .start_turn(&turn.pane_id, running.turn_id.clone())?;
            self.pending_agent_provider_tasks
                .insert(running.turn_id.clone());
            self.append_agent_trace_turn_transition(
                &turn,
                AgentTurnState::Queued,
                AgentTurnState::Running,
                "scheduler_start",
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &running.turn_id,
                "provider_task queued reason=scheduler_start",
            )?;
            if suppressed_turn_id != Some(running.turn_id.as_str()) {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","scheduler_started":true}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&running.turn_id)
                    ),
                )?;
            }
            started = started.saturating_add(1);
        }
        if started > 0 {
            self.checkpoint_agent_session_metadata()?;
        }
        Ok(started)
    }

    /// Fails queued, running, or blocked agent turns that belong to panes being
    /// closed outside the normal agent lifecycle.
    ///
    /// Shutdown paths call this before removing panes from the session model so
    /// final subagent task results can still identify the child pane, scheduled
    /// work is cancelled, pending shell transactions are interrupted, and any
    /// active subagent write scope is released.
    pub(in crate::runtime) fn fail_agent_turns_for_pane_shutdown(
        &mut self,
        pane_ids: &[String],
        reason: &str,
    ) -> Result<usize> {
        let pane_ids = pane_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if pane_ids.is_empty() {
            return Ok(0);
        }
        for pane_id in &pane_ids {
            let parent_agent_id = format!("agent-{pane_id}");
            self.close_subagent_descendants_for_parent_agent(&parent_agent_id, reason)?;
        }
        for pane_id in &pane_ids {
            self.pane_closing.insert((*pane_id).to_string());
        }
        let turns = self
            .agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| {
                pane_ids.contains(turn.pane_id.as_str())
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut failed = 0usize;
        for turn in turns {
            let _ = self.agent_scheduler.cancel(&turn.turn_id);
            self.cancel_live_shell_transactions_for_turn(&turn.turn_id)?;
            let running_in_shell = self
                .agent_shell_store
                .get(&turn.pane_id)
                .and_then(|session| session.running_turn_id.as_deref())
                == Some(turn.turn_id.as_str());
            if running_in_shell {
                self.finish_agent_turn(&turn.pane_id, &turn.turn_id, AgentTurnState::Failed)?;
            } else {
                self.emit_subagent_task_result_for_state(&turn, AgentTurnState::Failed)?;
                self.agent_turn_ledger
                    .finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                self.append_agent_trace_turn_transition(
                    &turn,
                    turn.state,
                    AgentTurnState::Failed,
                    "pane_shutdown_without_shell_session",
                )?;
                self.agent_turn_contexts.remove(&turn.turn_id);
                self.agent_turn_executions.remove(&turn.turn_id);
                self.agent_turn_pending_steering.remove(&turn.turn_id);
                self.agent_turn_shell_dispatch_history.remove(&turn.turn_id);
                self.agent_turn_network_action_history.remove(&turn.turn_id);
                self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
                self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
                self.agent_turn_model_profiles.remove(&turn.turn_id);
                self.pending_agent_provider_tasks.remove(&turn.turn_id);
                self.blocked_agent_approval_refs
                    .retain(|_, approval_ref| approval_ref.turn_id != turn.turn_id);
            }
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","reason":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    json_escape(reason)
                ),
            )?;
            failed = failed.saturating_add(1);
        }
        self.start_ready_agent_turns()?;
        Ok(failed)
    }

    /// Closes every live subagent pane descended from one parent agent.
    ///
    /// # Parameters
    /// - `parent_agent_id`: Agent id whose child delegation tree should close.
    /// - `reason`: Human-readable shutdown reason used in trace/status events.
    pub(in crate::runtime) fn close_subagent_descendants_for_parent_agent(
        &mut self,
        parent_agent_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let descendants = self.subagent_descendant_agent_ids_for_parent(parent_agent_id);
        if descendants.is_empty() {
            return Ok(0);
        }
        let Some(primary_client_id) = self.session.primary_client_id().cloned() else {
            for agent_id in descendants {
                if let Some(pane_id) = runtime_agent_pane_id(&agent_id) {
                    self.cleanup_removed_pane_runtime_state(pane_id.as_str());
                }
            }
            return Ok(0);
        };
        let mut closed = 0usize;
        for agent_id in descendants {
            let Some(pane_id) = runtime_agent_pane_id(&agent_id) else {
                self.deregister_macro_managed_subagent(&agent_id);
                self.subagent_lineage.remove(&agent_id);
                self.subagent_scope_declarations.remove(&agent_id);
                self.subagent_scopes.unregister(&agent_id);
                continue;
            };
            let pane_id_string = pane_id.to_string();
            if runtime_pane_by_id(&self.session, &pane_id_string).is_ok() {
                let params = format!(
                    r#"{{"pane_id":"{}","force":true}}"#,
                    json_escape(&pane_id_string)
                );
                self.dispatch_runtime_pane_close(&primary_client_id, &params)?;
                closed = closed.saturating_add(1);
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_id":"{}","state":"closed","reason":"parent_agent_closed","parent_agent_id":"{}","detail":"{}"}}"#,
                        json_escape(&pane_id_string),
                        json_escape(&agent_id),
                        json_escape(parent_agent_id),
                        json_escape(reason)
                    ),
                )?;
            } else {
                self.cleanup_removed_pane_runtime_state(&pane_id_string);
            }
        }
        Ok(closed)
    }

    /// Returns descendant subagent ids deepest-first for deterministic closure.
    ///
    /// # Parameters
    /// - `parent_agent_id`: Direct or root parent agent id to inspect.
    fn subagent_descendant_agent_ids_for_parent(&self, parent_agent_id: &str) -> Vec<String> {
        let mut descendants = self
            .subagent_lineage
            .iter()
            .filter(|(_agent_id, lineage)| {
                lineage.parent_agent_id == parent_agent_id
                    || self.subagent_lineage_has_ancestor(lineage, parent_agent_id)
            })
            .map(|(agent_id, lineage)| (lineage.depth, agent_id.clone()))
            .collect::<Vec<_>>();
        descendants.sort_by(|(left_depth, left_agent), (right_depth, right_agent)| {
            right_depth
                .cmp(left_depth)
                .then_with(|| left_agent.cmp(right_agent))
        });
        descendants
            .into_iter()
            .map(|(_depth, agent_id)| agent_id)
            .collect()
    }

    /// Reports whether a lineage record is below a target ancestor.
    ///
    /// # Parameters
    /// - `lineage`: Child lineage to walk upward.
    /// - `ancestor_agent_id`: Candidate ancestor agent id.
    fn subagent_lineage_has_ancestor(
        &self,
        lineage: &super::super::service_state::RuntimeSubagentLineage,
        ancestor_agent_id: &str,
    ) -> bool {
        let mut current_parent = lineage.parent_agent_id.as_str();
        while !current_parent.is_empty() {
            if current_parent == ancestor_agent_id {
                return true;
            }
            let Some(parent_lineage) = self.subagent_lineage.get(current_parent) else {
                return false;
            };
            current_parent = parent_lineage.parent_agent_id.as_str();
        }
        false
    }
}
