//! Provider-independent request envelope records.
//!
//! Requests carry canonical messages and scalar routing controls. Credentials,
//! transport execution, and product model-profile selection remain adapters.

use super::ModelMessages;
use crate::mcp::McpPromptTool;
use crate::surface::{AllowedActionSet, ModelInteractionKind};

/// One complete provider-independent model request.
///
/// The request carries only canonical agent contracts and scalar provider
/// options. Product model-profile selection, context assembly, credentials,
/// transport, and runtime state remain outside this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// Configured provider identity.
    pub provider: String,
    /// Provider model identity.
    pub model: String,
    /// Typed capabilities pinned when the selected profile was materialized.
    pub model_capabilities: crate::ModelCapabilities,
    /// Hard estimated cap on the complete provider-visible request input.
    pub max_input_tokens: Option<usize>,
    /// Provider reasoning effort, when configured for this request.
    pub reasoning_effort: Option<String>,
    /// Explicit thinking-mode override for providers that support it.
    pub thinking_enabled: Option<bool>,
    /// Provider-neutral latency or cost preference.
    pub latency_preference: Option<String>,
    /// Provider prompt-cache retention policy.
    pub prompt_cache_retention: Option<String>,
    /// Provider output-token cap.
    pub max_output_tokens: Option<usize>,
    /// Provider sampling temperature.
    pub temperature: Option<String>,
    /// Live product session identity used only for diagnostics.
    pub prompt_cache_session_id: Option<String>,
    /// Stable prompt-cache lineage identity.
    pub prompt_cache_lineage_id: Option<String>,
    /// Active agent turn identity.
    pub turn_id: String,
    /// Active agent identity.
    pub agent_id: String,
    /// MCP tools available to the request.
    pub available_mcp_tools: Vec<McpPromptTool>,
    /// Whether persistent-memory actions are enabled.
    pub memory_actions_enabled: bool,
    /// Whether local issue-tracking actions are enabled.
    pub issue_actions_enabled: bool,
    /// Provider interaction mode for the request.
    pub interaction_kind: ModelInteractionKind,
    /// Concrete MAAP action surface exposed to the provider.
    pub allowed_actions: AllowedActionSet,
    /// Provider stop sequences, when configured.
    pub stop: Option<Vec<String>>,
    /// Ordered provider-independent messages.
    pub messages: ModelMessages,
}
