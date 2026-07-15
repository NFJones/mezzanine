//! OpenAI request-specific MAAP schema construction.
//!
//! This module owns request-specific OpenAI cache-shape policy and concrete
//! function-tool envelope construction over the canonical MAAP schema.

use crate::{
    AllowedAction, AllowedActionSet, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
    ModelRequest, maap_action_batch_schema,
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
        "description": openai_maap_current_action_batch_description(request),
        "strict": true,
        "parameters": maap_action_batch_schema(
            &openai_stable_schema_action_surface(request),
            &request.available_mcp_tools,
        )
    })
}

/// Returns the provider-visible OpenAI action schema surface.
///
/// OpenAI prompt caching is sensitive to request-body tool bytes, so ordinary
/// non-MCP requests use one stable superset schema and rely on the late
/// allowed-action surface plus runtime validation for actual eligibility. MCP
/// calls remain request-specific because callable tools and argument schemas
/// are part of the integration contract.
fn openai_stable_schema_action_surface(request: &ModelRequest) -> AllowedActionSet {
    let mut actions = AllowedActionSet::from_actions([
        AllowedAction::Say,
        AllowedAction::RequestCapability,
        AllowedAction::ShellCommand,
        AllowedAction::ApplyPatch,
        AllowedAction::WebSearch,
        AllowedAction::FetchUrl,
        AllowedAction::SendMessage,
        AllowedAction::SpawnAgent,
        AllowedAction::ConfigChange,
        AllowedAction::MemorySearch,
        AllowedAction::MemoryStore,
        AllowedAction::IssueAdd,
        AllowedAction::IssueUpdate,
        AllowedAction::IssueQuery,
        AllowedAction::IssueDelete,
    ]);
    if let Some(description) = request
        .allowed_actions
        .config_change_setting_path_description()
    {
        actions = actions.with_config_change_setting_path_description(description);
    }
    if request.allowed_actions.contains(AllowedAction::McpCall) {
        actions.extend([AllowedAction::McpCall]);
    }
    actions
}

/// Returns the provider-facing description for the current MAAP action-batch tool.
pub fn openai_maap_current_action_batch_description(request: &ModelRequest) -> String {
    crate::schema::maap_current_action_batch_description(
        &request.allowed_actions,
        &request.available_mcp_tools,
    )
}
