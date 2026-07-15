//! Product adapters for provider-neutral MAAP schema construction.
//!
//! `mez-agent` owns action-batch schemas, MCP argument normalization, provider
//! descriptions, and legacy tool-surface identifiers. This module retains only
//! request-specific OpenAI cache-shape policy and concrete function-tool
//! envelope construction.

use super::{AllowedAction, AllowedActionSet, ModelRequest, OPENAI_MAAP_FUNCTION_TOOL_NAME};

use mez_agent::maap_action_batch_schema;

/// Builds the OpenAI MAAP function-tool list for the current request.
pub(super) fn openai_maap_action_batch_tools(request: &ModelRequest) -> Vec<serde_json::Value> {
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
        "description": maap_current_action_batch_description(request),
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
pub(super) fn maap_current_action_batch_description(request: &ModelRequest) -> String {
    mez_agent::maap_current_action_batch_description(
        &request.allowed_actions,
        &request.available_mcp_tools,
    )
}

/// Returns the legacy OpenAI export name for the shared current-action-batch description.
pub(super) fn openai_maap_current_action_batch_description(request: &ModelRequest) -> String {
    maap_current_action_batch_description(request)
}
