//! OpenAI request-specific MAAP schema construction.
//!
//! This module owns cache-stable OpenAI tool-shape policy and concrete
//! function-tool envelope construction over the canonical MAAP schema. The
//! compact request-state suffix and runtime validation remain authoritative
//! for the actions and MCP tools eligible on an individual request.

use crate::{
    AllowedAction, AllowedActionSet, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
    ModelRequest, maap_action_batch_schema,
};

/// Builds the OpenAI MAAP function-tool list for the current request.
pub(crate) fn openai_maap_action_batch_tools(request: &ModelRequest) -> Vec<serde_json::Value> {
    let _ = request;
    vec![openai_maap_current_action_batch_tool()]
}

/// Builds the canonical OpenAI Responses MAAP action-batch tool.
///
/// A single current-schema tool keeps provider-visible action selection simple:
/// the model chooses the best action object inside one batch instead of first
/// reasoning about a surface-specific wrapper function name.
fn openai_maap_current_action_batch_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
        "description": crate::schema::maap_cache_stable_action_batch_description(),
        "strict": true,
        "parameters": openai_cache_stable_action_batch_schema()
    })
}

/// Returns the provider-visible OpenAI action schema surface.
///
/// OpenAI prompt caching is sensitive to request-body tool bytes, so normal
/// action turns use one stable superset and rely on compact request state plus
/// runtime validation for actual eligibility.
fn openai_stable_schema_action_surface() -> AllowedActionSet {
    AllowedActionSet::from_actions([
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
        AllowedAction::McpCall,
    ])
}

/// Builds the byte-stable OpenAI schema with one generic MCP action variant.
///
/// The canonical schema shape below is produced in this crate and therefore
/// guarantees the documented action-union path. MCP arguments use compact JSON
/// text so third-party tool schemas cannot change provider-visible tool bytes;
/// canonical parsing normalizes that text before active-tool validation.
fn openai_cache_stable_action_batch_schema() -> serde_json::Value {
    let mut schema = maap_action_batch_schema(&openai_stable_schema_action_surface(), &[]);
    let action_variants = schema
        .pointer_mut("/properties/actions/items/anyOf")
        .and_then(serde_json::Value::as_array_mut)
        .expect("canonical MAAP action-batch schema always contains an action union");
    action_variants.push(crate::schema::maap_generic_mcp_call_action_schema());
    schema
}

/// Returns the provider-facing description for the current MAAP action-batch tool.
pub fn openai_maap_current_action_batch_description(request: &ModelRequest) -> String {
    crate::schema::maap_current_action_batch_description(
        &request.allowed_actions,
        &request.available_mcp_tools,
    )
}
