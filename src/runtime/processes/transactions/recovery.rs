//! Recovery and reachability checks for stranded agent turns.

use super::{
    AgentTurnRecord, AgentTurnState, BTreeSet, MezError, PaneReadinessState, Result,
    RuntimeSessionService, runtime_execution_ready_for_provider_continuation,
    runtime_pane_readiness_state_name,
};

impl RuntimeSessionService {
    /// Requeues pending shell dispatches that have no live transaction and are
    /// waiting behind readiness state that can be safely retried.
    pub(crate) fn recover_stranded_agent_shell_dispatches(&mut self) -> Result<usize> {
        let candidates = self.stranded_agent_shell_dispatch_recovery_candidates();
        let mut recovered = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            if self
                .agent_turn_executions()
                .get(&turn_id)
                .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                if self.queue_agent_provider_task(turn.turn_id.clone()) {
                    recovered = recovered.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task queued reason=ready_provider_continuation_recovery",
                    )?;
                }
                continue;
            }
            let readiness = self.pane_readiness_state(&turn.pane_id);
            match readiness {
                PaneReadinessState::Ready
                | PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if self.queue_agent_provider_task(turn.turn_id.clone()) {
                        recovered = recovered.saturating_add(1);
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "provider_task queued reason=pending_shell_dispatch_recovery readiness={}",
                                runtime_pane_readiness_state_name(readiness)
                            ),
                        )?;
                    }
                }
                PaneReadinessState::Probing => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        self.process
                            .pane_readiness_overrides
                            .clear_pending_probe(&turn.pane_id);
                        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
                        if self.queue_agent_provider_task(turn.turn_id.clone()) {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness probe was lost; retrying pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                "provider_task queued reason=lost_readiness_probe_recovery",
                            )?;
                        }
                    }
                }
                PaneReadinessState::Busy => {
                    let recovery = match self.pane_foreground_primary_shell_state(&turn.pane_id) {
                        Some(true) => Some((
                            PaneReadinessState::PromptCandidate,
                            "agent: shell readiness looked stale; retrying pending shell command",
                            "provider_task queued reason=stale_busy_recovery",
                        )),
                        Some(false) => None,
                        None => Some((
                            PaneReadinessState::Degraded,
                            "agent: shell readiness metadata was unavailable; retrying pending shell command",
                            "provider_task queued reason=unknown_busy_recovery",
                        )),
                    };
                    if let Some((next_readiness, status, trace)) = recovery {
                        self.set_pane_readiness(&turn.pane_id, next_readiness);
                        if self.queue_agent_provider_task(turn.turn_id.clone()) {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                status,
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                trace,
                            )?;
                        }
                    }
                }
                PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked => {
                    if self.pane_foreground_primary_shell_state(&turn.pane_id) != Some(true) {
                        continue;
                    }
                    self.set_pane_readiness(&turn.pane_id, PaneReadinessState::PromptCandidate);
                    if self.queue_agent_provider_task(turn.turn_id.clone()) {
                        recovered = recovered.saturating_add(1);
                        self.append_agent_status_text_to_terminal_buffer(
                            &turn.pane_id,
                            "agent: shell interactivity block looked stale; retrying pending shell command",
                        )?;
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "provider_task queued reason=stale_interactive_blocked_recovery readiness={}",
                                runtime_pane_readiness_state_name(readiness)
                            ),
                        )?;
                    }
                }
            }
        }
        Ok(recovered)
    }

    /// Runs the stranded agent shell dispatch recovery candidates operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn stranded_agent_shell_dispatch_recovery_candidates(&self) -> Vec<String> {
        self.agent_turn_executions()
            .iter()
            .filter(|(turn_id, execution)| {
                (self.execution_has_pending_shell_dispatch(turn_id, execution)
                    || runtime_execution_ready_for_provider_continuation(execution))
                    && !self.agent_provider_task_is_owned(turn_id)
                    && !self
                        .process
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.turn_id == turn_id.as_str())
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    }

    /// Fails running turns that have no service-owned or actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   actor-owned scheduler state.
    pub(crate) fn fail_unreachable_running_agent_turns_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        let candidates = self.unreachable_running_agent_turn_candidates(actor_progress_turn_ids);
        let mut failed = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: runtime found no remaining progress path; failing turn",
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "provider_task failed reason=no_runtime_progress_path",
            )?;
            let error = MezError::invalid_state(
                "running agent turn has no pending provider, claimed provider, shell, hook, approval, subagent, or continuation work",
            );
            self.fail_configured_agent_provider_task(&turn.turn_id, &error)?;
            failed = failed.saturating_add(1);
        }
        Ok(failed)
    }

    /// Returns running turns that cannot make forward progress without runtime
    /// intervention.
    pub(crate) fn unreachable_running_agent_turn_candidates(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Vec<String> {
        self.agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.state == AgentTurnState::Running)
            .filter(|turn| !self.turn_has_runtime_progress_path(turn, actor_progress_turn_ids))
            .map(|turn| turn.turn_id.clone())
            .collect()
    }

    /// Reports whether a running turn still has a known path to progress.
    pub(super) fn turn_has_runtime_progress_path(
        &self,
        turn: &AgentTurnRecord,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        let turn_id = turn.turn_id.as_str();
        self.agent_provider_task_is_owned(turn_id)
            || actor_progress_turn_ids.contains(turn_id)
            || self.agent_turn_has_pending_steering(turn_id)
            || self
                .process
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.turn_id == turn_id)
            || self.turn_has_pending_focused_shell_hook_continuation(turn_id)
            || self
                .joined_subagent_dependency(turn_id)
                .is_some_and(|dependency| {
                    self.joined_subagent_dependency_has_live_child(dependency)
                })
            || self.agent_turn_has_blocked_approval(turn_id)
            || self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    runtime_execution_ready_for_provider_continuation(execution)
                        || self.execution_has_pending_shell_dispatch(turn_id, execution)
                        || self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                })
    }

    /// Reports whether a focused-shell hook can still resume one of this turn's
    /// shell actions.
    pub(super) fn turn_has_pending_focused_shell_hook_continuation(&self, turn_id: &str) -> bool {
        self.integration
            .focused_shell_hook_transactions()
            .values()
            .filter_map(|pending| pending.continuation.as_ref())
            .any(|continuation| continuation.turn_id == turn_id)
    }
}
