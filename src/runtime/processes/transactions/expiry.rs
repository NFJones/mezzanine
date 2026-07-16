//! Timeout expiry for shell transactions and focused hooks.

use super::{
    ActionStatus, AuditActor, EventKind, PaneReadinessState, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeHookPipelineBlock, RuntimeSessionService,
    RuntimeShellTransactionActionFailure, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, local_action_plan, runtime_hook_event_name,
    runtime_pane_readiness_state_name, runtime_shell_transaction_effective_timeout_ms,
};

impl RuntimeSessionService {
    /// Expires live Mezzanine-owned shell transactions whose runtime timeout has
    /// elapsed without observing their expected terminal marker.
    pub(in crate::runtime) fn expire_timed_out_shell_transactions(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .process
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= timeout_ms)
                    .then(|| (marker.clone(), transaction.clone(), timeout_ms, elapsed_ms))
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for (marker, transaction, timeout_ms, elapsed_ms) in expired {
            if self
                .process
                .running_shell_transactions
                .remove(&marker)
                .is_none()
            {
                continue;
            }
            self.clear_shell_transaction_protocol_state(&marker);
            expired_count = expired_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.expire_agent_action_shell_transaction(
                        &marker,
                        transaction,
                        &action_id,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.expire_readiness_probe_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.expire_bootstrap_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
            }
        }
        Ok(expired_count)
    }

    /// Runs the expire timed out focused shell hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn expire_timed_out_focused_shell_hooks(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .integration
            .focused_shell_hook_transactions()
            .iter()
            .filter_map(|(marker, transaction)| {
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= transaction.timeout_ms).then(|| marker.clone())
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for marker in expired {
            let Some(pending) = self
                .integration
                .focused_shell_hook_transactions_mut()
                .remove(&marker)
            else {
                continue;
            };
            expired_count = expired_count.saturating_add(1);
            let result = focused_shell_pre_action_timeout_result(&pending.plan);
            if let Some(audit_log) = self.persistence.audit_log_mut() {
                let record = hook_execution_audit_record(
                    &pending.plan,
                    self.session.id.as_str(),
                    AuditActor {
                        kind: "runtime".to_string(),
                        id: "focused-shell-hook-timeout".to_string(),
                    },
                    "runtime_focused_shell_timeout",
                    &result,
                )
                .with_pane_id(pending.pane_id.clone());
                let _ = audit_log.append(record)?;
            }
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","marker":"{}","failure_kind":"Timeout"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(&pending.pane_id),
                    json_escape(&marker)
                ),
            )?;
            if let Some(continuation) = pending.continuation.as_ref() {
                let decision = self.record_hook_result(&pending.plan, &result, false)?;
                if decision == crate::hooks::HookFailureDecision::Block {
                    let block = RuntimeHookPipelineBlock::from_result(&result);
                    let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
                } else {
                    self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                    let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
                }
            }
            self.push_focused_shell_hook_result(result);
        }
        Ok(expired_count)
    }

    /// Fails a timed-out agent shell action and interrupts the pane command when
    /// the runtime can still reach the pane process.
    pub(super) fn expire_agent_action_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=shell_transaction_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let message = format!("shell command timed out after {timeout_ms} ms");
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "exit_code": null,
            "signal": null,
            "timed_out": true,
            "timeout_ms": timeout_ms,
            "elapsed_ms": elapsed_ms,
            "combined_output_bytes": transaction.observed_output_bytes,
            "combined_output_preview": transaction.observed_output_preview,
            "boundary_state": "timeout",
            "output_truncated": transaction.observed_output_truncated
        });
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::TimedOut,
                code: "shell_timeout".to_string(),
                message,
                sent_to_pane: true,
                terminal_observation,
                trace_reason: "shell_transaction_timeout".to_string(),
            },
        )?;
        Ok(())
    }

    /// Settles a readiness probe timeout and fails the pending shell action that
    /// depended on the probe, when such an action is still present.
    pub(super) fn expire_readiness_probe_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        if !self
            .process
            .pane_readiness_overrides
            .clear_pending_probe_if_matches(&transaction.pane_id, marker)
        {
            self.append_agent_trace_turn_event(
                &transaction.pane_id,
                &transaction.turn_id,
                &format!("readiness_probe ignored reason=stale_timeout marker={marker}"),
            )?;
            return Ok(());
        }
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let message =
                format!("shell readiness probe timed out after {timeout_ms} ms before dispatch");
            let terminal_observation = serde_json::json!({
                "source": "pty",
                "stream": "pty_combined",
                "marker": marker,
                "exit_code": null,
                "signal": null,
                "timed_out": true,
                "timeout_ms": timeout_ms,
                "elapsed_ms": elapsed_ms,
                "combined_output_bytes": transaction.observed_output_bytes,
                "combined_output_preview": transaction.observed_output_preview,
                "boundary_state": "readiness-probe-timeout",
                "output_truncated": transaction.observed_output_truncated
            });
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::TimedOut,
                    code: "readiness_probe_timeout".to_string(),
                    message,
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_timeout".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe timed out after {timeout_ms} ms"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"timed_out","marker":"{}","timeout_ms":{},"elapsed_ms":{}}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    timeout_ms,
                    elapsed_ms
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a timed-out bootstrap transaction as a degraded one-shot attempt
    /// instead of retrying the hidden bootstrap wrapper indefinitely.
    pub(super) fn expire_bootstrap_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.process
            .pane_bootstrap_pending
            .remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"timed_out","marker":"{}","previous_state":"{}","state":"degraded","timeout_ms":{},"elapsed_ms":{},"output_bytes":{},"output_truncated":{}}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                timeout_ms,
                elapsed_ms,
                transaction.observed_output_bytes,
                transaction.observed_output_truncated
            ),
        )?;
        Ok(())
    }

    /// Sends an interrupt to the pane shell for a timed-out transaction while
    /// tolerating panes that have already exited.
    pub(in crate::runtime::processes) fn interrupt_shell_transaction_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<()> {
        match self.write_runtime_pane_input(pane_id, b"\x03") {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Returns the first still-running shell action that has not produced a
    /// terminal action result for the given turn.
    pub(in crate::runtime::processes) fn pending_shell_action_id_for_turn(
        &self,
        turn_id: &str,
    ) -> Option<String> {
        let execution = self.agent_turn_executions().get(turn_id)?;
        let batch = execution.response.action_batch.as_ref()?;
        execution
            .action_results
            .iter()
            .find(|result| {
                result.status == ActionStatus::Running
                    && batch
                        .actions
                        .iter()
                        .find(|action| action.id == result.action_id)
                        .and_then(|action| local_action_plan(action).ok().flatten())
                        .is_some()
            })
            .map(|result| result.action_id.clone())
    }
}
