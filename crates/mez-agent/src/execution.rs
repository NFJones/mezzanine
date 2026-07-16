//! Provider-independent action execution ports and transport records.
//!
//! The agent harness uses these contracts to request local shell and MCP work
//! without owning pane I/O, MCP transports, filesystem access, or product
//! errors. Port implementations retain their native error type through an
//! associated type so the composition crate can project failures once at its
//! boundary.

use std::error::Error;

use crate::{
    ActionContentBlock, ActionResult, ActionResultContractResult, ActionStatus, AgentAction,
    AgentTurnResultIdentity, LocalActionPlan, MarkerToken, McpExecutionRequest,
    McpExecutionResponse, ShellTransaction, ShellTransportDiagnostics,
};

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
}
