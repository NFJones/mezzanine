//! Agent Context implementation.
//!
//! This module owns the agent context boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{McpPromptTool, MezError, Result, validate_non_empty};

mod appenders;
mod assembly;
mod compaction;
mod evidence;
mod skills;
mod surface;

pub use appenders::{
    append_mcp_context, append_memory_context, append_permission_policy_context,
    append_project_guidance_context, append_scheduler_context, set_project_guidance_context,
};
pub use assembly::{assemble_model_request, assemble_model_request_with_retained_tail_percent};
pub use compaction::{
    compact_model_context_for_budget, compact_model_context_for_budget_with_retained_tail_percent,
    model_context_text_word_count,
};
pub use skills::constrain_skill_actions_for_loaded_context;
pub use surface::{AgentCapability, AllowedAction, AllowedActionSet, ModelInteractionKind};

/// Maximum bytes from one context block copied into a provider request.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Maximum exact bytes pinned as hot action evidence during compaction.
const MODEL_CONTEXT_HOT_ACTION_LIMIT_BYTES: usize = 16 * 1024;
/// Marker used for deterministic local compaction summaries in provider context.
const MODEL_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Default raw suffix percent retained after local context compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;
/// Fallback context window when the model profile does not carry one.
const MODEL_CONTEXT_FALLBACK_WINDOW_TOKENS: usize = 128 * 1024;
/// Output-token cap used for the first output-limit retry when no profile cap
/// was configured.
const MODEL_OUTPUT_LIMIT_RETRY_TOKENS: usize = 16_384;
/// Upper bound for automatic output-limit retry cap escalation.
const MODEL_OUTPUT_LIMIT_RETRY_CEILING_TOKENS: usize = 32_768;
/// Conservative numerator for converting token context windows into word budgets.
const MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_NUMERATOR: usize = 3;
/// Conservative denominator for converting token context windows into word budgets.
const MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_DENOMINATOR: usize = 4;
/// Documented context window for OpenAI frontier 1M-token model families.
const OPENAI_FRONTIER_CONTEXT_WINDOW_TOKENS: usize = 1_050_000;
/// Documented context window for OpenAI GPT-5 family 400K-token model families.
const OPENAI_STANDARD_GPT5_CONTEXT_WINDOW_TOKENS: usize = 400_000;

// Context blocks, model profiles, and context assembly.

/// Carries Context Source Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSourceKind {
    /// Represents the System case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    System,
    /// Represents the User Instruction case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UserInstruction,
    /// Represents the Developer Instruction case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeveloperInstruction,
    /// Represents the Policy case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Policy,
    /// Represents the Configuration case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Configuration,
    /// Represents the Local Message case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LocalMessage,
    /// Represents the Project Guidance case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ProjectGuidance,
    /// Represents the Memory case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Memory,
    /// Represents the Transcript case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Transcript,
    /// Represents a prior user-authored transcript message.
    ///
    /// The role-specific transcript variants let model requests replay chat
    /// history without flattening all previous turns into one synthetic user
    /// block.
    TranscriptUser,
    /// Represents a prior assistant-authored transcript message.
    ///
    /// Assistant transcript context should preserve text the user previously
    /// saw so follow-up prompts can refer to plans, lists, and decisions.
    TranscriptAssistant,
    /// Represents a prior tool/action transcript message.
    ///
    /// Historical tool entries remain bounded and sanitized before becoming
    /// model context.
    TranscriptTool,
    /// Represents a compact ledger of evidence already gathered in a turn.
    ///
    /// This generated block lets provider continuations reuse command, test,
    /// patch, and file-read facts without replaying large raw tool outputs.
    EvidenceLedger,
    /// Represents the Action Result case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ActionResult,
}

/// Trust domains for context assembly as required by the specification.
/// Terminal output, project files, and web content are untrusted by default
/// unless the user explicitly marks a source as trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDomain {
    /// User-provided instructions or agent-to-agent messages.
    UserInput,
    /// Project instruction files discovered through the pane shell.
    ProjectFile,
    /// Configuration, policy, and system instructions.
    Configuration,
    /// External web or API content retrieved by the agent.
    WebContent,
    /// Previous model responses and action results.
    ModelOutput,
}

/// Describes how stable one context block is for provider prompt-cache reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextStability {
    /// Static instructions or configuration that change only with Mezzanine or
    /// profile configuration changes.
    Static,
    /// Repository-scoped guidance that changes when project files change.
    RepoScoped,
    /// Session-scoped summaries or memories that may persist across turns.
    SessionStable,
    /// Turn-local state such as the newest prompt, action results, or scheduler
    /// diagnostics.
    TurnVolatile,
}

/// Describes whether a block may participate in provider cache-prefix material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCachePolicy {
    /// The block may appear in the provider reusable-prefix group.
    Eligible,
    /// The block must stay out of provider cache-prefix calculations.
    Ineligible,
    /// The block is eligible and may be a provider-specific cache breakpoint.
    ProviderBreakpoint,
}

impl TrustDomain {
    /// Runs the for source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn for_source(source: ContextSourceKind) -> Self {
        match source {
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration => TrustDomain::Configuration,
            ContextSourceKind::UserInstruction | ContextSourceKind::LocalMessage => {
                TrustDomain::UserInput
            }
            ContextSourceKind::ProjectGuidance => TrustDomain::ProjectFile,
            ContextSourceKind::Memory => TrustDomain::UserInput,
            ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::ActionResult => TrustDomain::ModelOutput,
            ContextSourceKind::TranscriptUser => TrustDomain::UserInput,
        }
    }

    /// Runs the is untrusted by default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_untrusted_by_default(&self) -> bool {
        matches!(self, TrustDomain::ProjectFile | TrustDomain::WebContent)
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustDomain::UserInput => "user-input",
            TrustDomain::ProjectFile => "project-file",
            TrustDomain::Configuration => "configuration",
            TrustDomain::WebContent => "web-content",
            TrustDomain::ModelOutput => "model-output",
        }
    }
}

/// Carries Context Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    /// Stores the source value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: ContextSourceKind,
    /// Stores the label value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub label: String,
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content: String,
}

/// Counts context compaction performed while preparing provider-bound context.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModelContextCompactionReport {
    /// Number of individual blocks replaced with compact local summaries.
    pub compacted_blocks: usize,
    /// Number of already compacted blocks omitted after summaries still exceeded
    /// the local model context budget.
    pub omitted_blocks: usize,
    /// Original estimated words represented by omitted compacted blocks.
    pub omitted_original_words: usize,
}

impl ModelContextCompactionReport {
    /// Returns true when provider context was changed by local compaction.
    pub fn changed(&self) -> bool {
        self.compacted_blocks > 0 || self.omitted_blocks > 0
    }
}

impl ContextBlock {
    /// Runs the trust domain operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
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
            | ContextSourceKind::TranscriptTool => ContextStability::SessionStable,
            ContextSourceKind::EvidenceLedger => ContextStability::TurnVolatile,
            ContextSourceKind::UserInstruction
            | ContextSourceKind::LocalMessage
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
            | ContextSourceKind::TranscriptAssistant => {
                if self.stability() == ContextStability::TurnVolatile {
                    ContextCachePolicy::Ineligible
                } else if self.source == ContextSourceKind::ProjectGuidance {
                    ContextCachePolicy::ProviderBreakpoint
                } else {
                    ContextCachePolicy::Eligible
                }
            }
            ContextSourceKind::TranscriptTool => ContextCachePolicy::Ineligible,
            ContextSourceKind::UserInstruction
            | ContextSourceKind::LocalMessage
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::ActionResult => ContextCachePolicy::Ineligible,
        }
    }

    /// Returns true when the block may be rendered into the reusable prefix.
    pub fn stable_prefix_eligible(&self) -> bool {
        self.cache_policy() != ContextCachePolicy::Ineligible
            && self.stability() != ContextStability::TurnVolatile
    }

    /// Returns true when exact block content is recoverable outside the model
    /// prompt, so local compaction may summarize it before active instructions.
    pub fn recoverable_for_compaction(&self) -> bool {
        matches!(
            self.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
                | ContextSourceKind::EvidenceLedger
                | ContextSourceKind::ActionResult
                | ContextSourceKind::LocalMessage
        )
    }
}

/// Returns true when a configuration block is useful context but should not be
/// part of the provider reusable prefix.
///
/// These values are pane/session/environment identities. They can change
/// without changing task semantics and would otherwise fragment prompt-cache
/// prefixes across panes or shell refreshes.
fn configuration_context_is_turn_volatile(label: &str) -> bool {
    label == "session identity"
        || label == "pane identity"
        || label == "provider output-limit retry guidance"
        || label.starts_with("environment signature for pane ")
}

/// Carries Agent Context state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    /// Stores the blocks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub blocks: Vec<ContextBlock>,
}

/// Carries Model Message Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMessageRole {
    /// Represents the System case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    System,
    /// Represents the Developer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Developer,
    /// Represents the User case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    User,
    /// Represents the Assistant case for this enumeration.
    ///
    /// Prior assistant messages must keep their role when replayed so the
    /// model can distinguish user instructions from earlier model output.
    Assistant,
    /// Represents the Tool case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Tool,
}

/// Carries Model Message state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMessage {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ModelMessageRole,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: ContextSourceKind,
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content: String,
}

/// Carries Model Profile state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelProfile {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model: String,
    /// Stores the reasoning profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_profile: Option<String>,
    /// Stores the latency preference value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub latency_preference: Option<String>,
    /// Stores the multimodal required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub multimodal_required: bool,
    /// Stores the provider options value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider_options: std::collections::BTreeMap<String, String>,
    /// Safety tier for model failover comparison. When a model is unavailable,
    /// Mezzanine MUST NOT silently switch to a model with a lower safety tier.
    /// Tiers: `"high"`, `"medium"`, `"basic"` (or absent when unset).
    pub safety_tier: Option<String>,
}

impl ModelProfile {
    /// Returns the approximate provider context window in model tokens.
    ///
    /// Profile-specific values may be supplied through `provider_options` as
    /// `context_window_tokens` or `context_limit_tokens`. When omitted,
    /// Mezzanine first uses built-in provider model metadata for known default
    /// models, then falls back to a conservative built-in default so automatic
    /// compaction has a stable budget before provider metadata is available.
    pub fn context_window_tokens(&self) -> usize {
        self.configured_context_window_tokens()
            .or_else(|| known_provider_model_context_window_tokens(&self.provider, &self.model))
            .unwrap_or(MODEL_CONTEXT_FALLBACK_WINDOW_TOKENS)
    }

    /// Returns the configured provider output-token cap, if present.
    ///
    /// OpenAI-compatible providers use `max_output_tokens`; compatible
    /// adapters may expose the same concept as `max_completion_tokens`, so both
    /// non-secret profile options are accepted.
    pub fn max_output_tokens(&self) -> Option<usize> {
        self.provider_options
            .get("max_output_tokens")
            .or_else(|| self.provider_options.get("max_completion_tokens"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|tokens| *tokens > 0)
    }

    /// Returns the output-token cap to use after a provider output-limit
    /// failure.
    pub fn output_limit_retry_tokens(&self) -> usize {
        let configured = self.max_output_tokens().unwrap_or(0);
        configured.saturating_mul(2).clamp(
            MODEL_OUTPUT_LIMIT_RETRY_TOKENS,
            MODEL_OUTPUT_LIMIT_RETRY_CEILING_TOKENS,
        )
    }

    /// Returns the profile-configured context window, if the profile carries one.
    fn configured_context_window_tokens(&self) -> Option<usize> {
        self.provider_options
            .get("context_window_tokens")
            .or_else(|| self.provider_options.get("context_limit_tokens"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|tokens| *tokens > 0)
    }

    /// Returns the word budget used when explicit context compaction needs a
    /// model-window-sized target.
    pub fn context_window_budget_words(&self) -> usize {
        self.context_window_tokens()
            .saturating_mul(MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_NUMERATOR)
            .saturating_div(MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_DENOMINATOR)
            .max(1)
    }

    /// Ordinal for comparison: higher number = stronger safety.
    fn safety_ordinal(tier: Option<&str>) -> u8 {
        match tier {
            Some("high") => 3,
            Some("medium") => 2,
            Some("basic") => 1,
            _ => 0,
        }
    }

    /// Returns true if `fallback` has equivalent or stronger configured
    /// characteristics than `self`, permitting it to be offered as a safe
    /// failover candidate. Privacy, residency, and approval characteristics are
    /// modeled as exact non-secret provider options because their ordering is
    /// provider- and deployment-specific.
    pub fn failover_safe(&self, fallback: &Self) -> bool {
        if Self::safety_ordinal(fallback.safety_tier.as_deref())
            < Self::safety_ordinal(self.safety_tier.as_deref())
        {
            return false;
        }
        for key in [
            "privacy",
            "privacy_tier",
            "residency",
            "residency_region",
            "approval",
            "approval_policy",
        ] {
            if let Some(required) = self.provider_options.get(key)
                && fallback.provider_options.get(key) != Some(required)
            {
                return false;
            }
        }
        true
    }
}

/// Returns known provider model context-window metadata for built-in providers.
fn known_provider_model_context_window_tokens(provider: &str, model: &str) -> Option<usize> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => openai_known_model_context_window_tokens(model),
        _ => None,
    }
}

/// Returns documented context windows for OpenAI model families Mezzanine ships.
fn openai_known_model_context_window_tokens(model: &str) -> Option<usize> {
    let model = model.trim().to_ascii_lowercase();
    if openai_model_matches_snapshot_family(&model, "gpt-5.5")
        || openai_model_matches_snapshot_family(&model, "gpt-5.5-pro")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4-pro")
    {
        return Some(OPENAI_FRONTIER_CONTEXT_WINDOW_TOKENS);
    }
    if openai_model_matches_snapshot_family(&model, "gpt-5.4-mini")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4-nano")
        || openai_model_matches_snapshot_family(&model, "gpt-5.3-codex")
        || openai_model_matches_snapshot_family(&model, "gpt-5.2")
        || openai_model_matches_snapshot_family(&model, "gpt-5-codex")
        || openai_model_matches_snapshot_family(&model, "gpt-5-mini")
        || openai_model_matches_snapshot_family(&model, "gpt-5-nano")
        || openai_model_matches_snapshot_family(&model, "gpt-5")
    {
        return Some(OPENAI_STANDARD_GPT5_CONTEXT_WINDOW_TOKENS);
    }
    None
}

/// Matches an exact model family or a dated model snapshot for that family.
fn openai_model_matches_snapshot_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|first| first.is_ascii_digit())
}

/// Carries Model Profile Overrides state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelProfileOverrides {
    /// Stores the default profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_profile: Option<String>,
    /// Stores the session profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_profile: Option<String>,
    /// Stores the window profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_profile: Option<String>,
    /// Stores the pane profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_profile: Option<String>,
    /// Stores the agent profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_profile: Option<String>,
    /// Stores the subagent profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub subagent_profile: Option<String>,
}

/// Carries Selected Model Profile state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModelProfile {
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub profile: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: ModelProfileOverrideSource,
}

/// Carries Model Profile Override Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProfileOverrideSource {
    /// Represents the Default case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Default,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window,
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Subagent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Subagent,
}

/// Carries Model Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model: String,
    /// Stores the provider reasoning effort for this request, when configured.
    ///
    /// The field is runtime-owned per request so temporary turn sizing can
    /// adjust reasoning without mutating saved model profiles.
    pub reasoning_effort: Option<String>,
    /// Latency/cost preference for provider request routing, when configured.
    ///
    /// The value is runtime-owned per request so pane-local profile overrides
    /// can select provider service tiers without mutating saved profiles.
    pub latency_preference: Option<String>,
    /// Provider prompt-cache retention policy, when configured.
    ///
    /// OpenAI-compatible providers use this to request longer-lived prefix
    /// cache retention without baking retention policy into the prompt cache
    /// key itself.
    pub prompt_cache_retention: Option<String>,
    /// Provider output-token cap, when configured or temporarily escalated for
    /// an output-limit retry.
    pub max_output_tokens: Option<usize>,
    /// Live Mezzanine session identifier used to route provider prompt-cache
    /// entries without coupling the local key to provider or model names.
    ///
    /// The value is non-secret and is derived from runtime session context when
    /// present. Requests built outside a live session leave it unset.
    pub prompt_cache_session_id: Option<String>,
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the available mcp tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_tools: Vec<McpPromptTool>,
    /// Stores the interaction kind for this provider request.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interaction_kind: ModelInteractionKind,
    /// Stores the concrete MAAP action surface exposed for this request.
    ///
    /// The provider adapter uses this set to generate a strict per-request
    /// schema rather than exposing every MAAP action on every turn.
    pub allowed_actions: AllowedActionSet,
    /// Stores the messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub messages: Vec<ModelMessage>,
}

impl AgentContext {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(blocks: Vec<ContextBlock>) -> Result<Self> {
        if blocks.is_empty() {
            return Err(MezError::invalid_args(
                "agent context must contain at least one context block",
            ));
        }
        for block in &blocks {
            validate_non_empty("context label", &block.label)?;
        }
        Ok(Self { blocks })
    }
}

/// Runs the select model profile operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn select_model_profile(
    overrides: &ModelProfileOverrides,
    configured_default: &str,
) -> Result<SelectedModelProfile> {
    validate_non_empty("configured default model profile", configured_default)?;
    let candidates = [
        (
            overrides.subagent_profile.as_deref(),
            ModelProfileOverrideSource::Subagent,
        ),
        (
            overrides.agent_profile.as_deref(),
            ModelProfileOverrideSource::Agent,
        ),
        (
            overrides.pane_profile.as_deref(),
            ModelProfileOverrideSource::Pane,
        ),
        (
            overrides.window_profile.as_deref(),
            ModelProfileOverrideSource::Window,
        ),
        (
            overrides.session_profile.as_deref(),
            ModelProfileOverrideSource::Session,
        ),
        (
            overrides.default_profile.as_deref(),
            ModelProfileOverrideSource::Default,
        ),
    ];
    for (profile, source) in candidates {
        if let Some(profile) = profile {
            validate_non_empty("model profile override", profile)?;
            return Ok(SelectedModelProfile {
                profile: profile.to_string(),
                source,
            });
        }
    }
    Ok(SelectedModelProfile {
        profile: configured_default.to_string(),
        source: ModelProfileOverrideSource::Default,
    })
}

/// Builds the bracketed provider-message header for one context block.
pub(super) fn model_context_block_header(block: &ContextBlock) -> String {
    let trust = block.trust_domain();
    let domain_annotation = if trust.is_untrusted_by_default() {
        format!(" [untrusted:{}]", trust.as_str())
    } else {
        String::new()
    };
    format!("[{}{}]\n", block.label, domain_annotation)
}
