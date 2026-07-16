//! Protocol violations and pane input write-failure settlement.

use super::*;

impl RuntimeSessionService {
    /// Fails one live shell transaction because its wrapper marker protocol
    /// reached an impossible state.
    pub(in crate::runtime::processes) fn fail_shell_transaction_protocol_violation(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        boundary_state: &'static str,
        message: impl Into<String>,
    ) -> Result<usize> {
        let message = message.into();
        self.runtime_metrics
            .record_shell_transaction_protocol_violation();
        self.process.running_shell_transactions.remove(marker);
        self.clear_shell_transaction_protocol_state(marker);
        self.interrupt_shell_transaction_pane_if_live(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=shell_protocol_violation marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        match transaction.kind.clone() {
            RunningShellTransactionKind::AgentAction { action_id } => {
                let terminal_observation = shell_transaction_protocol_violation_observation(
                    marker,
                    &transaction,
                    boundary_state,
                    &message,
                );
                self.fail_running_shell_transaction_action(
                    &transaction,
                    marker,
                    RuntimeShellTransactionActionFailure {
                        action_id,
                        status: ActionStatus::Failed,
                        code: "shell_protocol_violation".to_string(),
                        message,
                        sent_to_pane: true,
                        terminal_observation,
                        trace_reason: "shell_protocol_violation".to_string(),
                    },
                )
            }
            RunningShellTransactionKind::ReadinessProbe => {
                if !self
                    .process
                    .pane_readiness_overrides
                    .clear_pending_probe_if_matches(&transaction.pane_id, marker)
                {
                    self.append_agent_trace_turn_event(
                        &transaction.pane_id,
                        &transaction.turn_id,
                        &format!(
                            "readiness_probe ignored reason=stale_protocol_violation marker={marker}"
                        ),
                    )?;
                    return Ok(0);
                }
                if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id)
                {
                    let terminal_observation = shell_transaction_protocol_violation_observation(
                        marker,
                        &transaction,
                        boundary_state,
                        &message,
                    );
                    self.fail_running_shell_transaction_action(
                        &transaction,
                        marker,
                        RuntimeShellTransactionActionFailure {
                            action_id,
                            status: ActionStatus::Failed,
                            code: "shell_protocol_violation".to_string(),
                            message,
                            sent_to_pane: false,
                            terminal_observation,
                            trace_reason: "shell_protocol_violation".to_string(),
                        },
                    )
                } else {
                    self.append_agent_error_text_to_terminal_buffer(
                        &transaction.pane_id,
                        &format!("agent: shell readiness probe protocol violation: {message}"),
                    )?;
                    Ok(1)
                }
            }
            RunningShellTransactionKind::Bootstrap => {
                self.process
                    .pane_bootstrap_pending
                    .remove(&transaction.pane_id);
                self.append_agent_error_text_to_terminal_buffer(
                    &transaction.pane_id,
                    &format!("agent: shell bootstrap protocol violation: {message}"),
                )?;
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"protocol_violation","marker":"{}","state":"degraded","error":"{}"}}"#,
                        json_escape(&transaction.pane_id),
                        json_escape(marker),
                        json_escape(&message)
                    ),
                )?;
                Ok(1)
            }
        }
    }

    /// Fails live shell transactions for a pane whose PTY input write failed.
    pub(in crate::runtime::processes) fn fail_shell_transactions_for_pane_write_failure(
        &mut self,
        pane_id: &str,
        error: &str,
    ) -> Result<usize> {
        let failed_transactions = self
            .process
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        let mut failed_count = 0usize;
        for (marker, transaction) in failed_transactions {
            if self
                .process
                .running_shell_transactions
                .remove(&marker)
                .is_none()
            {
                continue;
            }
            self.clear_shell_transaction_protocol_state(&marker);
            failed_count = failed_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.fail_agent_action_for_pane_write_failure(
                        &marker,
                        transaction,
                        &action_id,
                        error,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.fail_readiness_probe_for_pane_write_failure(&marker, transaction, error)?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.fail_bootstrap_for_pane_write_failure(&marker, transaction, error)?;
                }
            }
        }
        Ok(failed_count)
    }

    /// Fails one running agent action when its pane input cannot be written.
    pub(super) fn fail_agent_action_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        error: &str,
    ) -> Result<()> {
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let terminal_observation = pane_write_failure_terminal_observation(
            marker,
            &transaction,
            "pane-input-write-failed",
            error,
        );
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::Failed,
                code: "pane_input_write_failed".to_string(),
                message: format!("pane input write failed while sending shell action: {error}"),
                sent_to_pane: false,
                terminal_observation,
                trace_reason: "pane_input_write_failed".to_string(),
            },
        )?;
        Ok(())
    }

    /// Fails a pending shell action when its readiness probe cannot be written.
    pub(super) fn fail_readiness_probe_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        if !self
            .process
            .pane_readiness_overrides
            .clear_pending_probe_if_matches(&transaction.pane_id, marker)
        {
            self.append_agent_trace_turn_event(
                &transaction.pane_id,
                &transaction.turn_id,
                &format!("readiness_probe ignored reason=stale_write_failure marker={marker}"),
            )?;
            return Ok(());
        }
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let terminal_observation = pane_write_failure_terminal_observation(
                marker,
                &transaction,
                "readiness-probe-pane-input-write-failed",
                error,
            );
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::Failed,
                    code: "pane_input_write_failed".to_string(),
                    message: format!(
                        "pane input write failed while sending shell readiness probe: {error}"
                    ),
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_pane_input_write_failed".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe write failed: {error}"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"write_failed","marker":"{}","error":"{}"}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    json_escape(error)
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a bootstrap transaction degraded when its pane input cannot write.
    pub(super) fn fail_bootstrap_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        self.process
            .pane_bootstrap_pending
            .remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"write_failed","marker":"{}","previous_state":"{}","state":"degraded","error":"{}"}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                json_escape(error)
            ),
        )?;
        Ok(())
    }
}
