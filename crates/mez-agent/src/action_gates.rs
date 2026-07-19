//! Default provider-facing MAAP action-surface policy.
//!
//! This module applies concrete MCP, persistent-memory, and issue-tracking
//! availability to an assembled model request. Product runtime code supplies
//! live availability records, while this lower policy keeps provider request
//! shaping consistent across turn execution and diagnostics.

use crate::{AllowedAction, McpPromptTool, ModelRequest};

/// Applies concrete default action availability to an assembled model request.
pub fn apply_default_action_gates(
    request: &mut ModelRequest,
    available_mcp_tools: &[McpPromptTool],
    memory_actions_enabled: bool,
    issue_actions_enabled: bool,
) {
    request.available_mcp_tools = available_mcp_tools.to_vec();
    if !available_mcp_tools.is_empty() {
        request.allowed_actions.extend([AllowedAction::McpCall]);
    } else {
        request.allowed_actions.remove(AllowedAction::McpCall);
    }

    request.memory_actions_enabled = memory_actions_enabled;
    if memory_actions_enabled {
        request
            .allowed_actions
            .extend([AllowedAction::MemorySearch, AllowedAction::MemoryStore]);
    } else {
        request.allowed_actions.remove(AllowedAction::MemorySearch);
        request.allowed_actions.remove(AllowedAction::MemoryStore);
    }

    request.issue_actions_enabled = issue_actions_enabled;
    if !issue_actions_enabled {
        request.allowed_actions.remove(AllowedAction::IssueAdd);
        request.allowed_actions.remove(AllowedAction::IssueUpdate);
        request.allowed_actions.remove(AllowedAction::IssueQuery);
        request.allowed_actions.remove(AllowedAction::IssueDelete);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowedActionSet, ModelInteractionKind};

    /// Builds a minimal capability-decision request for action-gate policy tests.
    fn request() -> ModelRequest {
        ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::CapabilityDecision,
            allowed_actions: AllowedActionSet::capability_decision(),
            stop: None,
            messages: Vec::new(),
        }
    }

    /// Builds one representative callable MCP tool for action-gate tests.
    fn mcp_tool() -> McpPromptTool {
        McpPromptTool {
            server_id: "githubcopilot".to_string(),
            tool_name: "list_ci_results".to_string(),
            description: "Read GitHub CI check results for a repository.".to_string(),
            approval_required: false,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }
    }

    #[test]
    /// Verifies the shared default action-gate helper exposes the same concrete
    /// MCP and memory actions used by selected-model execution and provider
    /// request-shape diagnostics.
    fn default_action_gates_expose_mcp_and_memory_for_diagnostic_request_shapes() {
        let mut request = request();
        let tools = vec![mcp_tool()];

        apply_default_action_gates(&mut request, &tools, true, false);

        let allowed_actions = request.allowed_actions.action_type_names();
        assert!(allowed_actions.contains(&"mcp_call"));
        assert!(allowed_actions.contains(&"memory_search"));
        assert!(allowed_actions.contains(&"memory_store"));
        assert!(allowed_actions.contains(&"request_capability"));
        assert_eq!(request.available_mcp_tools, tools);
        assert!(request.memory_actions_enabled);
        assert!(!request.issue_actions_enabled);
    }

    #[test]
    /// Verifies available MCP tools do not suppress the persistent-memory surface.
    ///
    /// MCP availability is not a global reason to hide other enabled capabilities.
    /// This keeps memory usable for turns that legitimately need durable prior
    /// context even when MCP servers are configured.
    fn default_action_gates_keep_memory_when_mcp_is_available() {
        let mut request = request();
        let tool = mcp_tool();

        apply_default_action_gates(&mut request, std::slice::from_ref(&tool), true, false);

        let allowed_actions = request.allowed_actions.action_type_names();
        assert!(allowed_actions.contains(&"mcp_call"));
        assert!(allowed_actions.contains(&"memory_search"));
        assert!(allowed_actions.contains(&"memory_store"));
        assert!(allowed_actions.contains(&"request_capability"));
        assert_eq!(request.available_mcp_tools, vec![tool]);
        assert!(request.memory_actions_enabled);
    }

    #[test]
    /// Verifies live availability gates revoke actions retained from an earlier
    /// provider continuation when the underlying integration is no longer
    /// available.
    fn default_action_gates_revoke_unavailable_retained_actions() {
        let mut request = request();
        request.allowed_actions.extend([
            AllowedAction::McpCall,
            AllowedAction::MemorySearch,
            AllowedAction::MemoryStore,
            AllowedAction::IssueAdd,
            AllowedAction::IssueUpdate,
            AllowedAction::IssueQuery,
            AllowedAction::IssueDelete,
        ]);

        apply_default_action_gates(&mut request, &[], false, false);

        let allowed_actions = request.allowed_actions.action_type_names();
        assert!(!allowed_actions.contains(&"mcp_call"));
        assert!(!allowed_actions.contains(&"memory_search"));
        assert!(!allowed_actions.contains(&"memory_store"));
        assert!(!allowed_actions.contains(&"issue_add"));
        assert!(!allowed_actions.contains(&"issue_update"));
        assert!(!allowed_actions.contains(&"issue_query"));
        assert!(!allowed_actions.contains(&"issue_delete"));
        assert!(allowed_actions.contains(&"request_capability"));
    }
}
