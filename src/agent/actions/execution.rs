//! Shell and MCP action execution helpers.
//!
//! This module owns the boundary between planned agent actions and the
//! executor interfaces supplied by the runtime. It converts shell/MCP executor
//! outputs back into durable `ActionResult` values while keeping pane and MCP
//! I/O details out of turn negotiation.

use super::super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord,
    DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature, MarkerToken, McpToolCallPlan,
    McpToolCallResponse, MezError, Path, Result, ShellTransaction, ShellTransactionOutputTransport,
    ToolDiscoveryCache, ToolInventory, action_content_blocks_from_json_or_text,
    action_text_content_blocks, json_escape, local_action_plan, tool_discovery_script,
};
use super::{
    ShellTransportDiagnostics, decode_shell_output_transport_with_diagnostics,
    shell_command_structured_content_json,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution request state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionRequest {
    /// Structured `action_id` value carried by this API type.
    pub action_id: String,
    /// Structured `transaction` value carried by this API type.
    pub transaction: ShellTransaction,
    /// Structured `timeout_ms` value carried by this API type.
    pub timeout_ms: Option<u64>,
    /// Structured `interactive` value carried by this API type.
    pub interactive: bool,
    /// Structured `stateful` value carried by this API type.
    pub stateful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution output state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionOutput {
    /// Structured `exit_code` value carried by this API type.
    pub exit_code: Option<i32>,
    /// Structured `stdout` value carried by this API type.
    pub stdout: String,
    /// Structured `stderr` value carried by this API type.
    pub stderr: String,
    /// Structured `timed_out` value carried by this API type.
    pub timed_out: bool,
    /// Structured `interrupted` value carried by this API type.
    pub interrupted: bool,
    /// Structured transport diagnostics kept separate from command output.
    pub transport_diagnostics: ShellTransportDiagnostics,
}

impl ShellExecutionOutput {
    /// Builds shell execution output with no transport diagnostics.
    ///
    /// Callers that already provide decoded or non-transported output can use
    /// this constructor without fabricating empty diagnostic state at each
    /// call site.
    pub fn new(
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        timed_out: bool,
        interrupted: bool,
    ) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
            timed_out,
            interrupted,
            transport_diagnostics: ShellTransportDiagnostics::default(),
        }
    }
}

/// Defines the `PaneShellExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait PaneShellExecutor {
    /// Runs the execute shell operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    fn execute_shell(&mut self, request: &ShellExecutionRequest) -> Result<ShellExecutionOutput>;
}

/// Defines the `McpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait McpActionExecutor {
    /// Runs the execute mcp call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    fn execute_mcp_call(&mut self, plan: &McpToolCallPlan) -> Result<McpToolCallResponse>;
}

#[allow(async_fn_in_trait)]
/// Defines the `AsyncMcpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait AsyncMcpActionExecutor {
    /// Runs the execute mcp call async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    async fn execute_mcp_call_async(
        &mut self,
        plan: &McpToolCallPlan,
    ) -> Result<McpToolCallResponse>;
}

/// Executes the `execute_shell_action_through_pane` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_shell_action_through_pane(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ActionResult> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "pane shell execution requires a shell-backed action",
        ));
    };
    let transaction = ShellTransaction::new(
        marker.clone(),
        &turn.turn_id,
        &turn.agent_id,
        &turn.pane_id,
        shell_path,
        &plan.command,
    )?
    .with_output_transport(ShellTransactionOutputTransport::Base64);
    let request = ShellExecutionRequest {
        action_id: action.id.clone(),
        transaction,
        timeout_ms: plan.timeout_ms,
        interactive: plan.interactive,
        stateful: plan.stateful,
    };
    let output = postprocess_semantic_shell_output(action, executor.execute_shell(&request)?)?;
    shell_output_to_action_result(turn, action, output, marker)
}

/// Applies native success-output shaping for shell-backed semantic actions.
///
/// Pane-side semantic commands stay limited to small shell primitives. Line
/// slicing, truncation notices, and generated change previews are applied here
/// after the pane shell returns its bounded output.
pub fn postprocess_shell_action_success_output(
    action: &AgentAction,
    stdout: String,
) -> Result<String> {
    let output = ShellExecutionOutput::new(Some(0), stdout, String::new(), false, false);
    postprocess_semantic_shell_output(action, output).map(|output| output.stdout)
}

/// Builds compact action-result content for a plain model-authored shell command.
///
/// # Parameters
/// - `output`: The command stdout/stderr already decoded for model context.
/// - `exit_code`: The observed process exit code, when one was observed.
/// - `timed_out`: Whether the command timed out before a process exit.
/// - `interrupted`: Whether the command was interrupted by the runtime.
pub fn shell_command_result_content(
    output: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    interrupted: bool,
) -> Vec<String> {
    if !output.trim().is_empty() {
        return vec![output.to_string()];
    }
    let status = if timed_out {
        "shell command timed out".to_string()
    } else if interrupted {
        "shell command was interrupted".to_string()
    } else if let Some(exit_code) = exit_code {
        format!("shell command exited with status {exit_code}")
    } else {
        "shell command finished without an exit status".to_string()
    };
    vec![status]
}

fn postprocess_semantic_shell_output(
    action: &AgentAction,
    mut output: ShellExecutionOutput,
) -> Result<ShellExecutionOutput> {
    let decoded = decode_shell_output_transport_with_diagnostics(&output.stdout);
    output.stdout = decoded.output;
    output.transport_diagnostics = decoded.diagnostics;
    if output.exit_code != Some(0) || output.timed_out || output.interrupted {
        return Ok(output);
    }
    if let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload {
        ensure_success_preview(&mut output, patch_change_preview(patch));
    }
    Ok(output)
}

fn ensure_success_preview(output: &mut ShellExecutionOutput, preview: String) {
    if output.stdout.trim().is_empty() {
        output.stdout = preview;
    }
}

fn patch_change_preview(patch: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 160;
    let mut lines = vec!["diff -- apply patch".to_string()];
    for line in patch.lines().take(MAX_PREVIEW_LINES) {
        lines.push(line.to_string());
    }
    let total_lines = patch.lines().count();
    if total_lines > MAX_PREVIEW_LINES {
        lines.push(format!(
            "[mez: diff truncated; {} lines omitted]",
            total_lines - MAX_PREVIEW_LINES
        ));
    }
    lines.join("\n") + "\n"
}

/// Executes the `execute_mcp_action_through_runtime` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_mcp_action_through_runtime(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl McpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call(plan)?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `execute_mcp_action_through_runtime_async` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub async fn execute_mcp_action_through_runtime_async(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl AsyncMcpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call_async(plan).await?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `discover_tools_through_pane_shell` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn discover_tools_through_pane_shell(
    cache: &mut ToolDiscoveryCache,
    signature: EnvironmentSignature,
    turn: &AgentTurnRecord,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ToolInventory> {
    if let Some(inventory) = cache.get(&signature) {
        return Ok(inventory.clone());
    }

    let transaction = ShellTransaction::new(
        marker,
        &turn.turn_id,
        &turn.agent_id,
        &turn.pane_id,
        shell_path,
        tool_discovery_script(),
    )?;
    let request = ShellExecutionRequest {
        action_id: format!("tool-discovery:{}", turn.turn_id),
        transaction,
        timeout_ms: Some(DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS),
        interactive: false,
        stateful: false,
    };
    let output = executor.execute_shell(&request)?;
    if output.timed_out {
        return Err(MezError::invalid_state("tool discovery timed out"));
    }
    if output.interrupted {
        return Err(MezError::invalid_state("tool discovery was interrupted"));
    }
    if output.exit_code != Some(0) {
        return Err(MezError::invalid_state(format!(
            "tool discovery failed: {}",
            output.stderr.trim()
        )));
    }

    let inventory = ToolInventory::parse_bootstrap_output(&output.stdout);
    cache.record(signature, inventory.clone());
    Ok(inventory)
}

/// Executes the `shell_output_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn shell_output_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    output: ShellExecutionOutput,
    marker: MarkerToken,
) -> Result<ActionResult> {
    if local_action_plan(action)?.is_none() {
        return Err(MezError::invalid_args(
            "shell output requires a shell-backed action",
        ));
    }
    let combined_output_bytes = output.stdout.len().saturating_add(output.stderr.len());
    let transport_incomplete = output.transport_diagnostics.transport_incomplete();
    let output_truncated = output.transport_diagnostics.output_truncated();
    let signal: Option<i32> = if output.interrupted {
        Some(2) // SIGINT
    } else if let Some(ec) = output.exit_code {
        if ec > 128 && ec < 256 {
            Some(ec - 128)
        } else {
            None
        }
    } else {
        None
    };
    let structured = shell_command_structured_content_json(
        action,
        true,
        serde_json::Value::Null,
        &[],
        serde_json::json!({
            "source": "executor",
            "stream": "pty_combined",
            "marker": marker.as_str(),
            "exit_code": output.exit_code,
            "signal": signal,
            "timed_out": output.timed_out,
            "interrupted": output.interrupted,
            "combined_output_bytes": combined_output_bytes,
            "output_truncated": output_truncated,
            "transport_incomplete": transport_incomplete,
            "transport_diagnostics": output.transport_diagnostics.to_json()
        }),
    )?;
    if output.timed_out {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::TimedOut,
            "shell_timeout",
            "shell command timed out",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    if output.interrupted {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Interrupted,
            "shell_interrupted",
            "shell command was interrupted",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    let mut combined_output = String::new();
    if !output.stdout.is_empty() {
        combined_output.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        combined_output.push_str(&output.stderr);
    }
    let mut content = Vec::new();
    if !combined_output.is_empty() {
        content.push(combined_output);
    }
    if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
        return Ok(ActionResult::succeeded(
            turn,
            action,
            shell_command_result_content(
                content.first().map(String::as_str).unwrap_or_default(),
                output.exit_code,
                output.timed_out,
                output.interrupted,
            ),
            Some(structured),
        ));
    }
    if output.exit_code == Some(0) {
        Ok(ActionResult::succeeded(
            turn,
            action,
            content,
            Some(structured),
        ))
    } else {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "shell_exit_nonzero",
            "shell command exited with non-zero status",
        )?;
        result.content = action_text_content_blocks(content);
        result.structured_content_json = Some(structured);
        Ok(result)
    }
}

/// Executes the `mcp_response_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn mcp_response_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    response: McpToolCallResponse,
) -> Result<ActionResult> {
    let content_json = response.content_json.clone();
    let structured_payload = format!(
        r#"{{"server":"{}","tool":"{}","content":{},"structured_content":{},"is_error":{}}}"#,
        json_escape(&plan.server_id),
        json_escape(&plan.tool_name),
        content_json,
        response
            .structured_content_json
            .as_deref()
            .unwrap_or("null"),
        response.is_error
    );
    let content = action_content_blocks_from_json_or_text(&response.content_json);
    if response.is_error {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "mcp_tool_error",
            "MCP tool returned an error",
        )?;
        result.content = content;
        result.structured_content_json = Some(structured_payload);
        Ok(result)
    } else {
        let mut result =
            ActionResult::succeeded(turn, action, Vec::new(), Some(structured_payload));
        result.content = content;
        Ok(result)
    }
}
