//! Provider-independent agent context validation contracts.
//!
//! This module owns deterministic validation failures for context blocks and
//! model-profile selection. Product prompt assets, persistence, and provider
//! execution remain outside this crate and adapt these errors at their
//! composition boundaries.

use std::collections::BTreeSet;
use std::fmt;

use crate::action_result::ActionResult;
use crate::action_result_context::action_result_context_content;
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
    /// Routed-worker result and handoff context supplied for parent presentation.
    RoutedHandoff,
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
            | ContextSourceKind::RoutedHandoff
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

/// Explicit provider-neutral placement for model-visible context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPlacement {
    /// Invariant instructions and configuration that form the reusable prefix.
    StablePrefix,
    /// Immutable chronological conversation material appended after the prefix.
    ConversationAppend,
    /// Regenerated controller and current-turn state kept outside the prefix.
    EphemeralTail,
}

/// Model-facing meaning of one context block, independent of provider role.
///
/// Providers may need to wrap neutral context in a supported transport role,
/// but they must preserve this canonical meaning and cannot turn controller or
/// repository context into direct user authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSemanticKind {
    /// Invariant system, developer, policy, or project instructions.
    AmbientInstruction,
    /// Task-scoped instructions and references known before the active prompt.
    TaskPrelude,
    /// An event authored directly by a user.
    UserEvent,
    /// An event authored by the assistant.
    AssistantEvent,
    /// Settled tool, action, controller, or routed-workflow evidence.
    EvidenceEvent,
    /// Neutral historical, memory, or agent-to-agent reference material.
    ReferenceEvent,
    /// Mutable factual state needed only for the next provider request.
    LiveState,
}

/// Retention and compaction treatment for one context block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextRetention {
    /// Preserve the block byte-for-byte across request preparation and active
    /// turn compaction.
    Exact,
    /// Compact the block only with the closed execution group it belongs to.
    ExecutionGroup,
    /// The block may participate in chronological historical summarization.
    Summarizable,
    /// The block exists only in one prepared request and is never persisted.
    RequestLocal,
}

/// One ordered unit of model-visible context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    /// Provenance and role class for the block.
    pub source: ContextSourceKind,
    /// Explicit cache and ordering lifecycle chosen by the block producer.
    pub placement: ContextPlacement,
    /// Human-readable block label used in provider message framing.
    pub label: String,
    /// Exact model-visible block contents.
    pub content: String,
}

impl ContextBlock {
    /// Builds one invariant instruction in the stable reusable prefix.
    pub fn stable_instruction(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            source,
            placement: ContextPlacement::StablePrefix,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one exact task prelude appended before the active user prompt.
    pub fn task_prelude(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            source,
            placement: ContextPlacement::ConversationAppend,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one exact direct-user event in immutable chronology.
    pub fn user_event(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            source: ContextSourceKind::UserInstruction,
            placement: ContextPlacement::ConversationAppend,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one assistant-authored chronological event.
    pub fn assistant_event(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            source: ContextSourceKind::TranscriptAssistant,
            placement: ContextPlacement::ConversationAppend,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one settled evidence event in immutable chronology.
    pub fn evidence_event(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            source,
            placement: ContextPlacement::ConversationAppend,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one neutral reference event in immutable chronology.
    pub fn reference_event(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            source,
            placement: ContextPlacement::ConversationAppend,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Builds one factual request-local live-state block.
    pub fn live_state(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            source,
            placement: ContextPlacement::EphemeralTail,
            label: label.into(),
            content: content.into(),
        }
    }

    /// Returns the block's derived trust domain.
    pub fn trust_domain(&self) -> TrustDomain {
        TrustDomain::for_source(self.source)
    }

    /// Returns the compatibility stability class for this block's placement.
    pub fn stability(&self) -> ContextStability {
        match self.placement {
            ContextPlacement::StablePrefix => ContextStability::Static,
            ContextPlacement::ConversationAppend => ContextStability::SessionStable,
            ContextPlacement::EphemeralTail => ContextStability::TurnVolatile,
        }
    }

    /// Returns the provider-cache policy for this block.
    pub fn cache_policy(&self) -> ContextCachePolicy {
        match self.placement {
            ContextPlacement::StablePrefix | ContextPlacement::ConversationAppend => {
                ContextCachePolicy::Eligible
            }
            ContextPlacement::EphemeralTail => ContextCachePolicy::Ineligible,
        }
    }

    /// Returns whether the block may participate in a reusable prefix.
    pub fn stable_prefix_eligible(&self) -> bool {
        self.cache_policy() != ContextCachePolicy::Ineligible
            && self.stability() != ContextStability::TurnVolatile
    }

    /// Returns the explicit cache lifecycle placement used for request ordering.
    pub fn cache_disposition(&self) -> ContextPlacement {
        self.placement
    }

    /// Returns the canonical semantic meaning of this block.
    pub fn semantic_kind(&self) -> ContextSemanticKind {
        match self.source {
            ContextSourceKind::UserInstruction | ContextSourceKind::TranscriptUser => {
                ContextSemanticKind::UserEvent
            }
            ContextSourceKind::TranscriptAssistant => ContextSemanticKind::AssistantEvent,
            ContextSourceKind::TranscriptTool
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::RoutedHandoff
            | ContextSourceKind::ActionResult => ContextSemanticKind::EvidenceEvent,
            ContextSourceKind::SkillInstruction => ContextSemanticKind::TaskPrelude,
            ContextSourceKind::LocalMessage
            | ContextSourceKind::Memory
            | ContextSourceKind::Transcript => ContextSemanticKind::ReferenceEvent,
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::ProjectGuidance => {
                if self.placement == ContextPlacement::StablePrefix {
                    ContextSemanticKind::AmbientInstruction
                } else {
                    ContextSemanticKind::TaskPrelude
                }
            }
            ContextSourceKind::Policy
            | ContextSourceKind::Configuration
            | ContextSourceKind::RuntimeHint => match self.placement {
                ContextPlacement::StablePrefix => ContextSemanticKind::AmbientInstruction,
                ContextPlacement::ConversationAppend => ContextSemanticKind::TaskPrelude,
                ContextPlacement::EphemeralTail => ContextSemanticKind::LiveState,
            },
        }
    }

    /// Returns the canonical retention treatment of this block.
    pub fn retention(&self) -> ContextRetention {
        if self.placement == ContextPlacement::EphemeralTail {
            return ContextRetention::RequestLocal;
        }
        match self.source {
            ContextSourceKind::UserInstruction
            | ContextSourceKind::SkillInstruction
            | ContextSourceKind::LocalMessage
            | ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration
            | ContextSourceKind::ProjectGuidance
            | ContextSourceKind::RuntimeHint => ContextRetention::Exact,
            ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::EvidenceLedger
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::RoutedHandoff
            | ContextSourceKind::ActionResult => ContextRetention::ExecutionGroup,
            ContextSourceKind::Memory
            | ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser => ContextRetention::Summarizable,
        }
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
                | ContextSourceKind::RoutedHandoff
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

    /// Creates durable context containing only stable and append-only blocks.
    ///
    /// Runtime turn storage must use this constructor so request-local state
    /// cannot accidentally survive into later provider calls.
    pub fn new_durable(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        let context = Self::new(blocks)?;
        context.validate_durable()?;
        Ok(context)
    }

    /// Validates the semantic and lifetime contract for stored turn context.
    pub fn validate_durable(&self) -> AgentContextResult<()> {
        validate_context_placement_order(&self.blocks)?;
        validate_context_semantics(&self.blocks)?;
        if let Some((index, block)) = self
            .blocks
            .iter()
            .enumerate()
            .find(|(_, block)| block.placement == ContextPlacement::EphemeralTail)
        {
            return Err(context_semantic_error(
                index,
                block,
                "durable agent context cannot contain ephemeral-tail blocks",
            ));
        }
        Ok(())
    }

    /// Validates that blocks advance monotonically through cache lifecycle phases.
    ///
    /// This check remains separate from [`AgentContext::new`] because low-level
    /// tests and a small number of builders need to represent an intermediate
    /// context before it reaches a finalized runtime boundary. Production
    /// prompt submission and provider assembly must validate before side effects.
    pub fn validate_placement_order(&self) -> AgentContextResult<()> {
        validate_context_placement_order(&self.blocks)
    }

    /// Atomically promotes deterministic action results into chronology.
    ///
    /// The operation rejects unresolved running or blocked results before it
    /// mutates context, removes any volatile or legacy copy for each action,
    /// preserves an already committed exact copy in place, and appends each
    /// newly settled result at the immutable chronology boundary. Repeating
    /// the same commit is therefore idempotent and cannot reorder evidence.
    pub fn commit_settled_action_results(
        &mut self,
        results: &[ActionResult],
    ) -> AgentContextResult<usize> {
        if results.iter().any(|result| !result.is_terminal()) {
            return Err(AgentContextError::new(
                "only terminal action results may be committed to immutable chronology",
            ));
        }
        let mut action_ids = BTreeSet::new();
        if results
            .iter()
            .any(|result| !action_ids.insert(result.action_id.as_str()))
        {
            return Err(AgentContextError::new(
                "an action result commit may contain each action id only once",
            ));
        }

        let mut candidate = self.blocks.clone();
        let mut committed = 0usize;
        for result in results {
            let label = format!("action result {}", result.action_id);
            let content = action_result_context_content(result);
            let exact_index = candidate.iter().position(|block| {
                block.source == ContextSourceKind::ActionResult
                    && block.placement == ContextPlacement::ConversationAppend
                    && block.label == label
                    && block.content == content
            });
            let mut index = 0usize;
            candidate.retain(|block| {
                let same_action = block.source == ContextSourceKind::ActionResult
                    && action_result_block_id(block).is_some_and(|id| id == result.action_id);
                let keep = !same_action || exact_index == Some(index);
                index = index.saturating_add(1);
                keep
            });
            if exact_index.is_some() {
                continue;
            }
            insert_context_block_by_placement(
                &mut candidate,
                ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    placement: ContextPlacement::ConversationAppend,
                    label,
                    content,
                },
            );
            committed = committed.saturating_add(1);
        }
        validate_context_placement_order(&candidate)?;
        self.blocks = candidate;
        Ok(committed)
    }
}

/// One provider-bound view composed from durable chronology and request-local
/// live state.
///
/// The live state is validated separately and is discarded after the request;
/// it never mutates or becomes part of the stored [`AgentContext`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModelContext {
    durable: AgentContext,
    live_state: Vec<ContextBlock>,
}

impl PreparedModelContext {
    /// Builds and validates one prepared provider context.
    pub fn new(durable: AgentContext, live_state: Vec<ContextBlock>) -> AgentContextResult<Self> {
        durable.validate_durable()?;
        let prepared = Self {
            durable,
            live_state,
        };
        let ordered = prepared.ordered_blocks();
        validate_context_placement_order(&ordered)?;
        validate_context_semantics(&ordered)?;
        Ok(prepared)
    }

    /// Builds a prepared context with no request-local live state.
    pub fn from_durable(durable: AgentContext) -> AgentContextResult<Self> {
        Self::new(durable, Vec::new())
    }

    /// Returns the immutable stored portion of the request context.
    pub fn durable(&self) -> &AgentContext {
        &self.durable
    }

    /// Returns the request-local live-state suffix.
    pub fn live_state(&self) -> &[ContextBlock] {
        &self.live_state
    }

    /// Returns the number of blocks visible to the provider.
    pub fn len(&self) -> usize {
        self.durable.blocks.len() + self.live_state.len()
    }

    /// Reports whether the prepared request has no model-visible blocks.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clones the canonical provider-visible sequence without changing order.
    pub fn to_agent_context(&self) -> AgentContext {
        AgentContext {
            blocks: self.ordered_blocks(),
        }
    }

    /// Consumes the prepared view and joins its two already validated phases.
    pub fn into_agent_context(mut self) -> AgentContext {
        self.durable.blocks.append(&mut self.live_state);
        self.durable
    }

    /// Clones stable/append chronology followed by the live-state suffix.
    fn ordered_blocks(&self) -> Vec<ContextBlock> {
        let mut blocks = Vec::with_capacity(self.len());
        blocks.extend(self.durable.blocks.iter().cloned());
        blocks.extend(self.live_state.iter().cloned());
        blocks
    }
}

/// Extracts the action id from canonical and legacy result-block labels.
fn action_result_block_id(block: &ContextBlock) -> Option<&str> {
    block
        .label
        .strip_prefix("action result ")
        .or_else(|| block.label.strip_prefix("action failure "))
}

/// Returns the stable insertion boundary for one lifecycle placement.
///
/// New stable blocks are placed after the existing stable prefix, new
/// conversation blocks after existing immutable chronology, and new ephemeral
/// blocks at the end. This preserves producer order within each phase without
/// globally sorting context and changing transcript semantics.
pub fn context_placement_insertion_index(
    blocks: &[ContextBlock],
    placement: ContextPlacement,
) -> usize {
    blocks
        .iter()
        .position(|block| block.placement > placement)
        .unwrap_or(blocks.len())
}

/// Inserts one context block at its lifecycle phase boundary.
pub fn insert_context_block_by_placement(blocks: &mut Vec<ContextBlock>, block: ContextBlock) {
    let insertion_index = context_placement_insertion_index(blocks, block.placement);
    blocks.insert(insertion_index, block);
}

/// Rejects cache-lifecycle regressions without changing producer order.
pub fn validate_context_placement_order(blocks: &[ContextBlock]) -> AgentContextResult<()> {
    let mut entered_phase = ContextPlacement::StablePrefix;
    for (index, block) in blocks.iter().enumerate() {
        if block.placement < entered_phase {
            return Err(AgentContextError::new(format!(
                "context lifecycle regression at block index {index}: label={:?} source={:?} placement={:?} entered_phase={entered_phase:?}",
                block.label, block.source, block.placement
            )));
        }
        entered_phase = block.placement;
    }
    Ok(())
}

/// Rejects semantic, retention, and authorship combinations that would make a
/// provider request ambiguous or move durable events into request-local state.
pub fn validate_context_semantics(blocks: &[ContextBlock]) -> AgentContextResult<()> {
    let mut active_user_seen = false;
    for (index, block) in blocks.iter().enumerate() {
        validate_context_required("context label", &block.label)?;
        let semantic = block.semantic_kind();
        let retention = block.retention();
        let invalid_reason = match block.placement {
            ContextPlacement::StablePrefix
                if semantic != ContextSemanticKind::AmbientInstruction =>
            {
                Some("stable-prefix blocks must be ambient instructions")
            }
            ContextPlacement::ConversationAppend
                if semantic == ContextSemanticKind::LiveState
                    || retention == ContextRetention::RequestLocal =>
            {
                Some("append-only blocks cannot contain request-local live state")
            }
            ContextPlacement::EphemeralTail
                if semantic != ContextSemanticKind::LiveState
                    || retention != ContextRetention::RequestLocal =>
            {
                Some("ephemeral-tail blocks must be request-local live state")
            }
            _ => None,
        };
        if let Some(reason) = invalid_reason {
            return Err(context_semantic_error(index, block, reason));
        }
        if block.source == ContextSourceKind::UserInstruction {
            if block.placement != ContextPlacement::ConversationAppend
                || retention != ContextRetention::Exact
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "direct user instructions must be exact append-only user events",
                ));
            }
            active_user_seen = true;
        } else if active_user_seen && semantic == ContextSemanticKind::TaskPrelude {
            return Err(context_semantic_error(
                index,
                block,
                "task prelude cannot appear after the active user prompt",
            ));
        }
    }
    Ok(())
}

/// Builds one detailed semantic-validation failure.
fn context_semantic_error(index: usize, block: &ContextBlock, reason: &str) -> AgentContextError {
    AgentContextError::new(format!(
        "context semantic violation at block index {index}: label={:?} source={:?} placement={:?} semantic={:?} retention={:?}: {reason}",
        block.label,
        block.source,
        block.placement,
        block.semantic_kind(),
        block.retention()
    ))
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
    /// Neutral controller, reference, or live context that is not user speech.
    Context,
}

/// Provider-independent message supplied to model request rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMessage {
    /// Provider-facing role of the message.
    pub role: ModelMessageRole,
    /// Provenance and stability class of the message.
    pub source: ContextSourceKind,
    /// Explicit cache and ordering lifecycle carried from the context producer.
    pub placement: ContextPlacement,
    /// Model-visible message content.
    pub content: String,
}

impl ModelMessage {
    /// Returns the provider-neutral cache lifecycle disposition for this message.
    pub fn cache_disposition(&self) -> ContextPlacement {
        self.placement
    }
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
        AgentContext, AgentContextError, AgentRequestAssemblyError, AgentRequestAssemblyErrorKind,
        ContextBlock, ContextCachePolicy, ContextRetention, ContextSemanticKind, ContextSourceKind,
        ContextStability, PreparedModelContext, validate_context_required,
        validate_context_semantics,
    };
    use crate::{ActionContentBlock, ActionResult, ActionStatus, AgentPromptError};

    /// Builds one valid successful or running action-result fixture.
    fn action_result(action_id: &str, status: ActionStatus, text: &str) -> ActionResult {
        ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            action_id: action_id.to_string(),
            action_type: "shell_command",
            status,
            content: vec![ActionContentBlock::text(text)],
            structured_content_json: None,
            is_error: false,
            error: None,
        }
    }

    /// Verifies context blocks expose cache-stability metadata without changing
    /// the stored source, label, and content shape.
    #[test]
    fn context_block_cache_metadata_classifies_stable_and_volatile_sources() {
        let project = ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: crate::ContextPlacement::StablePrefix,
            label: "project guidance".to_string(),
            content: "follow repo guidance".to_string(),
        };
        let scheduler = ContextBlock {
            source: ContextSourceKind::Policy,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "scheduler state".to_string(),
            content: "state=idle".to_string(),
        };
        let action = ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "action result".to_string(),
            content: "command output".to_string(),
        };
        let transcript_tool = ContextBlock {
            source: ContextSourceKind::TranscriptTool,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "historical tool result".to_string(),
            content: "prior command output".to_string(),
        };
        let committed_evidence = ContextBlock {
            source: ContextSourceKind::CommittedEvidence,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "committed evidence".to_string(),
            content: "compact prior action evidence".to_string(),
        };
        let pane_identity = ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "pane identity".to_string(),
            content: "pane_id=%1 window_name=0".to_string(),
        };

        assert_eq!(project.placement, crate::ContextPlacement::StablePrefix);
        assert_eq!(project.stability(), ContextStability::Static);
        assert_eq!(project.cache_policy(), ContextCachePolicy::Eligible);
        assert!(project.stable_prefix_eligible());
        assert_eq!(scheduler.placement, crate::ContextPlacement::EphemeralTail);
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

    /// Verifies the narrow block constructors assign the canonical semantic
    /// and retention contracts without conflating those contracts with cache
    /// placement or provider transport role.
    #[test]
    fn context_block_constructors_expose_semantic_and_retention_contracts() {
        let stable = ContextBlock::stable_instruction(
            ContextSourceKind::Policy,
            "stable policy",
            "invariant=true",
        );
        let skill = ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "active skill",
            "follow the workflow",
        );
        let user = ContextBlock::user_event("user prompt", "perform the task");
        let assistant = ContextBlock::assistant_event("assistant action", "run tests");
        let evidence = ContextBlock::evidence_event(
            ContextSourceKind::ActionResult,
            "action result action-1",
            "tests passed",
        );
        let reference = ContextBlock::reference_event(
            ContextSourceKind::LocalMessage,
            "local message",
            "agent-%2: avoid file.rs",
        );
        let live = ContextBlock::live_state(
            ContextSourceKind::RuntimeHint,
            "runtime state",
            "cwd=/workspace",
        );

        assert_eq!(
            stable.semantic_kind(),
            ContextSemanticKind::AmbientInstruction
        );
        assert_eq!(stable.retention(), ContextRetention::Exact);
        assert_eq!(skill.semantic_kind(), ContextSemanticKind::TaskPrelude);
        assert_eq!(skill.retention(), ContextRetention::Exact);
        assert_eq!(user.semantic_kind(), ContextSemanticKind::UserEvent);
        assert_eq!(user.retention(), ContextRetention::Exact);
        assert_eq!(
            assistant.semantic_kind(),
            ContextSemanticKind::AssistantEvent
        );
        assert_eq!(assistant.retention(), ContextRetention::ExecutionGroup);
        assert_eq!(evidence.semantic_kind(), ContextSemanticKind::EvidenceEvent);
        assert_eq!(evidence.retention(), ContextRetention::ExecutionGroup);
        assert_eq!(
            reference.semantic_kind(),
            ContextSemanticKind::ReferenceEvent
        );
        assert_eq!(reference.retention(), ContextRetention::Exact);
        assert_eq!(live.semantic_kind(), ContextSemanticKind::LiveState);
        assert_eq!(live.retention(), ContextRetention::RequestLocal);
    }

    /// Verifies semantic validation accepts one complete canonical request
    /// chronology with an exact task prelude, direct user prompt, execution
    /// group, later reference event, and final factual live state.
    #[test]
    fn context_semantics_accept_canonical_chronology() {
        let blocks = vec![
            ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "stable"),
            ContextBlock::task_prelude(
                ContextSourceKind::SkillInstruction,
                "skill",
                "task workflow",
            ),
            ContextBlock::user_event("user prompt", "do the work"),
            ContextBlock::assistant_event("assistant action", "run command"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "action result action-1",
                "succeeded",
            ),
            ContextBlock::reference_event(
                ContextSourceKind::LocalMessage,
                "local message",
                "avoid overlap",
            ),
            ContextBlock::live_state(ContextSourceKind::RuntimeHint, "live state", "cwd=/repo"),
        ];

        validate_context_semantics(&blocks).unwrap();
    }

    /// Verifies semantic validation rejects events in the request-local tail,
    /// non-instructions in the stable prefix, and task preludes inserted after
    /// the active prompt, with enough diagnostics to identify the producer.
    #[test]
    fn context_semantics_reject_ambiguous_lifetime_and_authorship() {
        let invalid_cases = [
            vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "late user restatement".to_string(),
                content: "do the work".to_string(),
            }],
            vec![ContextBlock {
                source: ContextSourceKind::Memory,
                placement: crate::ContextPlacement::StablePrefix,
                label: "memory".to_string(),
                content: "historical note".to_string(),
            }],
            vec![
                ContextBlock::user_event("user prompt", "do the work"),
                ContextBlock::task_prelude(
                    ContextSourceKind::SkillInstruction,
                    "late skill",
                    "workflow",
                ),
            ],
        ];

        for blocks in invalid_cases {
            let error = validate_context_semantics(&blocks).unwrap_err();
            assert!(error.message().contains("context semantic violation"));
            assert!(error.message().contains("semantic="));
            assert!(error.message().contains("retention="));
        }
    }

    /// Verifies prepared request construction joins live state after immutable
    /// chronology without mutating the durable context retained by the runtime.
    #[test]
    fn prepared_model_context_keeps_live_state_out_of_durable_context() {
        let durable = AgentContext::new_durable(vec![
            ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "stable"),
            ContextBlock::user_event("user prompt", "do the work"),
        ])
        .unwrap();
        let original = durable.clone();
        let prepared = PreparedModelContext::new(
            durable,
            vec![ContextBlock::live_state(
                ContextSourceKind::RuntimeHint,
                "runtime state",
                "cwd=/repo",
            )],
        )
        .unwrap();

        assert_eq!(prepared.durable(), &original);
        assert_eq!(prepared.live_state().len(), 1);
        assert_eq!(prepared.len(), 3);
        let ordered = prepared.to_agent_context();
        assert_eq!(ordered.blocks[1].source, ContextSourceKind::UserInstruction);
        assert_eq!(
            ordered.blocks[2].semantic_kind(),
            ContextSemanticKind::LiveState
        );
        assert!(
            prepared
                .durable()
                .blocks
                .iter()
                .all(|block| { block.placement != crate::ContextPlacement::EphemeralTail })
        );
    }

    /// Verifies prepared request construction rejects event-like tail blocks
    /// and rejects durable storage that already contains request-local state.
    #[test]
    fn prepared_model_context_rejects_invalid_phase_ownership() {
        let durable =
            AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "do the work")])
                .unwrap();
        let event_tail = ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "action result action-1".to_string(),
            content: "succeeded".to_string(),
        };
        let error = PreparedModelContext::new(durable, vec![event_tail]).unwrap_err();
        assert!(error.message().contains("ephemeral-tail blocks"));

        let error = AgentContext::new_durable(vec![ContextBlock::live_state(
            ContextSourceKind::RuntimeHint,
            "runtime state",
            "cwd=/repo",
        )])
        .unwrap_err();
        assert!(error.message().contains("durable agent context"));
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

    /// Verifies settlement atomically replaces volatile evidence with one
    /// immutable chronological result and remains idempotent on replay.
    #[test]
    fn settled_action_result_commit_removes_volatile_copy_exactly_once() {
        let running = action_result("action-1", ActionStatus::Running, "still running");
        let settled = action_result("action-1", ActionStatus::Succeeded, "finished");
        let mut context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                label: "system".to_string(),
                content: "policy".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "action result action-1".to_string(),
                content: crate::action_result_context_content(&running),
            },
            ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "scheduler".to_string(),
                content: "waiting".to_string(),
            },
        ])
        .unwrap();

        assert_eq!(
            context.commit_settled_action_results(&[settled]).unwrap(),
            1
        );
        let committed = context.blocks.clone();
        assert_eq!(
            context
                .commit_settled_action_results(&[action_result(
                    "action-1",
                    ActionStatus::Succeeded,
                    "finished",
                )])
                .unwrap(),
            0
        );

        assert_eq!(context.blocks, committed);
        let action_blocks = context
            .blocks
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult)
            .collect::<Vec<_>>();
        assert_eq!(action_blocks.len(), 1);
        assert_eq!(
            action_blocks[0].placement,
            crate::ContextPlacement::ConversationAppend
        );
        context.validate_placement_order().unwrap();
    }

    /// Verifies a batch containing unresolved controller state is rejected
    /// before any otherwise terminal sibling can mutate chronology.
    #[test]
    fn settled_action_result_commit_rejects_unresolved_batches_atomically() {
        let mut context = AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: crate::ContextPlacement::StablePrefix,
            label: "system".to_string(),
            content: "policy".to_string(),
        }])
        .unwrap();
        let original = context.clone();

        let error = context
            .commit_settled_action_results(&[
                action_result("action-1", ActionStatus::Succeeded, "finished"),
                action_result("action-2", ActionStatus::Running, "running"),
            ])
            .unwrap_err();

        assert_eq!(
            error.message(),
            "only terminal action results may be committed to immutable chronology"
        );
        assert_eq!(context, original);
    }
}
