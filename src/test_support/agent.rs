//! Agent test builders and fakes.
//!
//! The agent and runtime tests construct many equivalent model profiles,
//! requests, MAAP batches, and actions. These builders centralize defaults so
//! tests can override only the behavior they are asserting.

use std::collections::BTreeMap;

use crate::agent::{ModelProfile, ModelRequest};
use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, AgentAction, AgentActionPayload, AgentCapability,
    AllowedActionSet, MaapBatch, McpPromptTool, ModelInteractionKind, ModelMessage, SayStatus,
};

/// Builds a model profile with stable defaults for tests.
#[derive(Debug, Clone)]
pub(crate) struct ModelProfileBuilder {
    profile: ModelProfile,
}

impl ModelProfileBuilder {
    /// Creates an OpenAI model profile.
    pub(crate) fn openai(model: &str) -> Self {
        Self::new("openai", model)
    }

    /// Creates a DeepSeek model profile.
    pub(crate) fn deepseek(model: &str) -> Self {
        Self::new("deepseek", model)
    }

    /// Creates a model profile for any provider/model pair.
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            profile: ModelProfile {
                provider: provider.to_string(),
                model: model.to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: BTreeMap::new(),
                safety_tier: None,
            },
        }
    }

    /// Sets the provider reasoning profile.
    pub(crate) fn reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.profile.reasoning_profile = Some(reasoning.into());
        self
    }

    /// Sets the latency preference.
    pub(crate) fn latency(mut self, latency: impl Into<String>) -> Self {
        self.profile.latency_preference = Some(latency.into());
        self
    }

    /// Sets the safety tier.
    pub(crate) fn safety_tier(mut self, tier: impl Into<String>) -> Self {
        self.profile.safety_tier = Some(tier.into());
        self
    }

    /// Adds one provider option.
    pub(crate) fn option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.profile
            .provider_options
            .insert(key.into(), value.into());
        self
    }

    /// Marks the profile as requiring multimodal support.
    pub(crate) fn multimodal_required(mut self) -> Self {
        self.profile.multimodal_required = true;
        self
    }

    /// Returns the built profile.
    pub(crate) fn build(self) -> ModelProfile {
        self.profile
    }
}

/// Builds a provider request with compact defaults.
#[derive(Debug, Clone)]
pub(crate) struct ModelRequestBuilder {
    request: ModelRequest,
}

impl ModelRequestBuilder {
    /// Creates a default request for one test turn.
    pub(crate) fn default_turn() -> Self {
        Self {
            request: ModelRequest {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: ModelInteractionKind::ActionExecution,
                allowed_actions: AllowedActionSet::action_execution_base(),
                messages: Vec::new(),
            },
        }
    }

    /// Applies a model profile to the request.
    pub(crate) fn profile(mut self, profile: &ModelProfile) -> Self {
        self.request.provider = profile.provider.clone();
        self.request.model = profile.model.clone();
        self.request.reasoning_effort = profile.reasoning_profile.clone();
        self.request.latency_preference = profile.latency_preference.clone();
        self.request.max_output_tokens = profile.max_output_tokens();
        self
    }

    /// Sets the interaction kind.
    pub(crate) fn interaction_kind(mut self, kind: ModelInteractionKind) -> Self {
        self.request.interaction_kind = kind;
        self
    }

    /// Sets the allowed action surface.
    pub(crate) fn allowed_actions(mut self, allowed_actions: AllowedActionSet) -> Self {
        self.request.allowed_actions = allowed_actions;
        self
    }

    /// Adds one provider-bound message.
    pub(crate) fn message(mut self, message: ModelMessage) -> Self {
        self.request.messages.push(message);
        self
    }

    /// Sets available MCP tool summaries.
    pub(crate) fn mcp_tools(mut self, tools: Vec<McpPromptTool>) -> Self {
        self.request.available_mcp_tools = tools;
        self
    }

    /// Returns the built request.
    pub(crate) fn build(self) -> ModelRequest {
        self.request
    }
}

/// Builds a MAAP action batch.
#[derive(Debug, Clone)]
pub(crate) struct BatchBuilder {
    batch: MaapBatch,
}

impl BatchBuilder {
    /// Creates a final action batch with default identity.
    pub(crate) fn final_turn(actions: Vec<AgentAction>) -> Self {
        Self {
            batch: MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "exercise test actions".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                actions,
                final_turn: true,
            },
        }
    }

    /// Sets the batch rationale.
    pub(crate) fn rationale(mut self, rationale: impl Into<String>) -> Self {
        self.batch.rationale = rationale.into();
        self
    }

    /// Sets the durable batch thought.
    pub(crate) fn thought(mut self, thought: impl Into<String>) -> Self {
        self.batch.thought = Some(thought.into());
        self
    }

    /// Marks the batch as a non-final continuation.
    pub(crate) fn non_final(mut self) -> Self {
        self.batch.final_turn = false;
        self
    }

    /// Returns the built batch.
    pub(crate) fn build(self) -> MaapBatch {
        self.batch
    }
}

/// Builds common MAAP actions for tests.
#[derive(Debug, Clone)]
pub(crate) struct ActionBuilder;

impl ActionBuilder {
    /// Builds a shell command action.
    pub(crate) fn shell(id: &str) -> AgentAction {
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
    pub(crate) fn say(id: &str, text: &str) -> AgentAction {
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
    pub(crate) fn abort(id: &str, reason: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "stop the turn".to_string(),
            payload: AgentActionPayload::Abort {
                reason: reason.to_string(),
            },
        }
    }

    /// Builds an MCP tool call action.
    pub(crate) fn mcp(id: &str) -> AgentAction {
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
    pub(crate) fn config_change(id: &str) -> AgentAction {
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
    pub(crate) fn capability(id: &str, capability: AgentCapability) -> AgentAction {
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
