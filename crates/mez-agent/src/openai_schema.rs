//! OpenAI request-specific MAAP schema construction.
//!
//! This module owns cache-stable OpenAI tool-shape policy and concrete
//! function-tool envelope construction over the canonical MAAP schema. The
//! compact request-state suffix and runtime validation remain authoritative
//! for the actions and MCP tools eligible on an individual request.

use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, ModelRequest,
    maap_action_batch_schema, normalize_openai_strict_schema,
};

/// Builds the OpenAI MAAP function-tool list for the current request.
pub(crate) fn openai_maap_action_batch_tools(request: &ModelRequest) -> Vec<serde_json::Value> {
    vec![openai_maap_current_action_batch_tool(request)]
}

/// Builds the canonical OpenAI Responses MAAP action-batch tool.
///
/// A single current-schema tool keeps provider-visible action selection simple:
/// the model chooses the best action object inside one batch instead of first
/// reasoning about a surface-specific wrapper function name.
fn openai_maap_current_action_batch_tool(request: &ModelRequest) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
        "description": crate::schema::maap_cache_stable_action_batch_description(),
        "strict": true,
        "parameters": normalize_openai_strict_schema(maap_action_batch_schema(
            &request.allowed_actions,
            &request.available_mcp_tools,
        ))
    })
}

/// Returns the provider-facing description for the current MAAP action-batch tool.
pub fn openai_maap_current_action_batch_description(request: &ModelRequest) -> String {
    crate::schema::maap_current_action_batch_description(
        &request.allowed_actions,
        &request.available_mcp_tools,
    )
}
