//! Product adapters and formatting helpers for the MAAP contract.
//!
//! Canonical action batches, parsing, and deterministic validation live in
//! `mez-agent`. This module supplies the product-owned shell-input policy and
//! retains small formatting helpers used by provider and execution adapters.

use super::{AgentTurnRecord, McpPromptTool, MezError, Result};
use mez_agent::validate_agent_authored_shell_command;
use mez_agent::{ActionContentBlock, MaapBatch, MaapContractError, MaapValidationContext};

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

/// Runs the action content blocks from json or text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn action_content_blocks_from_json_or_text(
    content_json: &str,
) -> Vec<ActionContentBlock> {
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

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}
