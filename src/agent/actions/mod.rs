//! Agent actions implementation.
//!
//! This module owns the agent actions boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{AgentAction, MezError, Result, local_action_plan};

mod execution;
mod planning;
mod recovery;
mod runner;
mod transcript;

pub use execution::{
    AsyncMcpActionExecutor, EnvironmentEquivalence, EnvironmentEquivalenceProbe,
    LocalActionExecutor, LocalExecutionOutput, LocalExecutionRequest, LocalExecutionTransport,
    McpActionExecutor, PaneShellExecutor, PaneShellLocalExecutor, ShellExecutionOutput,
    ShellExecutionRequest, discover_tools_through_pane_shell, execute_local_action,
    execute_mcp_action_through_runtime, execute_mcp_action_through_runtime_async,
    execute_shell_action_through_pane, postprocess_shell_action_success_output,
    shell_command_result_content,
};
pub use mez_agent::{
    ShellTransportDecodeResult, ShellTransportDiagnostics, decode_shell_output_transport,
    decode_shell_output_transport_with_diagnostics,
};
pub use runner::AgentTurnRunner;
pub use transcript::{
    AgentTurnExecution, assistant_context_content_for_execution, next_transcript_sequence,
    persist_turn_execution_transcript, transcript_entries_for_execution,
};

// Shell/MCP executors, action execution, and transcript persistence.

/// Maximum previous-response bytes included in a terminal failure summary prompt.
const FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES: usize = 8 * 1024;

/// Executes the `shell_command_structured_content_json` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn shell_command_structured_content_json(
    action: &AgentAction,
    execution_transport: Option<&str>,
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
    Ok(mez_agent::shell_action_structured_content_json(
        action,
        &plan,
        execution_transport,
        sent_to_pane,
        approval,
        matched_rules,
        terminal_observation,
    ))
}
