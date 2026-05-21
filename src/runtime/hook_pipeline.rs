//! Runtime Hook Pipeline implementation.
//!
//! This module owns the runtime hook pipeline boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuditActor, BTreeSet, DEFAULT_PTY_READ_LIMIT_BYTES, Duration, EventKind, EventVisibility,
    HookEvent, HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureDecision, HookFailureKind, HookOnFailure, Instant, MezError,
    PendingFocusedShellHookContinuation, Result, RuntimeAgentPreShellHookCompletion,
    RuntimeFocusedShellPaneExecutor, RuntimeHookPipelineBlock, RuntimeHookPipelineDecision,
    RuntimeSessionService, decide_hook_failure, execute_focused_shell_hook, execute_program_hook,
    focused_shell_pre_action_failed_result, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, plan_event, runtime_hook_event_for_lifecycle,
    runtime_hook_event_name, runtime_hook_target_pane_id,
};

// Configured pre-action and completion hook execution.

impl RuntimeSessionService {
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
        if let Some(event_log) = &mut self.event_log {
            event_log.append(
                kind,
                Some(self.session.id.to_string()),
                EventVisibility::PrimaryOnly,
                payload.clone(),
            )?;
        }
        if let Some(hook_event) = runtime_hook_event_for_lifecycle(kind, &payload) {
            self.run_configured_completed_hooks(hook_event, &payload)?;
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
        if self.hook_definitions.is_empty() {
            return Ok(());
        }
        let event_plan = plan_event(&self.hook_definitions, event, event_payload_json)?;
        for mut plan in event_plan.plans {
            plan.target_pane_id = runtime_hook_target_pane_id(event_payload_json);
            if plan.run_in_focused_shell {
                let _ = self.focused_shell_hooks.enqueue(plan)?;
                continue;
            }
            self.append_program_hook_start_audit(&plan)?;
            if self.defer_program_hooks {
                self.defer_program_hook(plan, true);
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
        if self.hook_definitions.is_empty() {
            return Ok(RuntimeHookPipelineDecision::Continue);
        }
        let event_plan = plan_event(&self.hook_definitions, event, event_payload_json)?;
        for mut plan in event_plan.plans {
            plan.target_pane_id = runtime_hook_target_pane_id(event_payload_json);
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
                let _ = self.focused_shell_hooks.enqueue(plan)?;
                continue;
            }
            self.append_program_hook_start_audit(&plan)?;
            if self.defer_program_hooks && plan.on_failure != HookOnFailure::Block {
                self.defer_program_hook(plan, false);
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
        let Some(primary_client_id) = self.session.primary_client_id().cloned() else {
            return Ok(focused_shell_pre_action_failed_result(
                plan,
                HookFailureKind::ShellUnavailable,
                "blocking focused-shell hook requires an attached primary client",
                true,
            ));
        };
        let target_async_owned = if let Some(target_pane_id) = plan.target_pane_id.as_deref() {
            self.pane_process_is_async_owned(target_pane_id)
        } else {
            self.active_window_pane_descriptor(None)
                .map(|descriptor| self.pane_process_is_async_owned(descriptor.pane_id.as_str()))
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
            .focused_shell_hook_transactions
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut executor = RuntimeFocusedShellPaneExecutor {
            service: self,
            primary_client_id: primary_client_id.clone(),
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
            .focused_shell_hook_transactions
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
            .focused_shell_hook_transactions
            .get(marker.as_str())
            .map(|pending| pending.pane_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("focused-shell pre-action hook marker lost dispatch state")
            })?;
        let deadline = Instant::now() + Duration::from_millis(plan.timeout_ms);
        loop {
            let activity_sequence = executor
                .service
                .pane_processes()
                .output_activity_sequence(pane_id.as_str());
            executor
                .service
                .poll_pane_outputs(DEFAULT_PTY_READ_LIMIT_BYTES)?;
            if !executor
                .service
                .focused_shell_hook_transactions
                .contains_key(&marker)
            {
                let result = executor
                    .service
                    .focused_shell_hook_results
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
                    .focused_shell_hook_transactions
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
                    .pane_processes()
                    .wait_for_output_activity_after(pane_id.as_str(), activity_sequence, remaining);
            } else {
                executor
                    .service
                    .focused_shell_hook_transactions
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

    /// Runs the agent pre shell hook completed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_pre_shell_hook_completed(
        &self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) -> bool {
        self.agent_pre_shell_hook_completions
            .contains(&RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                hook_id: hook_id.to_string(),
            })
    }

    /// Runs the record agent pre shell hook completed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_agent_pre_shell_hook_completed(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) {
        self.agent_pre_shell_hook_completions
            .insert(RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                hook_id: hook_id.to_string(),
            });
    }

    /// Runs the clear agent pre shell hook completions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_agent_pre_shell_hook_completions_for_turn(&mut self, turn_id: &str) {
        self.agent_pre_shell_hook_completions
            .retain(|completion| completion.turn_id != turn_id);
    }

    /// Emits an audit record recording that a program hook started execution
    /// before its child process is spawned. This produces a "start" audit entry
    /// regardless of whether the hook succeeds, fails, or times out.
    pub(super) fn append_program_hook_start_audit(
        &mut self,
        plan: &HookExecutionPlan,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
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
        let Some(audit_log) = self.audit_log.as_mut() else {
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
