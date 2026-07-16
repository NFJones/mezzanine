//! Product adapters and formatting helpers for the MAAP contract.
//!
//! Canonical action batches, parsing, and deterministic validation live in
//! `mez-agent`. This module supplies the product-owned shell-input policy and
//! retains small formatting helpers used by provider and execution adapters.

use super::{AgentTurnRecord, McpPromptTool, MezError, Result};
use mez_agent::validate_agent_authored_shell_command;
use mez_agent::{MaapBatch, MaapContractError, MaapValidationContext};

/// Adds product-owned validation inputs to the canonical MAAP batch contract.
pub(crate) trait MaapBatchProductValidation {
    /// Validates this batch against the active turn, MCP manifest, and shell
    /// command policy owned by the Mezzanine composition root.
    fn validate(
        &self,
        turn: &AgentTurnRecord,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> Result<()>;
}

impl MaapBatchProductValidation for MaapBatch {
    fn validate(
        &self,
        turn: &AgentTurnRecord,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> Result<()> {
        let validate_shell_command = |command: &str| {
            validate_agent_authored_shell_command(command)
                .map_err(|error| MaapContractError::invalid_args(error.message()))
        };
        self.validate_contract(&MaapValidationContext {
            turn_id: &turn.turn_id,
            agent_id: &turn.agent_id,
            available_mcp_servers,
            available_mcp_tools,
            validate_shell_command: &validate_shell_command,
        })
        .map_err(MezError::from)
    }
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(MezError::invalid_args(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}
