//! Hook planning and failure-policy decisions.
//!
//! Planning validates definitions, filters disabled hooks, expands invocation
//! configuration into execution plans, and maps failure policy to runtime action.

use crate::error::Result;

use super::types::{
    HookDefinition, HookEvent, HookEventPlan, HookExecutionPlan, HookFailure, HookFailureDecision,
    HookInvocation, HookOnFailure,
};

/// Runs the plan hook operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn plan_hook(definition: &HookDefinition) -> Result<Option<HookExecutionPlan>> {
    plan_hook_with_payload(definition, "{}")
}

/// Runs the plan hook with payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn plan_hook_with_payload(
    definition: &HookDefinition,
    event_payload_json: &str,
) -> Result<Option<HookExecutionPlan>> {
    definition.validate()?;
    if !definition.enabled {
        return Ok(None);
    }
    if !definition.matches_payload(event_payload_json)? {
        return Ok(None);
    }

    let plan = match &definition.invocation {
        HookInvocation::Program { command, args } => HookExecutionPlan {
            hook_id: definition.id.clone(),
            event: definition.event,
            run_in_focused_shell: false,
            target_pane_id: None,
            blocks_on_shell_availability: false,
            program: Some(command.clone()),
            args: args.clone(),
            shell_command: None,
            event_payload_json: event_payload_json.to_string(),
            timeout_ms: definition.timeout_ms.unwrap_or(30_000),
            on_failure: definition
                .on_failure
                .unwrap_or_else(|| default_on_failure(definition)),
        },
        HookInvocation::FocusedShell { command } => HookExecutionPlan {
            hook_id: definition.id.clone(),
            event: definition.event,
            run_in_focused_shell: true,
            target_pane_id: None,
            blocks_on_shell_availability: definition.agent_hook,
            program: None,
            args: Vec::new(),
            shell_command: Some(command.clone()),
            event_payload_json: event_payload_json.to_string(),
            timeout_ms: definition.timeout_ms.unwrap_or(30_000),
            on_failure: definition
                .on_failure
                .unwrap_or_else(|| default_on_failure(definition)),
        },
    };

    Ok(Some(plan))
}

/// Runs the plan event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn plan_event(
    definitions: &[HookDefinition],
    event: HookEvent,
    event_payload_json: &str,
) -> Result<HookEventPlan> {
    let mut plans = Vec::new();
    for definition in definitions
        .iter()
        .filter(|definition| definition.event == event)
    {
        if let Some(plan) = plan_hook_with_payload(definition, event_payload_json)? {
            plans.push(plan);
        }
    }
    Ok(HookEventPlan { plans })
}

/// Runs the decide hook failure operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn decide_hook_failure(
    plan: &HookExecutionPlan,
    _failure: &HookFailure,
    triggering_event_completed: bool,
) -> HookFailureDecision {
    match plan.on_failure {
        HookOnFailure::Block if triggering_event_completed => HookFailureDecision::Warn,
        HookOnFailure::Block => HookFailureDecision::Block,
        HookOnFailure::Warn => HookFailureDecision::Warn,
        HookOnFailure::Ignore => HookFailureDecision::Ignore,
    }
}

/// Runs the default on failure operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn default_on_failure(definition: &HookDefinition) -> HookOnFailure {
    match definition.event {
        HookEvent::PreShellCommand
        | HookEvent::PermissionRequest
        | HookEvent::PreMcpToolUse
        | HookEvent::LayoutLoad => HookOnFailure::Block,
        HookEvent::AgentTurnStart | HookEvent::UserPromptSubmit if definition.agent_hook => {
            HookOnFailure::Block
        }
        HookEvent::SessionStart if definition.required => HookOnFailure::Block,
        _ => HookOnFailure::Warn,
    }
}
