//! Provider-independent agent context validation contracts.
//!
//! This module owns deterministic validation failures for context blocks and
//! model-profile selection. Product prompt assets, persistence, and provider
//! execution remain outside this crate and adapt these errors at their
//! composition boundaries.

use std::fmt;

use crate::mcp::McpPromptTool;
use crate::surface::{AllowedActionSet, ModelInteractionKind};
use crate::{AgentPromptError, AgentPromptErrorKind};

/// Identifies the provenance and stability class of one model-context value.
///
/// Providers use this contract to preserve role provenance, choose stable
/// prompt-cache prefixes, and keep volatile controller state out of reusable
/// request material without depending on product runtime types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSourceKind {
    /// Product system instructions.
    System,
    /// The active user-authored instruction.
    UserInstruction,
    /// Explicitly loaded skill instructions.
    SkillInstruction,
    /// Developer-authored instructions.
    DeveloperInstruction,
    /// Runtime policy context.
    Policy,
    /// Product configuration context.
    Configuration,
    /// A local agent-to-agent message.
    LocalMessage,
    /// Runtime-generated controller guidance or state.
    RuntimeHint,
    /// Repository or project guidance.
    ProjectGuidance,
    /// Retrieved durable memory context.
    Memory,
    /// A legacy or role-neutral transcript entry.
    Transcript,
    /// A prior user-authored transcript entry.
    TranscriptUser,
    /// A prior assistant-authored transcript entry.
    TranscriptAssistant,
    /// A prior tool or action transcript entry.
    TranscriptTool,
    /// A compact ledger of evidence already gathered in the active turn.
    EvidenceLedger,
    /// Immutable evidence promoted from settled turn actions.
    CommittedEvidence,
    /// A current-turn action result.
    ActionResult,
}

/// Trust domain assigned to one model-context block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDomain {
    /// User-provided instructions or agent-to-agent messages.
    UserInput,
    /// Project instruction files discovered through the product adapter.
    ProjectFile,
    /// Configuration, policy, and system instructions.
    Configuration,
    /// External web or API content retrieved by the agent.
    WebContent,
    /// Previous model responses and action results.
    ModelOutput,
}

impl TrustDomain {
    /// Derives the trust domain for one context provenance class.
    pub fn for_source(source: ContextSourceKind) -> Self {
        match source {
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration => Self::Configuration,
            ContextSourceKind::UserInstruction | ContextSourceKind::LocalMessage => Self::UserInput,
            ContextSourceKind::SkillInstruction | ContextSourceKind::ProjectGuidance => {
                Self::ProjectFile
            }
            ContextSourceKind::RuntimeHint => Self::Configuration,
            ContextSourceKind::Memory | ContextSourceKind::TranscriptUser => Self::UserInput,
            ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult => Self::ModelOutput,
        }
    }

    /// Returns whether providers must treat this domain as untrusted by default.
    pub fn is_untrusted_by_default(self) -> bool {
        matches!(self, Self::ProjectFile | Self::WebContent)
    }

    /// Returns the stable prompt annotation for this trust domain.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserInput => "user-input",
            Self::ProjectFile => "project-file",
            Self::Configuration => "configuration",
            Self::WebContent => "web-content",
            Self::ModelOutput => "model-output",
        }
    }
}

/// Stability class used for provider prompt-cache grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextStability {
    /// Static product instructions or configuration.
    Static,
    /// Guidance scoped to repository contents.
    RepoScoped,
    /// Session-scoped summaries, transcripts, or memory.
    SessionStable,
    /// State that may change on every agent turn.
    TurnVolatile,
}

/// Provider prompt-cache eligibility for one context block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCachePolicy {
    /// The block may appear in a reusable provider prefix.
    Eligible,
    /// The block must remain outside reusable prefix calculations.
    Ineligible,
    /// The block may establish a provider-specific cache breakpoint.
    ProviderBreakpoint,
}

/// One ordered unit of model-visible context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    /// Provenance and role class for the block.
    pub source: ContextSourceKind,
    /// Human-readable block label used in provider message framing.
    pub label: String,
    /// Exact model-visible block contents.
    pub content: String,
}

impl ContextBlock {
    /// Returns the block's derived trust domain.
    pub fn trust_domain(&self) -> TrustDomain {
        TrustDomain::for_source(self.source)
    }

    /// Returns the provider-cache stability class for this block.
    pub fn stability(&self) -> ContextStability {
        if self.source == ContextSourceKind::Policy && self.label == "scheduler state" {
            return ContextStability::TurnVolatile;
        }
        if self.source == ContextSourceKind::Configuration
            && configuration_context_is_turn_volatile(&self.label)
        {
            return ContextStability::TurnVolatile;
        }
        match self.source {
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration => ContextStability::Static,
            ContextSourceKind::ProjectGuidance => ContextStability::RepoScoped,
            ContextSourceKind::Memory
            | ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence => ContextStability::SessionStable,
            ContextSourceKind::UserInstruction
            | ContextSourceKind::SkillInstruction
            | ContextSourceKind::LocalMessage
            | ContextSourceKind::RuntimeHint
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::ActionResult => ContextStability::TurnVolatile,
        }
    }

    /// Returns the provider-cache policy for this block.
    pub fn cache_policy(&self) -> ContextCachePolicy {
        match self.source {
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration
            | ContextSourceKind::ProjectGuidance
            | ContextSourceKind::Memory
            | ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence => {
                if self.stability() == ContextStability::TurnVolatile {
                    ContextCachePolicy::Ineligible
                } else if self.source == ContextSourceKind::ProjectGuidance {
                    ContextCachePolicy::ProviderBreakpoint
                } else {
                    ContextCachePolicy::Eligible
                }
            }
            ContextSourceKind::UserInstruction
            | ContextSourceKind::SkillInstruction
            | ContextSourceKind::LocalMessage
            | ContextSourceKind::RuntimeHint
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::ActionResult => ContextCachePolicy::Ineligible,
        }
    }

    /// Returns whether the block may participate in a reusable prefix.
    pub fn stable_prefix_eligible(&self) -> bool {
        self.cache_policy() != ContextCachePolicy::Ineligible
            && self.stability() != ContextStability::TurnVolatile
    }

    /// Returns whether exact content can be recovered outside model context.
    pub fn recoverable_for_compaction(&self) -> bool {
        matches!(
            self.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
                | ContextSourceKind::EvidenceLedger
                | ContextSourceKind::CommittedEvidence
                | ContextSourceKind::RuntimeHint
                | ContextSourceKind::ActionResult
                | ContextSourceKind::LocalMessage
        )
    }
}

/// Ordered context supplied to provider request assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    /// Ordered model-context blocks.
    pub blocks: Vec<ContextBlock>,
}

impl AgentContext {
    /// Creates validated non-empty agent context.
    pub fn new(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        if blocks.is_empty() {
            return Err(AgentContextError::new(
                "agent context must contain at least one context block",
            ));
        }
        for block in &blocks {
            validate_context_required("context label", &block.label)?;
        }
        Ok(Self { blocks })
    }
}

/// Counts deterministic compaction performed on provider-bound context.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModelContextCompactionReport {
    /// Number of blocks replaced with compact local summaries.
    pub compacted_blocks: usize,
    /// Number of compacted blocks omitted after summaries exceeded budget.
    pub omitted_blocks: usize,
    /// Original estimated words represented by omitted blocks.
    pub omitted_original_words: usize,
}

impl ModelContextCompactionReport {
    /// Returns whether provider context changed during compaction.
    pub fn changed(self) -> bool {
        self.compacted_blocks > 0 || self.omitted_blocks > 0
    }
}

/// Builds the bracketed provider-message header for one context block.
pub fn model_context_block_header(block: &ContextBlock) -> String {
    let trust = block.trust_domain();
    let domain_annotation = if trust.is_untrusted_by_default() {
        format!(" [untrusted:{}]", trust.as_str())
    } else {
        String::new()
    };
    format!("[{}{}]\n", block.label, domain_annotation)
}

/// Returns whether a configuration value is turn-volatile cache material.
fn configuration_context_is_turn_volatile(label: &str) -> bool {
    label == "session identity"
        || label == "pane identity"
        || label == "provider output-limit retry guidance"
        || label.starts_with("environment signature for pane ")
}

/// Provider-independent role of one model message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMessageRole {
    /// System-level instructions.
    System,
    /// Developer-level instructions.
    Developer,
    /// User-authored input.
    User,
    /// Prior assistant output.
    Assistant,
    /// Tool or action evidence.
    Tool,
}

/// Provider-independent message supplied to model request rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMessage {
    /// Provider-facing role of the message.
    pub role: ModelMessageRole,
    /// Provenance and stability class of the message.
    pub source: ContextSourceKind,
    /// Model-visible message content.
    pub content: String,
}

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
    pub messages: Vec<ModelMessage>,
}

/// Result type returned by deterministic agent-context operations.
pub type AgentContextResult<T> = Result<T, AgentContextError>;

/// Result type returned while assembling one provider model request.
pub type AgentRequestAssemblyResult<T> = Result<T, AgentRequestAssemblyError>;

/// Stable categories for provider-independent request assembly failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRequestAssemblyErrorKind {
    /// A required request, context, or prompt-profile input was malformed.
    InvalidArgs,
    /// A product-supplied prompt asset was unavailable or invalid.
    InvalidState,
}

/// A typed failure returned while assembling one provider model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequestAssemblyError {
    kind: AgentRequestAssemblyErrorKind,
    message: String,
}

impl AgentRequestAssemblyError {
    /// Returns the stable request-assembly failure category.
    pub fn kind(&self) -> AgentRequestAssemblyErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<AgentContextError> for AgentRequestAssemblyError {
    fn from(error: AgentContextError) -> Self {
        Self {
            kind: AgentRequestAssemblyErrorKind::InvalidArgs,
            message: error.to_string(),
        }
    }
}

impl From<AgentPromptError> for AgentRequestAssemblyError {
    fn from(error: AgentPromptError) -> Self {
        let kind = match error.kind() {
            AgentPromptErrorKind::InvalidArgs => AgentRequestAssemblyErrorKind::InvalidArgs,
            AgentPromptErrorKind::InvalidState => AgentRequestAssemblyErrorKind::InvalidState,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for AgentRequestAssemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentRequestAssemblyError {}

/// A malformed provider-independent agent-context value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContextError {
    message: String,
}

impl AgentContextError {
    /// Creates a context contract error with a stable diagnostic message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentContextError {}

/// Validates one required context field after trimming surrounding whitespace.
pub fn validate_context_required(field: &str, value: &str) -> AgentContextResult<()> {
    if value.trim().is_empty() {
        return Err(AgentContextError::new(format!("{field} must not be empty")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentContextError, AgentRequestAssemblyError, AgentRequestAssemblyErrorKind, ContextBlock,
        ContextCachePolicy, ContextSourceKind, ContextStability, validate_context_required,
    };
    use crate::AgentPromptError;

    /// Verifies context blocks expose cache-stability metadata without changing
    /// the stored source, label, and content shape.
    #[test]
    fn context_block_cache_metadata_classifies_stable_and_volatile_sources() {
        let project = ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            label: "project guidance".to_string(),
            content: "follow repo guidance".to_string(),
        };
        let scheduler = ContextBlock {
            source: ContextSourceKind::Policy,
            label: "scheduler state".to_string(),
            content: "state=idle".to_string(),
        };
        let action = ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "action result".to_string(),
            content: "command output".to_string(),
        };
        let transcript_tool = ContextBlock {
            source: ContextSourceKind::TranscriptTool,
            label: "historical tool result".to_string(),
            content: "prior command output".to_string(),
        };
        let committed_evidence = ContextBlock {
            source: ContextSourceKind::CommittedEvidence,
            label: "committed evidence".to_string(),
            content: "compact prior action evidence".to_string(),
        };
        let pane_identity = ContextBlock {
            source: ContextSourceKind::Configuration,
            label: "pane identity".to_string(),
            content: "pane_id=%1 window_name=0".to_string(),
        };

        assert_eq!(project.stability(), ContextStability::RepoScoped);
        assert_eq!(
            project.cache_policy(),
            ContextCachePolicy::ProviderBreakpoint
        );
        assert!(project.stable_prefix_eligible());
        assert_eq!(scheduler.stability(), ContextStability::TurnVolatile);
        assert_eq!(scheduler.cache_policy(), ContextCachePolicy::Ineligible);
        assert!(!scheduler.stable_prefix_eligible());
        assert_eq!(transcript_tool.stability(), ContextStability::SessionStable);
        assert_eq!(transcript_tool.cache_policy(), ContextCachePolicy::Eligible);
        assert!(transcript_tool.stable_prefix_eligible());
        assert_eq!(
            committed_evidence.stability(),
            ContextStability::SessionStable
        );
        assert_eq!(
            committed_evidence.cache_policy(),
            ContextCachePolicy::Eligible
        );
        assert!(committed_evidence.stable_prefix_eligible());
        assert!(committed_evidence.recoverable_for_compaction());
        assert_eq!(pane_identity.stability(), ContextStability::TurnVolatile);
        assert_eq!(pane_identity.cache_policy(), ContextCachePolicy::Ineligible);
        assert!(!pane_identity.stable_prefix_eligible());
        assert!(action.recoverable_for_compaction());
    }

    /// Required context validation accepts substantive values and rejects
    /// whitespace-only values with a stable field-specific diagnostic.
    #[test]
    fn context_required_validation_rejects_whitespace() {
        assert!(validate_context_required("model", "gpt-5").is_ok());
        let error = validate_context_required("model", " \t ").unwrap_err();
        assert_eq!(error.to_string(), "model must not be empty");
    }

    /// Request assembly preserves invalid-argument classification when either
    /// context validation or prompt-profile validation rejects an input.
    #[test]
    fn request_assembly_preserves_invalid_argument_errors() {
        let context_error = AgentRequestAssemblyError::from(AgentContextError::new("bad model"));
        let prompt_error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_args("bad profile"));

        assert_eq!(
            context_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
        assert_eq!(
            prompt_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
    }

    /// Request assembly retains invalid-state classification for failures in
    /// product-supplied prompt assets so the composition layer can adapt it.
    #[test]
    fn request_assembly_preserves_prompt_asset_errors() {
        let error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_state("asset missing"));

        assert_eq!(error.kind(), AgentRequestAssemblyErrorKind::InvalidState);
        assert_eq!(error.message(), "asset missing");
    }
}
