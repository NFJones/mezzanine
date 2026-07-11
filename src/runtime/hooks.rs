//! Runtime Hooks implementation.
//!
//! This module owns the runtime hooks boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuditActor, AuditRecord, ClientId, DeferredProgramHook, EventKind, FocusedShellExecutor,
    FocusedShellHookDispatch, FocusedShellHookDispatchStatus, HookDefinition, HookEvent,
    HookExecutionPlan, HookExecutionResult, Result, RuntimeFocusedShellHookRun,
    RuntimeFocusedShellPaneExecutor, RuntimeSessionService, json_escape, plan_event,
    runtime_hook_execution_status_name, runtime_hook_target_pane_id,
};
use crate::runtime::{AsyncHookEvent, RuntimeSideEffect, RuntimeTransition};

// Focused shell hook queueing and lifecycle hook dispatch.

/// Defines the MAX FOCUSED SHELL HOOK RESULTS RETAINED const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const MAX_FOCUSED_SHELL_HOOK_RESULTS_RETAINED: usize = 256;

impl RuntimeSessionService {
    /// Applies one hook-worker completion through the transport-neutral transition contract.
    pub(crate) fn apply_hook_transition(
        &mut self,
        event: AsyncHookEvent,
    ) -> Result<RuntimeTransition> {
        let applied = match event {
            AsyncHookEvent::ProgramCompleted {
                plan,
                result,
                triggering_event_completed,
            } => {
                self.apply_async_program_hook_result(*plan, *result, triggering_event_completed)?
            }
            AsyncHookEvent::Completed {
                hook_id,
                exit_code,
                output_preview,
            } => self.apply_async_hook_completed_event(hook_id, exit_code, output_preview)?,
            AsyncHookEvent::Failed { hook_id, error } => {
                self.apply_async_hook_failed_event(hook_id, error)?
            }
        };
        Ok(RuntimeTransition {
            applied,
            side_effects: Vec::new(),
        })
    }

    /// Enables or disables deferral of non-blocking program hooks.
    ///
    /// The synchronous compatibility runtime leaves this disabled so hooks run
    /// inline. The async actor enables it and drains queued hooks into a
    /// Drains program hook executions queued for an async hook worker.
    pub(crate) fn drain_deferred_program_hooks(&mut self) -> Vec<DeferredProgramHook> {
        std::mem::take(&mut self.deferred_program_hooks)
    }

    /// Drains queued program hooks through the runtime transition contract.
    pub(crate) fn drain_program_hook_transition(&mut self) -> RuntimeTransition {
        let side_effects = self
            .drain_deferred_program_hooks()
            .into_iter()
            .map(|hook| RuntimeSideEffect::RunProgramHook {
                plan: Box::new(hook.plan),
                triggering_event_completed: hook.triggering_event_completed,
            })
            .collect();
        RuntimeTransition {
            applied: false,
            side_effects,
        }
    }

    /// Runs the focused shell hook queue len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn focused_shell_hook_queue_len(&self) -> usize {
        self.focused_shell_hooks.len()
    }

    /// Runs the focused shell hook results operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn focused_shell_hook_results(&self) -> &[HookExecutionResult] {
        &self.focused_shell_hook_results
    }

    /// Runs the focused shell available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn focused_shell_available(&self) -> bool {
        let Ok(descriptor) = self.active_window_pane_descriptor(None) else {
            return false;
        };
        self.primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .is_some()
    }

    /// Runs the focused shell available for plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn focused_shell_available_for_plan(
        &self,
        plan: Option<&HookExecutionPlan>,
    ) -> bool {
        if let Some(target_pane_id) = plan.and_then(|plan| plan.target_pane_id.as_deref()) {
            return self
                .primary_pid_for_live_pane_process(target_pane_id)
                .is_some();
        }
        self.focused_shell_available()
    }

    /// Runs the enqueue focused shell hook operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enqueue_focused_shell_hook(&mut self, plan: HookExecutionPlan) -> Result<u64> {
        self.require_live()?;
        self.focused_shell_hooks.enqueue(plan)
    }

    /// Runs the defer program hook operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn defer_program_hook(
        &mut self,
        plan: HookExecutionPlan,
        triggering_event_completed: bool,
    ) {
        self.deferred_program_hooks.push(DeferredProgramHook {
            plan,
            triggering_event_completed,
        });
    }

    /// Runs the push focused shell hook result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn push_focused_shell_hook_result(&mut self, result: HookExecutionResult) {
        self.focused_shell_hook_results.push(result);
        if self.focused_shell_hook_results.len() > MAX_FOCUSED_SHELL_HOOK_RESULTS_RETAINED {
            let overflow = self
                .focused_shell_hook_results
                .len()
                .saturating_sub(MAX_FOCUSED_SHELL_HOOK_RESULTS_RETAINED);
            self.focused_shell_hook_results.drain(0..overflow);
        }
    }

    /// Runs the dispatch focused shell hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn dispatch_focused_shell_hooks(
        &mut self,
        executor: &mut impl FocusedShellExecutor,
    ) -> Result<Vec<FocusedShellHookDispatch>> {
        self.require_live()?;
        let mut dispatches = Vec::new();
        loop {
            let shell_available = self.focused_shell_available();
            let Some(dispatch) = self
                .focused_shell_hooks
                .dispatch_next(shell_available, executor)?
            else {
                break;
            };
            let blocked = dispatch.status == FocusedShellHookDispatchStatus::BlockedOnShell;
            dispatches.push(dispatch);
            if blocked {
                break;
            }
        }
        Ok(dispatches)
    }

    /// Applies an async hook-worker completion event through actor-owned
    /// runtime ingress.
    ///
    /// The current async hook event shape carries completion diagnostics rather
    /// than enough state to resume a blocked hook pipeline. Recording the event
    /// as a session-visible diagnostic still makes completion ordered,
    /// replayable, and available to event subscribers while the hook worker
    /// ownership model is migrated.
    pub fn apply_async_hook_completed_event(
        &mut self,
        hook_id: impl AsRef<str>,
        exit_code: Option<i32>,
        output_preview: impl AsRef<str>,
    ) -> Result<bool> {
        let exit_code_json = exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_string());
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"async_hook":"completed","hook_id":"{}","exit_code":{},"output_preview":"{}"}}"#,
                json_escape(hook_id.as_ref()),
                exit_code_json,
                json_escape(output_preview.as_ref())
            ),
        )?;
        Ok(true)
    }

    /// Applies an async hook-worker failure event through actor-owned runtime
    /// ingress.
    ///
    /// Failures are recorded as diagnostics so users and tests can distinguish
    /// worker failure from hook-program non-zero completion while later phases
    /// move hook scheduling and blocking decisions out of compatibility paths.
    pub fn apply_async_hook_failed_event(
        &mut self,
        hook_id: impl AsRef<str>,
        error: impl AsRef<str>,
    ) -> Result<bool> {
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"async_hook":"failed","hook_id":"{}","error":"{}"}}"#,
                json_escape(hook_id.as_ref()),
                json_escape(error.as_ref())
            ),
        )?;
        Ok(true)
    }

    /// Applies a completed async program-hook result to actor-owned runtime state.
    pub fn apply_async_program_hook_result(
        &mut self,
        plan: HookExecutionPlan,
        result: HookExecutionResult,
        triggering_event_completed: bool,
    ) -> Result<bool> {
        self.append_program_hook_audit(&plan, &result)?;
        let _ = self.record_hook_result(&plan, &result, triggering_event_completed)?;
        Ok(true)
    }

    /// Runs the dispatch focused shell hooks to active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn dispatch_focused_shell_hooks_to_active_pane(
        &mut self,
        primary_client_id: &ClientId,
    ) -> Result<Vec<FocusedShellHookDispatch>> {
        self.require_live()?;
        let mut queue = std::mem::take(&mut self.focused_shell_hooks);
        let result = (|| {
            let mut executor = RuntimeFocusedShellPaneExecutor {
                service: self,
                primary_client_id: primary_client_id.clone(),
                continuation: None,
            };
            let mut dispatches = Vec::new();
            loop {
                let shell_available = executor
                    .service
                    .focused_shell_available_for_plan(queue.front_plan());
                let Some(dispatch) = queue.dispatch_next(shell_available, &mut executor)? else {
                    break;
                };
                let blocked = dispatch.status == FocusedShellHookDispatchStatus::BlockedOnShell;
                if let Some(result) = dispatch.result.as_ref() {
                    executor
                        .service
                        .append_focused_shell_dispatch_audit(primary_client_id, result)?;
                } else {
                    executor.service.append_focused_shell_hook_start_audit(
                        primary_client_id,
                        &dispatch.hook_id,
                    )?;
                }
                dispatches.push(dispatch);
                if blocked {
                    break;
                }
            }
            Ok(dispatches)
        })();
        self.focused_shell_hooks = queue;
        result
    }

    /// Runs the append focused shell dispatch audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_focused_shell_dispatch_audit(
        &mut self,
        primary_client_id: &ClientId,
        result: &HookExecutionResult,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: primary_client_id.to_string(),
            },
            "hook",
            "runtime_focused_shell_dispatch",
        )
        .with_metadata("hook_id", result.hook_id.clone())
        .with_metadata("hook_event", format!("{:?}", result.event))
        .with_metadata("status", runtime_hook_execution_status_name(result.status));
        if let Some(exit_code) = result.exit_code {
            record = record.with_metadata("exit_code", exit_code.to_string());
        }
        if let Some(failure) = &result.failure {
            record = record
                .with_metadata("failure_kind", format!("{:?}", failure.kind))
                .with_metadata("retryable", failure.retryable.to_string());
        }
        record.outcome = runtime_hook_execution_status_name(result.status).to_string();
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Emits an audit record when a focused-shell hook is queued for
    /// asynchronous pane execution, before the marker transaction completes.
    pub(super) fn append_focused_shell_hook_start_audit(
        &mut self,
        primary_client_id: &ClientId,
        hook_id: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: primary_client_id.to_string(),
            },
            "hook",
            "runtime_focused_shell_hook_start",
        )
        .with_metadata("hook_id", hook_id.to_string())
        .with_metadata("status", "queued");
        record.outcome = "queued".to_string();
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the apply focused shell hooks for event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_focused_shell_hooks_for_event(
        &mut self,
        definitions: &[HookDefinition],
        event: HookEvent,
        event_payload_json: &str,
        executor: &mut impl FocusedShellExecutor,
    ) -> Result<RuntimeFocusedShellHookRun> {
        self.require_live()?;
        let event_plan = plan_event(definitions, event, event_payload_json)?;
        let mut enqueued = Vec::new();
        let target_pane_id = runtime_hook_target_pane_id(event_payload_json);
        for mut plan in event_plan
            .plans
            .into_iter()
            .filter(|plan| plan.run_in_focused_shell)
        {
            plan.target_pane_id = target_pane_id.clone();
            enqueued.push(self.enqueue_focused_shell_hook(plan)?);
        }
        let dispatches = self.dispatch_focused_shell_hooks(executor)?;
        Ok(RuntimeFocusedShellHookRun {
            enqueued,
            dispatches,
            pending_hooks: self.focused_shell_hooks.len(),
        })
    }
}
