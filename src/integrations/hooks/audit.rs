//! Audit record emission for hook execution.
//!
//! Audit helpers translate hook plans and execution results into sanitized audit
//! records while keeping execution code independent from audit-log persistence.

#[cfg(test)]
use super::{FocusedShellExecutor, execute_focused_shell_hook, execute_program_hook};
#[cfg(test)]
use crate::error::Result;
#[cfg(test)]
use crate::security::audit::AuditLog;

use crate::security::audit::{AuditActor, AuditRecord};

use super::types::{
    HookEvent, HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailureKind,
};

/// Runs the execute program hook with audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn execute_program_hook_with_audit(
    plan: &HookExecutionPlan,
    audit_log: &mut AuditLog,
    session_id: &str,
    actor: AuditActor,
) -> Result<HookExecutionResult> {
    let result = execute_program_hook(plan)?;
    let record = hook_audit_record(plan, session_id, actor, "execute_program_hook", &result);
    let _ = audit_log.append(record)?;
    Ok(result)
}

/// Runs the execute focused shell hook with audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn execute_focused_shell_hook_with_audit(
    plan: &HookExecutionPlan,
    executor: &mut impl FocusedShellExecutor,
    audit_log: &mut AuditLog,
    session_id: &str,
    actor: AuditActor,
) -> Result<HookExecutionResult> {
    let result = execute_focused_shell_hook(plan, executor)?;
    let record = hook_audit_record(
        plan,
        session_id,
        actor,
        "execute_focused_shell_hook",
        &result,
    );
    let _ = audit_log.append(record)?;
    Ok(result)
}

/// Runs the hook audit record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_audit_record(
    plan: &HookExecutionPlan,
    session_id: &str,
    actor: AuditActor,
    action: &str,
    result: &HookExecutionResult,
) -> AuditRecord {
    let mut record = AuditRecord::new(session_id, actor, "hook", action)
        .with_metadata("hook_id", plan.hook_id.clone())
        .with_metadata("hook_event", hook_event_name(plan.event))
        .with_metadata("runner", hook_runner_name(plan))
        .with_metadata("status", hook_execution_status_name(result.status));
    if let Some(program) = &plan.program {
        record = record.with_metadata("program", program.clone());
    }
    if let Some(shell_command) = &plan.shell_command {
        record = record.with_metadata("shell_command_bytes", shell_command.len().to_string());
    }
    if let Some(exit_code) = result.exit_code {
        record = record.with_metadata("exit_code", exit_code.to_string());
    }
    if let Some(failure) = &result.failure {
        record = record
            .with_metadata("failure_kind", hook_failure_kind_name(failure.kind))
            .with_metadata("retryable", failure.retryable.to_string());
    }
    record.outcome = hook_execution_status_name(result.status).to_string();
    record
}

/// Runs the hook execution audit record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn hook_execution_audit_record(
    plan: &HookExecutionPlan,
    session_id: &str,
    actor: AuditActor,
    action: &str,
    result: &HookExecutionResult,
) -> AuditRecord {
    hook_audit_record(plan, session_id, actor, action, result)
}

/// Runs the hook event name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn hook_event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "session_start",
        HookEvent::SessionStop => "session_stop",
        HookEvent::ClientAttach => "client_attach",
        HookEvent::ClientDetach => "client_detach",
        HookEvent::WindowCreate => "window_create",
        HookEvent::WindowClose => "window_close",
        HookEvent::SessionDetach => "session_detach",
        HookEvent::PaneCreate => "pane_create",
        HookEvent::PaneClose => "pane_close",
        HookEvent::UserPromptSubmit => "user_prompt_submit",
        HookEvent::AgentTurnStart => "agent_turn_start",
        HookEvent::AgentTurnStop => "agent_turn_stop",
        HookEvent::PreShellCommand => "pre_shell_command",
        HookEvent::PostShellCommand => "post_shell_command",
        HookEvent::PermissionRequest => "permission_request",
        HookEvent::PermissionDecision => "permission_decision",
        HookEvent::PreMcpToolUse => "pre_mcp_tool_use",
        HookEvent::PostMcpToolUse => "post_mcp_tool_use",
        HookEvent::LayoutSave => "layout_save",
        HookEvent::LayoutLoad => "layout_load",
    }
}

/// Runs the hook execution status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_execution_status_name(status: HookExecutionStatus) -> &'static str {
    match status {
        HookExecutionStatus::Succeeded => "succeeded",
        HookExecutionStatus::Queued => "queued",
        HookExecutionStatus::Failed => "failed",
        HookExecutionStatus::TimedOut => "timed_out",
    }
}

/// Runs the hook runner name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_runner_name(plan: &HookExecutionPlan) -> &'static str {
    if plan.run_in_focused_shell {
        "focused_shell"
    } else {
        "program"
    }
}

/// Runs the hook failure kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_failure_kind_name(kind: HookFailureKind) -> &'static str {
    match kind {
        HookFailureKind::Planning => "planning",
        HookFailureKind::Spawn => "spawn",
        HookFailureKind::ExitNonZero => "exit_nonzero",
        HookFailureKind::Timeout => "timeout",
        HookFailureKind::PolicyDenied => "policy_denied",
        HookFailureKind::ShellUnavailable => "shell_unavailable",
    }
}
