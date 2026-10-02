//! Runtime hook admission, execution and failure projection.
//!
//! Native basic-action events cannot queue or launch executable handlers.
//! Required pre-action protections fail closed, optional/completed handlers are
//! diagnosed without erasing effects, and pane/actual-shell integrations retain
//! their existing boundaries. Runtime action and approval ownership distinguish
//! semantic operations from legacy shell-shaped payloads before admission.

use super::{
    AuditActor, BTreeSet, DEFAULT_PTY_READ_LIMIT_BYTES, Duration, EventKind, EventVisibility,
    HookEvent, HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureDecision, HookFailureKind, HookOnFailure, Instant, MezError,
    PendingFocusedShellHookContinuation, PendingProgramHookContinuation, Result,
    RuntimeFocusedShellPaneExecutor, RuntimeHookPipelineBlock, RuntimeHookPipelineDecision,
    RuntimeSessionService, decide_hook_failure, execute_focused_shell_hook, execute_program_hook,
    focused_shell_pre_action_failed_result, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, plan_event, runtime_hook_event_for_lifecycle,
    runtime_hook_event_name, runtime_hook_target_pane_id,
};

// Configured pre-action and completion hook execution.

impl RuntimeSessionService {
    /// Determines whether this agent event belongs to a native basic-action
    /// path. Runtime action/approval ownership overrides shell-shaped payload
    /// labels used by the legacy patch adapter. UI lifecycle integrations are
    /// outside this gate; actual shell-command hooks remain intentional work.
    fn native_basic_hook_path(&self, event: HookEvent, payload: &str) -> bool {
        if !matches!(
            event,
            HookEvent::UserPromptSubmit
                | HookEvent::AgentTurnStart
                | HookEvent::AgentTurnStop
                | HookEvent::PermissionRequest
                | HookEvent::PermissionDecision
                | HookEvent::PreShellCommand
                | HookEvent::PostShellCommand
                | HookEvent::PreMcpToolUse
                | HookEvent::PostMcpToolUse
        ) {
            return false;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            return false;
        };
        let approval = value
            .get("approval_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| self.blocked_approvals().get(id));
        let pane = runtime_hook_target_pane_id(payload)
            .or_else(|| {
                value
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| self.agent_turn_ledger().turn(id))
                    .map(|turn| turn.pane_id.clone())
            })
            .or_else(|| approval.map(|approval| approval.pane_id.clone()));
        let Some(pane) = pane else {
            return false;
        };
        if self.effective_agent_shell_mode_for_pane(&pane)
            != crate::runtime::config::ShellMode::Native
        {
            return false;
        }
        let action_kind = value
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .zip(value.get("action_id").and_then(serde_json::Value::as_str))
            .and_then(|(turn, id)| {
                self.agent_turn_executions()
                    .get(turn)
                    .and_then(|execution| execution.response.action_batch.as_ref())
                    .and_then(|batch| batch.actions.iter().find(|action| action.id == id))
                    .map(|action| action.action_type())
            })
            .or_else(|| approval.map(|approval| approval.action_kind.as_str()))
            .or_else(|| {
                value
                    .get("semantic_action_type")
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| value.get("action_type").and_then(serde_json::Value::as_str));
        action_kind != Some("shell_command")
    }

    /// Emits a bounded incompatibility diagnostic without running or queueing
    /// an executable handler. Completed effects are never rolled back.
    fn record_native_hook_incompatibility(&mut self, plan: &HookExecutionPlan) -> Result<()> {
        self.append_lifecycle_event(EventKind::HookFailed, serde_json::json!({
            "hook_id": plan.hook_id, "hook_event": runtime_hook_event_name(plan.event),
            "failure_kind": "native_process_incompatible", "retryable": false,
            "message": "executable hook unavailable on native basic-action path; no handler ran",
        }).to_string())?;
        if let Some(pane) = plan.target_pane_id.as_deref() {
            self.append_agent_status_text_to_terminal_buffer(
                pane,
                &format!(
                    "agent: executable hook `{}` unavailable on native basic-action path",
                    plan.hook_id
                ),
            )?;
        }
        Ok(())
    }

    /// Runs the append primary lifecycle event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_primary_lifecycle_event(
        &mut self,
        kind: EventKind,
        payload: String,
    ) -> Result<()> {
        if let Some(event_log) = self.control.event_log_mut() {
            event_log.append(
                kind,
                Some(self.session.id.to_string()),
                EventVisibility::AllPrimaries,
                payload.clone(),
            )?;
        }
        if let Some(hook_event) = runtime_hook_event_for_lifecycle(kind, &payload) {
            self.run_configured_completed_hooks(hook_event, &payload)?;
        }
        Ok(())
    }

    /// Appends one event visible only to an exact attached primary client.
    pub(super) fn append_primary_client_event(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        kind: EventKind,
        payload: String,
    ) -> Result<()> {
        if let Some(event_log) = self.control.event_log_mut() {
            event_log.append(
                kind,
                Some(self.session.id.to_string()),
                EventVisibility::PrimaryClient(client_id.clone()),
                payload,
            )?;
        }
        Ok(())
    }

    /// Runs the run configured completed hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn run_configured_completed_hooks(
        &mut self,
        event: HookEvent,
        event_payload_json: &str,
    ) -> Result<()> {
        if self.integration.hook_definitions().is_empty() {
            return Ok(());
        }
        let event_plan = plan_event(
            self.integration.hook_definitions(),
            event,
            event_payload_json,
        )?;
        for mut plan in event_plan.plans {
            plan.target_pane_id = runtime_hook_target_pane_id(event_payload_json);
            if self.native_basic_hook_path(event, event_payload_json) {
                self.record_native_hook_incompatibility(&plan)?;
                continue;
            }
            if plan.run_in_focused_shell {
                let _ = self
                    .integration
                    .focused_shell_hook_queue_mut()
                    .enqueue(plan)?;
                continue;
            }
            self.append_program_hook_start_audit(&plan)?;
            if self.persistence.hook_uses_adapter() {
                self.defer_program_hook(plan, true, None);
                continue;
            }
            let result = match execute_program_hook(&plan) {
                Ok(result) => result,
                Err(error) => HookExecutionResult {
                    hook_id: plan.hook_id.clone(),
                    event: plan.event,
                    status: HookExecutionStatus::Failed,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    failure: Some(HookFailure {
                        hook_id: plan.hook_id.clone(),
                        event: plan.event,
                        kind: HookFailureKind::Spawn,
                        message: error.to_string(),
                        retryable: false,
                    }),
                },
            };
            self.append_program_hook_audit(&plan, &result)?;
            let _ = self.record_hook_result(&plan, &result, true)?;
        }
        Ok(())
    }

    /// Runs the run configured pre action hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn run_configured_pre_action_hooks(
        &mut self,
        event: HookEvent,
        event_payload_json: &str,
    ) -> Result<Option<RuntimeHookPipelineBlock>> {
        match self.run_configured_pre_action_hooks_with_continuation(
            event,
            event_payload_json,
            None,
        )? {
            RuntimeHookPipelineDecision::Continue => Ok(None),
            RuntimeHookPipelineDecision::Block(block) => Ok(Some(block)),
            RuntimeHookPipelineDecision::Pending => Err(MezError::invalid_state(
                "pre-action hook pipeline returned a pending decision without a continuation",
            )),
        }
    }

    /// Runs the run configured pre action hooks with continuation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn run_configured_pre_action_hooks_with_continuation(
        &mut self,
        event: HookEvent,
        event_payload_json: &str,
        continuation: Option<PendingFocusedShellHookContinuation>,
    ) -> Result<RuntimeHookPipelineDecision> {
        if self.integration.hook_definitions().is_empty() {
            return Ok(RuntimeHookPipelineDecision::Continue);
        }
        let event_plan = plan_event(
            self.integration.hook_definitions(),
            event,
            event_payload_json,
        )?;
        for mut plan in event_plan.plans {
            plan.target_pane_id = runtime_hook_target_pane_id(event_payload_json);
            if self.native_basic_hook_path(event, event_payload_json) {
                let required = self
                    .integration
                    .hook_definitions()
                    .iter()
                    .any(|definition| definition.id == plan.hook_id && definition.required);
                self.record_native_hook_incompatibility(&plan)?;
                if required || plan.on_failure == HookOnFailure::Block {
                    return Ok(RuntimeHookPipelineDecision::Block(RuntimeHookPipelineBlock {
                        hook_id: plan.hook_id, event,
                        failure_kind: HookFailureKind::ShellUnavailable,
                        message: "required executable gate is incompatible with native basic-action execution; no handler ran".to_string(),
                    }));
                }
                continue;
            }
            if let Some(continuation) = continuation.as_ref()
                && self.agent_pre_shell_hook_completed(continuation, &plan.hook_id)
            {
                continue;
            }
            if plan.run_in_focused_shell {
                if plan.on_failure == HookOnFailure::Block {
                    let result = self
                        .execute_blocking_focused_shell_pre_action_hook_with_continuation(
                            &plan,
                            continuation.clone(),
                        )?;
                    if result.status == HookExecutionStatus::Queued {
                        return Ok(RuntimeHookPipelineDecision::Pending);
                    }
                    let decision = self.record_hook_result(&plan, &result, false)?;
                    if decision != HookFailureDecision::Block
                        && let Some(continuation) = continuation.as_ref()
                    {
                        self.record_agent_pre_shell_hook_completed(continuation, &plan.hook_id);
                    }
                    if decision == HookFailureDecision::Block {
                        return Ok(RuntimeHookPipelineDecision::Block(
                            RuntimeHookPipelineBlock::from_result(&result),
                        ));
                    }
                    continue;
                }
                let _ = self
                    .integration
                    .focused_shell_hook_queue_mut()
                    .enqueue(plan)?;
                continue;
            }
            self.append_program_hook_start_audit(&plan)?;
            if self.persistence.hook_uses_adapter() {
                if plan.on_failure == HookOnFailure::Block
                    && let Some(continuation) = continuation.as_ref()
                {
                    let pending =
                        PendingProgramHookContinuation::new(continuation, plan.hook_id.clone());
                    if self
                        .integration
                        .pending_program_hook_continuations_mut()
                        .insert(pending.clone())
                    {
                        self.defer_program_hook(plan, false, Some(pending));
                    }
                    return Ok(RuntimeHookPipelineDecision::Pending);
                }
                if plan.on_failure == HookOnFailure::Block {
                    return Err(MezError::invalid_state(format!(
                        "blocking program hook `{}` has no async continuation for event {}",
                        plan.hook_id,
                        runtime_hook_event_name(plan.event)
                    )));
                }
                if plan.on_failure != HookOnFailure::Block {
                    self.defer_program_hook(plan, false, None);
                    continue;
                }
            }
            let result = match execute_program_hook(&plan) {
                Ok(result) => result,
                Err(error) => HookExecutionResult {
                    hook_id: plan.hook_id.clone(),
                    event: plan.event,
                    status: HookExecutionStatus::Failed,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    failure: Some(HookFailure {
                        hook_id: plan.hook_id.clone(),
                        event: plan.event,
                        kind: HookFailureKind::Spawn,
                        message: error.to_string(),
                        retryable: false,
                    }),
                },
            };
            self.append_program_hook_audit(&plan, &result)?;
            let decision = self.record_hook_result(&plan, &result, false)?;
            if decision != HookFailureDecision::Block
                && let Some(continuation) = continuation.as_ref()
            {
                self.record_agent_pre_shell_hook_completed(continuation, &plan.hook_id);
            }
            if decision == HookFailureDecision::Block {
                return Ok(RuntimeHookPipelineDecision::Block(
                    RuntimeHookPipelineBlock::from_result(&result),
                ));
            }
        }
        Ok(RuntimeHookPipelineDecision::Continue)
    }

    /// Runs the execute blocking focused shell pre action hook with continuation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_blocking_focused_shell_pre_action_hook_with_continuation(
        &mut self,
        plan: &HookExecutionPlan,
        continuation: Option<PendingFocusedShellHookContinuation>,
    ) -> Result<HookExecutionResult> {
        let Some(primary_client_id) = self.session.layout_owner_client_id().cloned() else {
            return Ok(focused_shell_pre_action_failed_result(
                plan,
                HookFailureKind::ShellUnavailable,
                "blocking focused-shell hook requires an attached primary client",
                true,
            ));
        };
        let target_async_owned = if let Some(target_pane_id) = plan.target_pane_id.as_deref() {
            self.pane_process_is_adapter_owned(target_pane_id)
        } else {
            self.active_window_pane_descriptor(None)
                .map(|descriptor| self.pane_process_is_adapter_owned(descriptor.pane_id.as_str()))
                .unwrap_or(false)
        };
        if target_async_owned && continuation.is_none() {
            return Ok(focused_shell_pre_action_failed_result(
                plan,
                HookFailureKind::ShellUnavailable,
                "blocking focused-shell hook cannot wait for an async-owned pane without a continuation",
                true,
            ));
        }
        let transaction_start = self
            .integration
            .focused_shell_hook_transactions()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut executor = RuntimeFocusedShellPaneExecutor {
            service: self,
            continuation,
        };
        let initial = execute_focused_shell_hook(plan, &mut executor)?;
        executor
            .service
            .append_focused_shell_dispatch_audit(&primary_client_id, &initial)?;
        if initial.status != HookExecutionStatus::Queued {
            return Ok(initial);
        }
        if target_async_owned {
            return Ok(initial);
        }

        let marker = executor
            .service
            .integration
            .focused_shell_hook_transactions()
            .iter()
            .find(|(marker, pending)| {
                !transaction_start.contains(*marker)
                    && pending.plan.hook_id == plan.hook_id
                    && pending.plan.event == plan.event
            })
            .map(|(marker, _)| marker.clone())
            .ok_or_else(|| {
                MezError::invalid_state("focused-shell pre-action hook did not register a marker")
            })?;
        let pane_id = executor
            .service
            .integration
            .focused_shell_hook_transactions()
            .get(marker.as_str())
            .map(|pending| pending.pane_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("focused-shell pre-action hook marker lost dispatch state")
            })?;
        let deadline = Instant::now() + Duration::from_millis(plan.timeout_ms);
        loop {
            let activity_sequence = executor
                .service
                .pane_process_output_activity_sequence(pane_id.as_str());
            executor
                .service
                .poll_pane_outputs(DEFAULT_PTY_READ_LIMIT_BYTES)?;
            if !executor
                .service
                .integration
                .focused_shell_hook_transactions()
                .contains_key(&marker)
            {
                let result = executor
                    .service
                    .integration
                    .focused_shell_hook_results()
                    .iter()
                    .rev()
                    .find(|result| result.hook_id == plan.hook_id && result.event == plan.event)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "focused-shell pre-action hook completed without a result",
                        )
                    })?;
                return Ok(result);
            }
            if Instant::now() >= deadline {
                executor
                    .service
                    .integration
                    .focused_shell_hook_transactions_mut()
                    .remove(&marker);
                let result = focused_shell_pre_action_timeout_result(plan);
                executor
                    .service
                    .append_focused_shell_dispatch_audit(&primary_client_id, &result)?;
                executor
                    .service
                    .push_focused_shell_hook_result(result.clone());
                return Ok(result);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Some(activity_sequence) = activity_sequence {
                let _ = executor
                    .service
                    .wait_for_pane_process_output_activity_after(
                        pane_id.as_str(),
                        activity_sequence,
                        remaining,
                    );
            } else {
                executor
                    .service
                    .integration
                    .focused_shell_hook_transactions_mut()
                    .remove(&marker);
                let result = focused_shell_pre_action_failed_result(
                    plan,
                    HookFailureKind::ShellUnavailable,
                    "focused-shell pre-action hook lost its pane output activity source",
                    true,
                );
                executor
                    .service
                    .append_focused_shell_dispatch_audit(&primary_client_id, &result)?;
                executor
                    .service
                    .push_focused_shell_hook_result(result.clone());
                return Ok(result);
            }
        }
    }

    /// Emits an audit record recording that a program hook started execution
    /// before its child process is spawned. This produces a "start" audit entry
    /// regardless of whether the hook succeeds, fails, or times out.
    pub(super) fn append_program_hook_start_audit(
        &mut self,
        plan: &HookExecutionPlan,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let actor = AuditActor {
            kind: "runtime".to_string(),
            id: "lifecycle".to_string(),
        };
        let record = hook_execution_audit_record(
            plan,
            &self.session.id.to_string(),
            actor,
            "runtime_lifecycle_program_hook_start",
            &HookExecutionResult {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                status: HookExecutionStatus::Queued,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_bytes: 0,
                stderr_bytes: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                failure: None,
            },
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the append program hook audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_program_hook_audit(
        &mut self,
        plan: &HookExecutionPlan,
        result: &HookExecutionResult,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let actor = AuditActor {
            kind: "runtime".to_string(),
            id: "lifecycle".to_string(),
        };
        let record = hook_execution_audit_record(
            plan,
            &self.session.id.to_string(),
            actor,
            "runtime_lifecycle_program_hook",
            result,
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the record hook result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_hook_result(
        &mut self,
        plan: &HookExecutionPlan,
        result: &HookExecutionResult,
        triggering_event_completed: bool,
    ) -> Result<HookFailureDecision> {
        let Some(failure) = result.failure.as_ref() else {
            return Ok(HookFailureDecision::Ignore);
        };
        self.append_lifecycle_event(
            EventKind::HookFailed,
            format!(
                r#"{{"hook_id":"{}","hook_event":"{}","failure_kind":"{:?}","retryable":{},"on_failure":"{:?}","message":"{}"}}"#,
                json_escape(&failure.hook_id),
                runtime_hook_event_name(failure.event),
                failure.kind,
                failure.retryable,
                plan.on_failure,
                json_escape(&failure.message)
            ),
        )?;
        Ok(decide_hook_failure(
            plan,
            failure,
            triggering_event_completed,
        ))
    }
}
