//! Shell and MCP action execution helpers.
//!
//! This module owns the boundary between planned agent actions and the
//! executor interfaces supplied by the runtime. It converts shell/MCP executor
//! outputs back into durable `ActionResult` values while keeping pane and MCP
//! I/O details out of turn negotiation.

use super::super::{
    ActionResult, AgentAction, AgentTurnRecord, McpExecutionRequest, MezError, Result,
};
#[cfg(test)]
use super::super::{
    DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature, MarkerToken, ToolDiscoveryCache,
    ToolInventory, tool_discovery_script,
};
#[cfg(test)]
use super::super::{Path, ShellTransaction, ShellTransactionOutputTransport};
use mez_agent::{AsyncMcpActionExecutor, McpActionExecutor, mcp_response_to_action_result};
#[cfg(test)]
use mez_agent::{
    LocalActionExecutor, LocalExecutionOutput, LocalExecutionRequest, LocalExecutionTransport,
    PaneShellExecutor, ShellExecutionRequest,
};
#[cfg(test)]
use mez_agent::{
    local_action_plan, local_execution_output_to_action_result, postprocess_local_shell_output,
};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Adapts the existing pane shell executor to the transport-neutral executor
/// contract.
#[cfg(test)]
pub struct PaneShellLocalExecutor<'a, E> {
    shell_path: &'a Path,
    pane_executor: &'a mut E,
}

#[cfg(test)]
impl<'a, E> PaneShellLocalExecutor<'a, E> {
    /// Builds an adapter around a pane shell executor and shell path.
    #[cfg(test)]
    pub fn new(shell_path: &'a Path, pane_executor: &'a mut E) -> Self {
        Self {
            shell_path,
            pane_executor,
        }
    }
}

#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
    let now_unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let effective_timeout_ms = mez_agent::agent_shell_timeout_ms(
        turn.started_at_unix_seconds,
        now_unix_millis,
        plan.timeout_ms,
    );
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
    output.shell_output = postprocess_local_shell_output(action, output.shell_output);
    Ok(local_execution_output_to_action_result(
        turn, action, output, &marker,
    )?)
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
    mez_agent::validate_mcp_execution_request(action, plan)?;

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
    mez_agent::validate_mcp_execution_request(action, plan)?;

    let response = executor.execute_mcp_call_async(plan).await?;
    Ok(mcp_response_to_action_result(turn, action, plan, response)?)
}

/// Executes the `discover_tools_through_pane_shell` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
#[cfg(test)]
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
