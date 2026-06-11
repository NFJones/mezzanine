//! Agent actions implementation.
//!
//! This module owns the agent actions boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnState, ContextSourceKind,
    MezError, ModelMessageRole, Result, SayStatus, json_escape, local_action_plan,
};

mod execution;
mod planning;
mod read_observation;
mod recovery;
mod result_context;
mod runner;
mod shell_transport;
mod transcript;

pub use execution::{
    AsyncMcpActionExecutor, McpActionExecutor, PaneShellExecutor, ShellExecutionOutput,
    ShellExecutionRequest, discover_tools_through_pane_shell, execute_mcp_action_through_runtime,
    execute_mcp_action_through_runtime_async, execute_shell_action_through_pane,
    postprocess_shell_action_success_output, shell_command_result_content,
};
pub use read_observation::{
    ShellReadObservation, ShellReadObservationKind, ShellReadRange,
    shell_read_observations_for_command,
};
pub use result_context::action_result_context_content;
pub(super) use result_context::action_result_transcript_content;
pub use runner::AgentTurnRunner;
pub use shell_transport::{
    ShellTransportDecodeResult, ShellTransportDiagnostics, decode_shell_output_transport,
    decode_shell_output_transport_with_diagnostics,
};
pub use transcript::{
    AgentTurnExecution, assistant_context_content_for_execution, next_transcript_sequence,
    persist_turn_execution_transcript, transcript_entries_for_execution,
};

// Shell/MCP executors, action execution, and transcript persistence.

/// Maximum previous-response bytes included in one ephemeral MAAP repair prompt.
const MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES: usize = 12 * 1024;
/// Maximum previous-response bytes included in a terminal failure summary prompt.
const FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES: usize = 8 * 1024;

/// Executes the `turn_state_from_action_results` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn turn_state_from_action_results(
    results: &[ActionResult],
    final_turn: bool,
) -> AgentTurnState {
    if results
        .iter()
        .any(|result| result.status == ActionStatus::Blocked)
    {
        AgentTurnState::Blocked
    } else if results.iter().any(|result| result.is_error) {
        AgentTurnState::Failed
    } else if results
        .iter()
        .any(|result| result.status == ActionStatus::Running)
    {
        AgentTurnState::Running
    } else if final_turn || results_are_display_only_completion(results) {
        AgentTurnState::Completed
    } else {
        AgentTurnState::Running
    }
}

/// Reports whether action results represent an explicit display-only
/// completion.
///
/// Empty result sets are not completions. Treating them as such through
/// vacuous `all(...)` semantics can mask missing provider output or missing
/// action planning as a settled turn.
fn results_are_display_only_completion(results: &[ActionResult]) -> bool {
    !results.is_empty() && results.iter().all(result_is_display_only)
}

/// Executes the `result_is_display_only` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn result_is_display_only(result: &ActionResult) -> bool {
    matches!(result.action_type, "complete")
}

/// Builds the structured result payload for a `say` action.
fn say_structured_content_json(status: SayStatus, content_type: &str, text: &str) -> String {
    format!(
        r#"{{"kind":"say","status":"{}","content_type":"{}","text":"{}"}}"#,
        status.as_str(),
        json_escape(content_type),
        json_escape(text),
    )
}

/// Executes the `shell_command_structured_content_json` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn shell_command_structured_content_json(
    action: &AgentAction,
    sent_to_pane: bool,
    approval: serde_json::Value,
    matched_rules: &[String],
    terminal_observation: serde_json::Value,
) -> Result<String> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "shell structured content requires a shell-backed action",
        ));
    };
    let generated_command_elided =
        !matches!(action.payload, AgentActionPayload::ShellCommand { .. });
    let command = if generated_command_elided {
        plan.policy_command.clone()
    } else {
        plan.command.clone()
    };
    let read_observations = shell_read_observations_for_command(&command);
    let value = serde_json::json!({
        "kind": action.action_type(),
        "summary": plan.summary,
        "command": command,
        "read_observations": read_observations,
        "generated_command_elided": generated_command_elided,
        "generated_command_bytes": if generated_command_elided { Some(plan.command.len()) } else { None },
        "sent_to_pane": sent_to_pane,
        "stateful": plan.stateful,
        "approval": approval,
        "matched_rules": matched_rules,
        "terminal_observation": terminal_observation
    });
    serde_json::to_string(&value).map_err(|error| {
        MezError::invalid_state(format!("shell structured content encoding failed: {error}"))
    })
}

/// Executes the `role_for_source` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn role_for_source(source: ContextSourceKind) -> ModelMessageRole {
    match source {
        ContextSourceKind::System => ModelMessageRole::System,
        ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Policy
        | ContextSourceKind::Configuration
        | ContextSourceKind::RuntimeHint => ModelMessageRole::Developer,
        ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool => {
            ModelMessageRole::Tool
        }
        ContextSourceKind::EvidenceLedger | ContextSourceKind::CommittedEvidence => {
            ModelMessageRole::Developer
        }
        ContextSourceKind::TranscriptAssistant => ModelMessageRole::Assistant,
        ContextSourceKind::UserInstruction
        | ContextSourceKind::SkillInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser => ModelMessageRole::User,
    }
}
