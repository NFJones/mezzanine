//! Provider-independent action execution ports and transport records.
//!
//! The agent harness uses these contracts to request local shell and MCP work
//! without owning pane I/O, MCP transports, filesystem access, or product
//! errors. Port implementations retain their native error type through an
//! associated type so the composition crate can project failures once at its
//! boundary.

use std::error::Error;

use crate::{
    AgentAction, LocalActionPlan, MarkerToken, McpExecutionRequest, McpExecutionResponse,
    ShellTransaction, ShellTransportDiagnostics,
};

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
    use super::{LocalExecutionOutput, LocalExecutionTransport, ShellExecutionOutput};

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
}
