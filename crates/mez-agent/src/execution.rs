//! Provider-independent action execution ports and transport records.
//!
//! The agent harness uses these contracts to request local shell and MCP work
//! without owning pane I/O, MCP transports, filesystem access, or product
//! errors. Port implementations retain their native error type through an
//! associated type so the composition crate can project failures once at its
//! boundary.

use std::error::Error;
use std::fmt;

use crate::{
    ActionContentBlock, ActionResult, ActionResultContractError, ActionResultContractResult,
    ActionStatus, AgentAction, AgentActionPayload, AgentTurnResultIdentity, LocalActionPlan,
    LocalActionPlanningError, MarkerToken, McpExecutionRequest, McpExecutionResponse,
    ShellTransaction, ShellTransportDiagnostics, action_text_content_blocks,
    decode_shell_output_transport_with_diagnostics, local_action_plan,
    shell_action_structured_content_json,
};

/// Default turn-wide budget for shell-backed agent work.
pub const DEFAULT_AGENT_TURN_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Error returned while projecting local execution output into a MAAP result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExecutionProjectionError {
    message: String,
}

impl LocalExecutionProjectionError {
    /// Returns the canonical projection diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for LocalExecutionProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LocalExecutionProjectionError {}

impl From<LocalActionPlanningError> for LocalExecutionProjectionError {
    fn from(error: LocalActionPlanningError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<ActionResultContractError> for LocalExecutionProjectionError {
    fn from(error: ActionResultContractError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

/// Error returned when an approved MCP execution request no longer matches
/// its canonical model-authored action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExecutionValidationError {
    message: String,
}

impl McpExecutionValidationError {
    /// Returns the stable validation diagnostic for product error projection.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for McpExecutionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for McpExecutionValidationError {}

/// Validates that one retained MCP execution request still matches its
/// model-authored action before a concrete transport is invoked.
pub fn validate_mcp_execution_request(
    action: &AgentAction,
    request: &McpExecutionRequest,
) -> Result<(), McpExecutionValidationError> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(McpExecutionValidationError {
            message: "MCP execution requires an mcp_call action".to_string(),
        });
    };
    if request.server_id != *server
        || request.tool_name != *tool
        || request.arguments_json.trim() != arguments_json.trim()
    {
        return Err(McpExecutionValidationError {
            message: "MCP execution plan does not match the action payload".to_string(),
        });
    }
    Ok(())
}

/// Returns the remaining turn-wide shell budget at one injected clock value.
///
/// Synthetic turns use timestamps before 2000 in tests and compatibility
/// fixtures. Treat those timestamps as having a fresh budget instead of
/// immediately expiring them against the host clock.
pub fn agent_turn_remaining_timeout_ms(started_at_unix_seconds: u64, now_unix_millis: u64) -> u64 {
    if started_at_unix_seconds < 946_684_800 {
        return DEFAULT_AGENT_TURN_TIMEOUT_MS;
    }
    let started_at_millis = started_at_unix_seconds.saturating_mul(1000);
    let elapsed_millis = now_unix_millis.saturating_sub(started_at_millis);
    DEFAULT_AGENT_TURN_TIMEOUT_MS
        .saturating_sub(elapsed_millis)
        .max(1)
}

/// Bounds one model-authored shell timeout by the remaining turn-wide budget.
pub fn agent_shell_timeout_ms(
    started_at_unix_seconds: u64,
    now_unix_millis: u64,
    requested_timeout_ms: Option<u64>,
) -> u64 {
    let remaining = agent_turn_remaining_timeout_ms(started_at_unix_seconds, now_unix_millis);
    requested_timeout_ms
        .map(|timeout_ms| timeout_ms.min(remaining))
        .unwrap_or(remaining)
        .max(1)
}

/// Builds compact action-result content for a plain model-authored shell
/// command when the command produced no visible output.
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

/// Applies canonical success-output shaping for shell-backed semantic actions.
///
/// Concrete transports remain responsible for collecting output. This helper
/// decodes canonical shell framing and supplies a bounded apply-patch preview
/// when the successful transport returned no output.
pub fn postprocess_local_shell_output(
    action: &AgentAction,
    mut output: ShellExecutionOutput,
) -> ShellExecutionOutput {
    let decoded = decode_shell_output_transport_with_diagnostics(&output.stdout);
    if decoded.diagnostics.saw_begin_marker {
        output.stdout = decoded.output;
        output.transport_diagnostics = decoded.diagnostics;
    }
    if output.exit_code != Some(0) || output.timed_out || output.interrupted {
        return output;
    }
    if let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload
        && output.stdout.trim().is_empty()
    {
        output.stdout = patch_change_preview(patch);
    }
    output
}

/// Applies canonical success-output shaping to one captured stdout string.
pub fn postprocess_shell_action_success_output(action: &AgentAction, stdout: String) -> String {
    postprocess_local_shell_output(
        action,
        ShellExecutionOutput::new(Some(0), stdout, String::new(), false, false),
    )
    .stdout
}

/// Converts transport-neutral local execution output into a canonical action
/// result after concrete execution has completed.
pub fn local_execution_output_to_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    output: LocalExecutionOutput,
    marker: &MarkerToken,
) -> Result<ActionResult, LocalExecutionProjectionError> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(LocalExecutionProjectionError {
            message: "shell output requires a shell-backed action".to_string(),
        });
    };
    let transport = output.transport;
    let sent_to_pane = output.sent_to_pane;
    let output = output.shell_output;
    let combined_output_bytes = output.stdout.len().saturating_add(output.stderr.len());
    let transport_incomplete = output.transport_diagnostics.transport_incomplete();
    let output_truncated = output.transport_diagnostics.output_truncated();
    let signal = output.signal.or_else(|| {
        if output.interrupted {
            Some(2)
        } else {
            output
                .exit_code
                .filter(|exit_code| *exit_code > 128 && *exit_code < 256)
                .map(|exit_code| exit_code - 128)
        }
    });
    let combined_output = format!("{}{}", output.stdout, output.stderr);
    let structured = shell_action_structured_content_json(
        action,
        &plan,
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
    );
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
    let content = if combined_output.is_empty() {
        Vec::new()
    } else {
        vec![combined_output]
    };
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
        return Ok(ActionResult::succeeded(
            turn,
            action,
            content,
            Some(structured),
        ));
    }
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

/// Builds a bounded patch preview for successful silent apply-patch commands.
fn patch_change_preview(patch: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 160;
    let mut lines = vec!["diff -- apply patch".to_string()];
    lines.extend(patch.lines().take(MAX_PREVIEW_LINES).map(ToOwned::to_owned));
    let total_lines = patch.lines().count();
    if total_lines > MAX_PREVIEW_LINES {
        lines.push(format!(
            "[mez: diff truncated; {} lines omitted]",
            total_lines - MAX_PREVIEW_LINES
        ));
    }
    lines.join("\n") + "\n"
}

/// Converts JSON-encoded MCP content blocks into canonical action content.
///
/// Non-array, malformed, or unsupported payloads remain visible as one text
/// block so a concrete MCP transport cannot silently discard tool output.
pub fn action_content_blocks_from_json_or_text(content_json: &str) -> Vec<ActionContentBlock> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content_json) else {
        return vec![ActionContentBlock::text(content_json.to_string())];
    };
    let Some(items) = value.as_array() else {
        return vec![ActionContentBlock::text(content_json.to_string())];
    };
    let blocks = items
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(serde_json::Value::as_str)?;
            if item_type != "text" {
                return None;
            }
            let text = item.get("text").and_then(serde_json::Value::as_str)?;
            Some(ActionContentBlock::text(text.to_string()))
        })
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        vec![ActionContentBlock::text(content_json.to_string())]
    } else {
        blocks
    }
}

/// Converts one concrete MCP response into its canonical action result.
///
/// Transport execution remains product-owned. This function owns only record
/// projection from the lower MCP request/response contracts into the lower
/// action-result contract.
pub fn mcp_response_to_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    request: &McpExecutionRequest,
    response: McpExecutionResponse,
) -> ActionResultContractResult<ActionResult> {
    let content_json = response.content_json.clone();
    let server_json = serde_json::Value::String(request.server_id.clone()).to_string();
    let tool_json = serde_json::Value::String(request.tool_name.clone()).to_string();
    let structured_payload = format!(
        r#"{{"server":{server_json},"tool":{tool_json},"content":{content_json},"structured_content":{},"is_error":{}}}"#,
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

/// One request to execute a rendered shell transaction through a pane adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecutionRequest {
    /// Stable action identity associated with the transaction.
    pub action_id: String,
    /// Fully rendered transaction contract for the target pane shell.
    pub transaction: ShellTransaction,
    /// Optional execution timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Whether execution may require direct user interaction.
    pub interactive: bool,
    /// Whether the command mutates the active pane shell state.
    pub stateful: bool,
}

/// Normalized output observed by a pane shell execution adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecutionOutput {
    /// Process exit code when one was observed.
    pub exit_code: Option<i32>,
    /// Native process signal when the executor can observe signal termination.
    pub signal: Option<i32>,
    /// Captured standard output or combined pane output.
    pub stdout: String,
    /// Captured standard error when the adapter separates it.
    pub stderr: String,
    /// Whether the execution exceeded its timeout.
    pub timed_out: bool,
    /// Whether runtime cancellation interrupted execution.
    pub interrupted: bool,
    /// Shell-output transport framing diagnostics.
    pub transport_diagnostics: ShellTransportDiagnostics,
}

impl ShellExecutionOutput {
    /// Builds shell output with no signal or transport diagnostics.
    pub fn new(
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        timed_out: bool,
        interrupted: bool,
    ) -> Self {
        Self {
            exit_code,
            signal: None,
            stdout,
            stderr,
            timed_out,
            interrupted,
            transport_diagnostics: ShellTransportDiagnostics::default(),
        }
    }
}

/// Port implemented by a concrete pane shell transaction executor.
pub trait PaneShellExecutor {
    /// Product-specific shell execution failure.
    type Error: Error;

    /// Executes one shell transaction and returns normalized output.
    fn execute_shell(
        &mut self,
        request: &ShellExecutionRequest,
    ) -> Result<ShellExecutionOutput, Self::Error>;
}

/// Runtime transport selected for one planned local action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalExecutionTransport {
    /// Dispatch the local action through the active pane shell.
    PaneShell,
}

impl LocalExecutionTransport {
    /// Returns the stable transport name recorded in action-result metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PaneShell => "pane_shell",
        }
    }
}

/// Transport-neutral request to execute one planned local action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExecutionRequest {
    /// Stable action identity.
    pub action_id: String,
    /// Original MAAP action whose model-facing shape remains unchanged.
    pub action: AgentAction,
    /// Active turn identity used by transaction adapters.
    pub turn_id: String,
    /// Active agent identity used by transaction adapters.
    pub agent_id: String,
    /// Target pane identity used by transaction adapters.
    pub pane_id: String,
    /// Lowered local action semantics accepted before dispatch.
    pub plan: LocalActionPlan,
    /// Effective finite timeout after applying the turn budget.
    pub effective_timeout_ms: u64,
    /// Runtime transport selected for this action.
    pub transport: LocalExecutionTransport,
    /// Marker used by transports that need command-output boundaries.
    pub marker: MarkerToken,
}

/// Transport-neutral output from one planned local action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExecutionOutput {
    /// Runtime transport that produced this output.
    pub transport: LocalExecutionTransport,
    /// Whether execution sent input to the active pane.
    pub sent_to_pane: bool,
    /// Shell-shaped output returned by the selected transport.
    pub shell_output: ShellExecutionOutput,
}

impl LocalExecutionOutput {
    /// Builds output produced by a pane shell adapter.
    pub fn pane_shell(shell_output: ShellExecutionOutput) -> Self {
        Self {
            transport: LocalExecutionTransport::PaneShell,
            sent_to_pane: true,
            shell_output,
        }
    }
}

/// Port implemented by a concrete local action transport.
pub trait LocalActionExecutor {
    /// Product-specific local action execution failure.
    type Error: Error;

    /// Reports the transport used by this executor.
    fn transport(&self) -> LocalExecutionTransport;

    /// Executes one already-planned local action.
    fn execute_local_action(
        &mut self,
        request: &LocalExecutionRequest,
    ) -> Result<LocalExecutionOutput, Self::Error>;
}

/// Synchronous port implemented by a concrete MCP runtime transport.
pub trait McpActionExecutor {
    /// Product-specific MCP execution failure.
    type Error: Error;

    /// Executes one approved MCP request.
    fn execute_mcp_call(
        &mut self,
        request: &McpExecutionRequest,
    ) -> Result<McpExecutionResponse, Self::Error>;
}

/// Asynchronous port implemented by a concrete MCP runtime transport.
#[allow(async_fn_in_trait)]
pub trait AsyncMcpActionExecutor {
    /// Product-specific MCP execution failure.
    type Error: Error;

    /// Executes one approved MCP request asynchronously.
    async fn execute_mcp_call_async(
        &mut self,
        request: &McpExecutionRequest,
    ) -> Result<McpExecutionResponse, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentActionPayload, AgentTurnRecord, AgentTurnState, AgentTurnTrigger};

    /// Builds one turn fixture for execution result projection.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds one MCP action fixture for response projection.
    fn action() -> AgentAction {
        AgentAction {
            id: "mcp-1".to_string(),
            rationale: "inspect remote state".to_string(),
            payload: AgentActionPayload::McpCall {
                server: "issues".to_string(),
                tool: "query".to_string(),
                arguments_json: "{}".to_string(),
            },
        }
    }

    /// Builds one shell action fixture for timeout and result projection.
    fn shell_action() -> AgentAction {
        AgentAction {
            id: "shell-1".to_string(),
            rationale: "inspect state".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "inspect state".to_string(),
                command: "false".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(2_000),
            },
        }
    }

    /// Execution records preserve explicit transport and normalized shell
    /// outcome state without requiring a product adapter.
    #[test]
    fn execution_records_preserve_transport_contract() {
        let output = LocalExecutionOutput::pane_shell(ShellExecutionOutput::new(
            Some(0),
            "ok".to_string(),
            String::new(),
            false,
            false,
        ));

        assert_eq!(output.transport, LocalExecutionTransport::PaneShell);
        assert_eq!(output.transport.as_str(), "pane_shell");
        assert!(output.sent_to_pane);
        assert_eq!(output.shell_output.exit_code, Some(0));
    }

    /// Verifies shell timeouts use one lower-owned turn budget for synthetic,
    /// active, expired, and explicitly bounded execution requests.
    #[test]
    fn shell_timeout_policy_bounds_requests_by_remaining_turn_budget() {
        assert_eq!(
            agent_shell_timeout_ms(1, 2_000_000_000_000, None),
            DEFAULT_AGENT_TURN_TIMEOUT_MS
        );
        assert_eq!(
            agent_shell_timeout_ms(2_000_000_000, 2_000_000_010_000, Some(2_000)),
            2_000
        );
        assert_eq!(
            agent_shell_timeout_ms(2_000_000_000, 2_001_800_000_000, None),
            1
        );
    }

    /// Verifies shell transport output becomes one canonical failed action
    /// result with stable status, content, and structured observation fields.
    #[test]
    fn local_shell_output_projection_is_provider_independent() {
        let result = local_execution_output_to_action_result(
            &turn(),
            &shell_action(),
            LocalExecutionOutput::pane_shell(ShellExecutionOutput::new(
                Some(2),
                "failure detail".to_string(),
                String::new(),
                false,
                false,
            )),
            &MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap(),
        )
        .unwrap();

        assert_eq!(result.status, ActionStatus::Succeeded);
        assert_eq!(result.content_text(), "failure detail");
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(structured["execution_transport"], "pane_shell");
        assert_eq!(structured["terminal_observation"]["exit_code"], 2);
    }

    /// Verifies successful MCP responses preserve text blocks and structured
    /// request/response metadata in one canonical action result.
    #[test]
    fn mcp_response_projection_preserves_canonical_content() {
        let result = mcp_response_to_action_result(
            &turn(),
            &action(),
            &McpExecutionRequest {
                server_id: "issues".to_string(),
                tool_name: "query".to_string(),
                arguments_json: "{}".to_string(),
                timeout_ms: 1_000,
            },
            McpExecutionResponse {
                content_json: r#"[{"type":"text","text":"found one"}]"#.to_string(),
                structured_content_json: Some(r#"{"count":1}"#.to_string()),
                is_error: false,
            },
        )
        .unwrap();

        assert_eq!(result.status, ActionStatus::Succeeded);
        assert_eq!(result.content_text(), "found one");
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(structured["server"], "issues");
        assert_eq!(structured["structured_content"]["count"], 1);
    }

    /// Verifies error MCP responses become failed results while malformed
    /// content remains visible as a fallback text block.
    #[test]
    fn mcp_response_projection_preserves_error_fallback_text() {
        let result = mcp_response_to_action_result(
            &turn(),
            &action(),
            &McpExecutionRequest {
                server_id: "issues".to_string(),
                tool_name: "query".to_string(),
                arguments_json: "{}".to_string(),
                timeout_ms: 1_000,
            },
            McpExecutionResponse {
                content_json: "not-json".to_string(),
                structured_content_json: None,
                is_error: true,
            },
        )
        .unwrap();

        assert_eq!(result.status, ActionStatus::Failed);
        assert_eq!(result.content_text(), "not-json");
        assert_eq!(result.error.unwrap().code, "mcp_tool_error");
    }

    /// Verifies retained MCP execution requests must preserve the exact action
    /// target and normalized argument payload before transport dispatch.
    #[test]
    fn mcp_execution_request_validation_rejects_stale_plans() {
        let request = McpExecutionRequest {
            server_id: "issues".to_string(),
            tool_name: "query".to_string(),
            arguments_json: "{}".to_string(),
            timeout_ms: 1_000,
        };
        validate_mcp_execution_request(&action(), &request).unwrap();

        let mut stale = request;
        stale.tool_name = "delete".to_string();
        assert!(validate_mcp_execution_request(&action(), &stale).is_err());
        assert!(validate_mcp_execution_request(&shell_action(), &stale).is_err());
    }
}
