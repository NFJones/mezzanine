//! Shell transaction event observation and foreground-shell state.

use super::{
    AgentTurnState, Result, RuntimeSessionService, TerminalOscEvent,
    runtime_execution_ready_for_provider_continuation,
};

impl RuntimeSessionService {
    /// Reports whether host process metadata can determine if the pane primary
    /// shell is the foreground process group for its PTY.
    pub(crate) fn pane_foreground_primary_shell_state(&self, pane_id: &str) -> Option<bool> {
        let primary_pid = self.process.pane_processes.primary_pid(pane_id)?;
        let foreground_group = self
            .process
            .pane_processes
            .foreground_process_group_id(pane_id)
            .or_else(|| {
                self.process
                    .pane_foreground_process_groups
                    .get(pane_id)
                    .copied()
            })?;
        let primary_process_group = self
            .process
            .pane_processes
            .process_group_leader(pane_id)
            .and_then(|leader| u32::try_from(leader).ok())
            .unwrap_or(primary_pid);
        Some(foreground_group == primary_pid || foreground_group == primary_process_group)
    }

    /// Runs the observe agent shell transaction events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn observe_agent_shell_transaction_events(
        &mut self,
        output_pane_id: &str,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut observed = 0usize;
        let mut observed_harness_transaction_end = false;
        for event in events {
            let decoded_event;
            let event = if let TerminalOscEvent::ShellIntegration { payload } = event {
                let encoded = format!("133;{payload}");
                decoded_event = crate::terminal::parse_mez_shell_transaction_osc(&encoded);
                let Some(event) = decoded_event.as_ref() else {
                    continue;
                };
                event
            } else {
                event
            };
            match event {
                TerminalOscEvent::ShellIntegration { .. } => {}
                TerminalOscEvent::TitleChanged { .. } | TerminalOscEvent::ClipboardSet { .. } => {}
                TerminalOscEvent::ShellPromptStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellPromptEnd => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-end",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandFinished { .. } => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-command-finished",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandOutputStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_busy(
                                output_pane_id,
                                "osc133-command-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellTransactionStart {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                } => {
                    observed =
                        observed.saturating_add(self.observe_agent_shell_transaction_start(
                            output_pane_id,
                            marker,
                            turn_id,
                            agent_id,
                            pane_id,
                        )?);
                }
                TerminalOscEvent::ShellTransactionEnd {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                    exit_code,
                } => {
                    let agent_observed = self.observe_agent_shell_transaction_end(
                        output_pane_id,
                        marker,
                        turn_id,
                        agent_id,
                        pane_id,
                        *exit_code,
                    )?;
                    if agent_observed == 0 {
                        observed = observed.saturating_add(
                            self.observe_focused_shell_hook_transaction_end(
                                output_pane_id,
                                marker,
                                pane_id,
                                *exit_code,
                            )?,
                        );
                    } else {
                        observed = observed.saturating_add(agent_observed);
                        observed_harness_transaction_end = true;
                    }
                }
            }
        }
        Ok(observed)
    }

    /// Runs the pane agent turn waiting for provider or shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_agent_turn_waiting_for_provider_or_shell_dispatch(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        let turn_id = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())?;
        let turn_is_running = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return None;
        }
        if self.agent_provider_task_is_pending(turn_id) {
            return Some(turn_id.to_string());
        }
        if self.agent_provider_task_is_claimed(turn_id) {
            return None;
        }
        let execution = self.agent_turn_executions().get(turn_id)?;
        if runtime_execution_ready_for_provider_continuation(execution)
            || self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            Some(turn_id.to_string())
        } else {
            None
        }
    }

    /// Runs the queue waiting agent turn for passive readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_waiting_agent_turn_for_passive_readiness(
        &mut self,
        pane_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let Some(turn_id) = self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        else {
            return Ok(0);
        };
        if !self.queue_agent_provider_task(turn_id.clone()) {
            return Ok(0);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!("provider_task queued reason={reason}"),
        )?;
        Ok(1)
    }
}
