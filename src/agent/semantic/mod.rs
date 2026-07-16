//! Product adapter for semantic MAAP action lowering.
//!
//! Models author MAAP actions such as `shell_command` and `apply_patch`. This
//! module applies product shell-command validation and projects lower semantic
//! patch planning errors into `MezError`. Local actions remain pane shell
//! transactions so they execute in the user's active environment.

use super::Result;
use super::{AgentAction, AgentActionPayload};
use mez_agent::validate_agent_authored_shell_command;
use mez_agent::{LocalActionKind, LocalActionPlan};

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
        AgentActionPayload::ApplyPatch { patch, strip } => Ok(Some(
            mez_agent::semantic_patch_planning::apply_patch_plan(patch, *strip)?,
        )),
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
