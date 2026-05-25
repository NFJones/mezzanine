//! Agent Context implementation.
//!
//! This module owns the agent context boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{MezError, Result, validate_non_empty};

mod appenders;
mod assembly;
mod compaction;
mod evidence;
mod model;
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
pub use model::{
    ModelMessage, ModelMessageRole, ModelProfile, ModelProfileOverrideSource,
    ModelProfileOverrides, ModelRequest, SelectedModelProfile, select_model_profile,
};
pub use skills::constrain_skill_actions_for_loaded_context;
pub use surface::{AgentCapability, AllowedAction, AllowedActionSet, ModelInteractionKind};

/// Maximum bytes from one context block copied into a provider request.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Marker used for deterministic local compaction summaries in provider context.
const MODEL_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Default raw suffix percent retained after local context compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;
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
    /// Represents compact immutable evidence promoted from settled turn actions.
    ///
    /// These generated blocks are safe to place in provider cache-prefix
    /// material after a later assistant response has already observed the raw
    /// action result. They preserve continuity without replaying large volatile
    /// result payloads forever.
    CommittedEvidence,
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
            | ContextSourceKind::CommittedEvidence
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
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence => ContextStability::SessionStable,
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
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::CommittedEvidence => {
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
                | ContextSourceKind::CommittedEvidence
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
