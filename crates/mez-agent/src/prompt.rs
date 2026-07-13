//! Provider-neutral prompt profile contracts.
//!
//! This module owns the stable inputs used to assemble an agent system prompt.
//! Prompt assets and product-specific assembly remain in the composition crate.

use std::fmt;

use crate::McpPromptSummary;

/// Result type returned by provider-neutral prompt assembly contracts.
pub type AgentPromptResult<T> = Result<T, AgentPromptError>;

/// Stable categories for agent prompt assembly failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPromptErrorKind {
    /// A required provider-neutral prompt input was missing or malformed.
    InvalidArgs,
    /// A product-owned prompt asset was unavailable or invalid.
    InvalidState,
}

/// A typed failure returned while validating or assembling an agent prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptError {
    kind: AgentPromptErrorKind,
    message: String,
}

impl AgentPromptError {
    /// Creates an invalid-argument prompt failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: AgentPromptErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Creates an invalid-state prompt failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: AgentPromptErrorKind::InvalidState,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentPromptErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentPromptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentPromptError {}

/// Validates one required prompt-profile field after trimming whitespace.
pub fn validate_agent_prompt_required(field: &str, value: &str) -> AgentPromptResult<()> {
    if value.trim().is_empty() {
        return Err(AgentPromptError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

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
    use super::{
        AgentPromptError, AgentPromptErrorKind, AgentPromptProfile, validate_agent_prompt_required,
    };
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

    #[test]
    /// Verifies required prompt identity fields reject whitespace while prompt
    /// asset failures retain their distinct invalid-state category.
    fn prompt_errors_preserve_validation_and_asset_categories() {
        let error = validate_agent_prompt_required("agent id", " \t ").unwrap_err();
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidArgs);
        assert_eq!(error.message(), "agent id must not be empty");

        let error = AgentPromptError::invalid_state("prompt asset is missing");
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidState);
    }
}
