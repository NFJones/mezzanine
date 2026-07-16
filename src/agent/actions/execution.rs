//! Shell and MCP action execution helpers.
//!
//! This module owns the boundary between planned agent actions and the
//! executor interfaces supplied by the runtime. It converts shell/MCP executor
//! outputs back into durable `ActionResult` values while keeping pane and MCP
//! I/O details out of turn negotiation.

use super::super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord,
    DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature, MarkerToken, McpExecutionRequest,
    MezError, Path, Result, ShellTransaction, ShellTransactionOutputTransport, ToolDiscoveryCache,
    ToolInventory, action_text_content_blocks, local_action_plan, tool_discovery_script,
};
use super::shell_command_structured_content_json;
use mez_agent::{
    AsyncMcpActionExecutor, LocalActionExecutor, LocalExecutionOutput, LocalExecutionRequest,
    LocalExecutionTransport, McpActionExecutor, PaneShellExecutor, ShellExecutionOutput,
    ShellExecutionRequest, decode_shell_output_transport_with_diagnostics,
    mcp_response_to_action_result,
};
use std::time::{SystemTime, UNIX_EPOCH};
/// Default turn-wide shell action timeout used by transport-neutral execution.
const LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Adapts the existing pane shell executor to the transport-neutral executor
/// contract.
pub struct PaneShellLocalExecutor<'a, E> {
    shell_path: &'a Path,
    pane_executor: &'a mut E,
}

impl<'a, E> PaneShellLocalExecutor<'a, E> {
    /// Builds an adapter around a pane shell executor and shell path.
    pub fn new(shell_path: &'a Path, pane_executor: &'a mut E) -> Self {
        Self {
            shell_path,
            pane_executor,
        }
    }
}

impl<E> LocalActionExecutor for PaneShellLocalExecutor<'_, E>
where
    E: PaneShellExecutor<Error = MezError>,
{
    type Error = MezError;

    fn transport(&self) -> LocalExecutionTransport {
        LocalExecutionTransport::PaneShell
    }

    fn execute_local_action(
        &mut self,
        request: &LocalExecutionRequest,
    ) -> Result<LocalExecutionOutput> {
        let transaction = ShellTransaction::new(
            request.marker.clone(),
            &request.turn_id,
            &request.agent_id,
            &request.pane_id,
            self.shell_path,
            &request.plan.command,
        )?
        .with_output_transport(ShellTransactionOutputTransport::Base64);
        let shell_request = ShellExecutionRequest {
            action_id: request.action_id.clone(),
            transaction,
            timeout_ms: Some(request.effective_timeout_ms),
            interactive: request.plan.interactive,
            stateful: request.plan.stateful,
        };
        self.pane_executor
            .execute_shell(&shell_request)
            .map(LocalExecutionOutput::pane_shell)
    }
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
    executor: &mut impl PaneShellExecutor<Error = MezError>,
) -> Result<ActionResult> {
    let mut local_executor = PaneShellLocalExecutor::new(shell_path, executor);
    execute_local_action(turn, action, marker, &mut local_executor)
}

/// Executes a local action through the supplied transport-neutral executor.
///
/// Callers receive the same `ActionResult` shape regardless of the transport
/// that ran the planned local action.
pub fn execute_local_action(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    marker: MarkerToken,
    executor: &mut impl LocalActionExecutor<Error = MezError>,
) -> Result<ActionResult> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "local execution requires a local action",
        ));
    };
    let effective_timeout_ms = local_execution_shell_timeout_ms(turn, plan.timeout_ms);
    let transport = executor.transport();
    let request = LocalExecutionRequest {
        action_id: action.id.clone(),
        action: action.clone(),
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        plan,
        effective_timeout_ms,
        transport,
        marker: marker.clone(),
    };
    let mut output = executor.execute_local_action(&request)?;
    output.shell_output = postprocess_semantic_shell_output(action, output.shell_output)?;
    local_output_to_action_result(turn, action, output, marker)
}

/// Returns the remaining turn-wide timeout budget for transport-neutral local execution.
fn local_execution_turn_remaining_timeout_ms(turn: &AgentTurnRecord) -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    if turn.started_at_unix_seconds < 946_684_800 {
        return LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS;
    }
    let started_at_ms = turn.started_at_unix_seconds.saturating_mul(1000);
    let elapsed_ms = now_ms.saturating_sub(started_at_ms);
    LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS
        .saturating_sub(elapsed_ms)
        .max(1)
}

/// Returns the finite shell timeout for one local execution request.
fn local_execution_shell_timeout_ms(turn: &AgentTurnRecord, timeout_ms: Option<u64>) -> u64 {
    let remaining = local_execution_turn_remaining_timeout_ms(turn);
    timeout_ms
        .map(|timeout_ms| timeout_ms.min(remaining))
        .unwrap_or(remaining)
        .max(1)
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
    if decoded.diagnostics.saw_begin_marker {
        output.stdout = decoded.output;
        output.transport_diagnostics = decoded.diagnostics;
    }
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
    plan: &McpExecutionRequest,
    executor: &mut impl McpActionExecutor<Error = MezError>,
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
    Ok(mcp_response_to_action_result(turn, action, plan, response)?)
}

/// Executes the `execute_mcp_action_through_runtime_async` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub async fn execute_mcp_action_through_runtime_async(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpExecutionRequest,
    executor: &mut impl AsyncMcpActionExecutor<Error = MezError>,
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
    Ok(mcp_response_to_action_result(turn, action, plan, response)?)
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
    executor: &mut impl PaneShellExecutor<Error = MezError>,
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
pub(super) fn local_output_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    output: LocalExecutionOutput,
    marker: MarkerToken,
) -> Result<ActionResult> {
    local_output_to_action_result_with_transport(
        turn,
        action,
        output.transport,
        output.sent_to_pane,
        output.shell_output,
        marker,
    )
}

fn local_output_to_action_result_with_transport(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: LocalExecutionTransport,
    sent_to_pane: bool,
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
    let signal: Option<i32> = if let Some(signal) = output.signal {
        Some(signal)
    } else if output.interrupted {
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
    let mut combined_output = String::new();
    if !output.stdout.is_empty() {
        combined_output.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        combined_output.push_str(&output.stderr);
    }
    let structured = shell_command_structured_content_json(
        action,
        Some(transport.as_str()),
        sent_to_pane,
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
            "combined_output_preview": combined_output,
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
        let exit_message = match output.exit_code {
            Some(exit_code) => format!("shell command exited with status {exit_code}"),
            None => "shell command finished without an exit status".to_string(),
        };
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "shell_command_failed",
            exit_message,
        )?;
        result.content = action_text_content_blocks(content);
        result.structured_content_json = Some(structured);
        Ok(result)
    }
}
