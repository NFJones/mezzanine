//! Agent action fixtures owned by the agent integration tests.
//!
//! These builders describe representative MAAP actions used by multiple
//! behavior-focused leaves in this test tree. They stay beside their sole test
//! owner instead of expanding the application-wide test-support surface.

use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, AgentAction, AgentActionPayload, AgentCapability,
    SayStatus,
};

/// Builds common MAAP actions for agent integration tests.
#[derive(Debug, Clone)]
pub(super) struct ActionBuilder;

impl ActionBuilder {
    /// Builds a shell command action.
    pub(super) fn shell(id: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "inspect current directory".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Inspect the current directory".to_string(),
                command: "pwd".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(1000),
            },
        }
    }

    /// Builds a final plain-text say action.
    pub(super) fn say(id: &str, text: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "reply to user".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: text.to_string(),
                content_type: AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        }
    }

    /// Builds an abort action.
    pub(super) fn abort(id: &str, reason: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "stop the turn".to_string(),
            payload: AgentActionPayload::Abort {
                reason: reason.to_string(),
            },
        }
    }

    /// Builds an MCP tool call action.
    pub(super) fn mcp(id: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "inspect external integration state".to_string(),
            payload: AgentActionPayload::McpCall {
                server: "state".to_string(),
                tool: "list".to_string(),
                arguments_json: r#"{"path":"."}"#.to_string(),
            },
        }
    }

    /// Builds a live configuration mutation action.
    pub(super) fn config_change(id: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "change the active theme".to_string(),
            payload: AgentActionPayload::ConfigChange {
                setting_path: "theme.active".to_string(),
                operation: "set".to_string(),
                value: Some("kanagawa".to_string()),
            },
        }
    }

    /// Builds a capability request action.
    pub(super) fn capability(id: &str, capability: AgentCapability) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "request the action surface needed for the task".to_string(),
            payload: AgentActionPayload::RequestCapability {
                capability,
                reason: format!("need {} actions for this test", capability.as_str()),
            },
        }
    }
}
