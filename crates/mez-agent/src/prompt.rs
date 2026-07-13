//! Provider-neutral prompt profile contracts.
//!
//! This module owns the stable inputs used to assemble an agent system prompt.
//! Prompt assets and product-specific assembly remain in the composition crate.

use crate::McpPromptSummary;

/// Stable name of the default agent prompt profile.
pub const AGENT_PROMPT_PROFILE_NAME: &str = "default";

/// Current version of the default agent prompt profile.
pub const AGENT_PROMPT_PROFILE_VERSION: u32 = 30;

/// Provider-neutral state used to assemble one agent system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptProfile {
    /// Stable agent identifier included in the prompt.
    pub agent_id: String,
    /// Stable pane identifier included in the prompt.
    pub pane_id: String,
    /// Optional provider kind used for provider-specific guidance.
    pub provider: Option<String>,
    /// Optional subagent cooperation mode.
    pub cooperation_mode: Option<String>,
    /// Declared read scopes for a subagent.
    pub read_scopes: Vec<String>,
    /// Declared write scopes for a subagent.
    pub write_scopes: Vec<String>,
    /// Secret-safe MCP manifest summary embedded in the prompt.
    pub mcp_summary: McpPromptSummary,
}

impl AgentPromptProfile {
    /// Creates the default prompt profile for one agent and pane.
    pub fn default_for(agent_id: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
            provider: None,
            cooperation_mode: None,
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            mcp_summary: McpPromptSummary {
                available_servers: Vec::new(),
                available_tools: Vec::new(),
                unavailable_servers: Vec::new(),
            },
        }
    }

    /// Sets the provider kind used for provider-specific prompt guidance.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Sets the MCP prompt summary embedded in the profile.
    pub fn with_mcp_summary(mut self, summary: McpPromptSummary) -> Self {
        self.mcp_summary = summary;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::AgentPromptProfile;
    use crate::{McpPromptServer, McpPromptSummary};

    #[test]
    /// Verifies a default profile contains only the required agent and pane
    /// identifiers and starts with empty optional integration state.
    fn prompt_profile_defaults_are_dependency_neutral() {
        let profile = AgentPromptProfile::default_for("agent-1", "%1");

        assert_eq!(profile.agent_id, "agent-1");
        assert_eq!(profile.pane_id, "%1");
        assert_eq!(profile.provider, None);
        assert!(profile.read_scopes.is_empty());
        assert!(profile.write_scopes.is_empty());
        assert!(profile.mcp_summary.available_servers.is_empty());
    }

    #[test]
    /// Verifies builder methods preserve the profile identity while replacing
    /// provider and MCP prompt context.
    fn prompt_profile_builders_preserve_identity() {
        let summary = McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "filesystem".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files".to_string(),
                usage_instructions: String::new(),
                tool_count: 0,
                approval_required_tool_count: 0,
            }],
            available_tools: Vec::new(),
            unavailable_servers: Vec::new(),
        };
        let profile = AgentPromptProfile::default_for("agent-1", "%1")
            .with_provider("anthropic")
            .with_mcp_summary(summary.clone());

        assert_eq!(profile.agent_id, "agent-1");
        assert_eq!(profile.pane_id, "%1");
        assert_eq!(profile.provider.as_deref(), Some("anthropic"));
        assert_eq!(profile.mcp_summary, summary);
    }
}
