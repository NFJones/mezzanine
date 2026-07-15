//! Semantic MAAP action lowering.
//!
//! Models author MAAP actions such as `shell_command` and `apply_patch`. This
//! module owns deterministic conversion from those actions into runtime
//! execution plans. Local actions are expressed as pane shell transactions so
//! they operate inside the same local, remote, or container shell the user is
//! interacting with through Mezzanine.

mod patch;

use super::Result;
use super::shell::validate_agent_authored_shell_command;
use super::{AgentAction, AgentActionPayload};
use mez_agent::{LocalActionKind, LocalActionPlan};

#[cfg(test)]
pub(super) use patch::APPLY_PATCH_TIMEOUT_MS;
pub use patch::{
    ApplyPatchTransactionPhase, apply_patch_error_plan, apply_patch_read_plan_for_paths,
    apply_patch_touched_paths, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output, apply_patch_write_plan_from_read_outputs,
};

/// Returns the local shell plan for a shell-backed MAAP action.
pub fn local_action_plan(action: &AgentAction) -> Result<Option<LocalActionPlan>> {
    match &action.payload {
        AgentActionPayload::ShellCommand {
            summary,
            command,
            interactive,
            stateful,
            timeout_ms,
        } => {
            validate_agent_authored_shell_command(command)?;
            Ok(Some(LocalActionPlan {
                kind: LocalActionKind::ShellCommand,
                summary: summary.clone(),
                command: command.clone(),
                policy_command: command.clone(),
                interactive: *interactive,
                stateful: *stateful,
                timeout_ms: *timeout_ms,
                display_output_after_completion: false,
            }))
        }
        AgentActionPayload::ApplyPatch { patch, strip } => {
            Ok(Some(patch::apply_patch_plan(patch, *strip)?))
        }
        _ => Ok(None),
    }
}

/// Returns the user-facing summary for a shell-backed action.
pub fn local_action_summary(action: &AgentAction) -> Result<Option<String>> {
    Ok(local_action_plan(action)?.map(|plan| plan.summary))
}

/// Returns true when an action is implemented by a pane shell transaction.
pub fn action_is_local_shell_backed(action: &AgentAction) -> bool {
    matches!(
        action.payload,
        AgentActionPayload::ShellCommand { .. } | AgentActionPayload::ApplyPatch { .. }
    )
}
