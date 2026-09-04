//! Runtime agent MCP discovery action helpers.
//!
//! This module executes the fixed read-only `mcp_server_search` and
//! `mcp_server_get` MAAP actions against the live registry. It returns only
//! safe server metadata and never starts a transport or grants tool authority.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, MezError, Result, RuntimeSessionService,
    runtime_agent_action_summary, runtime_agent_turn_state_from_action_results,
    runtime_mezzanine_error_code,
};

impl RuntimeSessionService {
    /// Executes pending fixed MCP discovery actions for one running turn.
    pub(crate) fn execute_running_mcp_discovery_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "mcp_server_search" | "mcp_server_get"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP discovery result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "MCP server discovery".to_string())
                    ),
                )?;
            }
            let referencable = match &action.payload {
                AgentActionPayload::McpServerGet { server } => {
                    self.agent_turn_contexts()
                        .get(&turn.turn_id)
                        .is_some_and(|context| {
                            mez_agent::mcp_server_is_referencable(context, server)
                        })
                        || execution.action_results[..index].iter().any(|result| {
                            mez_agent::mcp_server_is_referencable_from_action_result(result, server)
                        })
                }
                _ => true,
            };
            execution.action_results[index] =
                self.execute_mcp_discovery_action(turn, &action, referencable)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(executed)
    }

    /// Projects one fixed MCP discovery action into a settled safe result.
    fn execute_mcp_discovery_action(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        referencable: bool,
    ) -> Result<ActionResult> {
        match &action.payload {
            AgentActionPayload::McpServerSearch { query, limit } => {
                let limit = limit.unwrap_or(10) as usize;
                let servers = self.mcp_registry().search_agent_shell_servers(query, limit);
                let structured = serde_json::json!({
                    "query": query,
                    "servers": servers.iter().map(mcp_directory_record).collect::<Vec<_>>(),
                })
                .to_string();
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![format!(
                        "MCP server search returned {} result(s)",
                        servers.len()
                    )],
                    Some(structured),
                ))
            }
            AgentActionPayload::McpServerGet { server } => {
                if !referencable {
                    let error = MezError::forbidden(
                        "MCP server must be returned by mcp_server_search or durable MCP directory context before retrieval",
                    );
                    return Ok(ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Rejected,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?);
                }
                let summary = match self.mcp_registry().agent_shell_server_summary(server) {
                    Ok(summary) => summary,
                    Err(error) => {
                        let error = MezError::from(error);
                        return Ok(ActionResult::failed(
                            turn,
                            action,
                            ActionStatus::Failed,
                            runtime_mezzanine_error_code(error.kind()),
                            error.message().to_string(),
                        )?);
                    }
                };
                let structured = serde_json::json!({
                    "server": mcp_server_record(&summary),
                })
                .to_string();
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![format!(
                        "MCP server metadata retrieved for {}",
                        summary.server_id
                    )],
                    Some(structured),
                ))
            }
            _ => Err(MezError::invalid_args(
                "MCP discovery execution requires an mcp_server_search or mcp_server_get action",
            )),
        }
    }
}

/// Serializes compact safe directory metadata for MCP server search results.
fn mcp_directory_record(server: &mez_agent::AgentShellMcpServerSummary) -> serde_json::Value {
    serde_json::json!({
        "server_id": server.server_id,
        "display_name": server.display_name,
        "purpose": server.purpose,
        "usage_instructions": server.usage_instructions,
        "state": server.state,
    })
}

/// Serializes safe metadata for one MCP server lookup result.
fn mcp_server_record(server: &mez_agent::AgentShellMcpServerSummary) -> serde_json::Value {
    serde_json::json!({
        "server_id": server.server_id,
        "display_name": server.display_name,
        "purpose": server.purpose,
        "usage_instructions": server.usage_instructions,
        "state": server.state,
        "status": server.status,
        "enabled": server.enabled,
        "retryable": server.retryable,
        "reason": server.reason,
        "tools": server.tools.iter().map(|tool| serde_json::json!({
            "name": tool.name,
            "state": tool.state,
            "description": tool.description,
            "input_schema": serde_json::from_str::<serde_json::Value>(&tool.input_schema_json)
                .unwrap_or(serde_json::Value::Null),
        })).collect::<Vec<_>>(),
    })
}
