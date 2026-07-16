//! Focused-shell hook transaction completion.

use super::*;

impl RuntimeSessionService {
    /// Runs the observe focused shell hook transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_focused_shell_hook_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(pending) = self
            .integration
            .focused_shell_hook_transactions_mut()
            .remove(marker)
        else {
            return Ok(0);
        };
        if pending.pane_id != pane_id || output_pane_id != pane_id {
            return Err(MezError::invalid_state(
                "focused-shell hook marker metadata does not match runtime dispatch state",
            ));
        }
        let success = exit_code == 0;
        let result = HookExecutionResult {
            hook_id: pending.plan.hook_id.clone(),
            event: pending.plan.event,
            status: if success {
                HookExecutionStatus::Succeeded
            } else {
                HookExecutionStatus::Failed
            },
            exit_code: Some(exit_code),
            stdout: format!("focused-shell hook exited with status {exit_code}"),
            stderr: String::new(),
            failure: if success {
                None
            } else {
                Some(HookFailure {
                    hook_id: pending.plan.hook_id.clone(),
                    event: pending.plan.event,
                    kind: HookFailureKind::ExitNonZero,
                    message: "focused-shell hook exited with non-zero status".to_string(),
                    retryable: false,
                })
            },
        };
        if !success {
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","exit_code":{},"marker":"{}"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(pane_id),
                    exit_code,
                    json_escape(marker)
                ),
            )?;
        }
        if let Some(audit_log) = self.persistence.audit_log_mut() {
            let record = hook_execution_audit_record(
                &pending.plan,
                self.session.id.as_str(),
                AuditActor {
                    kind: "runtime".to_string(),
                    id: "focused-shell-hook-observer".to_string(),
                },
                "runtime_focused_shell_completion",
                &result,
            )
            .with_pane_id(pane_id.to_string());
            let _ = audit_log.append(record)?;
        }
        if let Some(continuation) = pending.continuation.as_ref() {
            let decision = self.record_hook_result(&pending.plan, &result, false)?;
            if decision == crate::hooks::HookFailureDecision::Block {
                let block = RuntimeHookPipelineBlock::from_result(&result);
                let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
            } else {
                self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                let continuation_pane_id = self
                    .agent_turn_ledger()
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == continuation.turn_id)
                    .map(|turn| turn.pane_id.clone())
                    .unwrap_or_else(|| pane_id.to_string());
                self.append_agent_trace_turn_event(
                    &continuation_pane_id,
                    &continuation.turn_id,
                    &format!(
                        "action {} pre_shell_hook {} completed status={}",
                        continuation.action_id,
                        pending.plan.hook_id,
                        runtime_hook_execution_status_name(result.status)
                    ),
                )?;
                let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
            }
        }
        self.push_focused_shell_hook_result(result);
        Ok(1)
    }
}
