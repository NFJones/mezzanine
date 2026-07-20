//! Provider-independent agent context validation contracts.
//!
//! This module owns typed stable slots, monotonic conversation events,
//! request-local live state, and deterministic validation failures for context
//! and model-profile selection. Typed collections are authoritative; ordered
//! blocks and metadata are read-only projections rebuilt after a checked
//! mutation. Mutations validate an isolated candidate before commit, direct
//! user events are exact, evidence requires a preceding causal owner, and no
//! request-local state may survive in durable context. Product prompt assets,
//! transcript persistence, and provider execution remain outside this crate
//! and adapt these contracts at their composition boundaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

use sha2::{Digest, Sha256};

use crate::action_result::ActionResult;
use crate::action_result_context::action_result_context_content;
use crate::mcp::McpPromptTool;
use crate::surface::{AllowedActionSet, ModelInteractionKind};
use crate::{AgentPromptError, AgentPromptErrorKind, ProviderTranscriptEvent};

/// Sequence spacing reserved between newly appended canonical events.
///
/// Sparse identities let a compaction-refresh import replace a historical
/// prefix with a different number of records while preserving every retained
/// prompt, steering, message, and same-turn event identity.
const CONTEXT_EVENT_SEQUENCE_STRIDE: u64 = 1024;

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

/// Monotonic identity assigned when one chronological event commits.
///
/// Stable instructions and request-local live state do not have event
/// sequences. Conversation events receive exactly one sequence and keep it
/// through provider projection and compaction-range replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextEventSequence(u64);

impl ContextEventSequence {
    /// Creates a non-zero committed event sequence.
    pub fn new(value: u64) -> AgentContextResult<Self> {
        if value == 0 {
            return Err(AgentContextError::new(
                "context event sequence must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the numeric sequence value.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Stable identity shared by one assistant execution and its result events.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextExecutionGroupId(String);

impl ContextExecutionGroupId {
    /// Creates a non-empty execution-group identity.
    pub fn new(value: impl Into<String>) -> AgentContextResult<Self> {
        let value = value.into();
        validate_context_required("context execution group id", &value)?;
        Ok(Self(value))
    }

    /// Returns the underlying group identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity for one replaceable reusable-prefix slot.
///
/// Slot identity is controller metadata and is never rendered into model
/// context. Producers use it to update mutable-on-source-change authority
/// without removing and re-appending an indistinguishable prefix block.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableContextSlotId(String);

impl StableContextSlotId {
    /// Creates one non-empty stable slot identity.
    pub fn new(value: impl Into<String>) -> AgentContextResult<Self> {
        let value = value.into();
        validate_context_required("stable context slot id", &value)?;
        Ok(Self(value))
    }

    /// Returns the stable slot identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Cryptographic fingerprint of the source material for one stable slot.
///
/// The digest remains outside model-visible text and lets refresh code
/// distinguish a true source change from repeated discovery of identical
/// authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableContextSourceFingerprint(String);

impl StableContextSourceFingerprint {
    /// Creates a validated lowercase SHA-256 source fingerprint.
    pub fn new(value: impl Into<String>) -> AgentContextResult<Self> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AgentContextError::new(
                "stable context source fingerprint must be 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the lowercase SHA-256 digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One producer-classified reusable-prefix block with replacement identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableContextBlock {
    slot_id: StableContextSlotId,
    source_fingerprint: StableContextSourceFingerprint,
    block: ContextBlock,
}

impl StableContextBlock {
    /// Creates one stable ambient-instruction slot.
    pub fn new(
        slot_id: StableContextSlotId,
        source_fingerprint: StableContextSourceFingerprint,
        block: ContextBlock,
    ) -> AgentContextResult<Self> {
        if block.placement != ContextPlacement::StablePrefix
            || block.semantic_kind() != ContextSemanticKind::AmbientInstruction
        {
            return Err(AgentContextError::new(
                "stable context slots require stable-prefix ambient instructions",
            ));
        }
        Ok(Self {
            slot_id,
            source_fingerprint,
            block,
        })
    }

    /// Returns the non-model-visible slot identity.
    pub fn slot_id(&self) -> &StableContextSlotId {
        &self.slot_id
    }

    /// Returns the non-model-visible source fingerprint.
    pub fn source_fingerprint(&self) -> &StableContextSourceFingerprint {
        &self.source_fingerprint
    }

    /// Returns the exact model-visible stable block.
    pub fn block(&self) -> &ContextBlock {
        &self.block
    }

    /// Builds a deterministic compatibility slot for an already ordered block
    /// vector at a legacy/test construction boundary.
    fn from_compatibility_block(block: ContextBlock, index: usize) -> AgentContextResult<Self> {
        let mut identity_material = format!("{index}:{:?}:{}", block.source, block.label);
        let identity_digest = Sha256::digest(identity_material.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        identity_material.push('\0');
        identity_material.push_str(&block.content);
        let source_fingerprint = Sha256::digest(identity_material.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self::new(
            StableContextSlotId::new(format!("compat-{identity_digest}"))?,
            StableContextSourceFingerprint::new(source_fingerprint)?,
            block,
        )
    }

    /// Builds the adapter-facing metadata projection for this stable slot.
    fn metadata(&self) -> ContextBlockMetadata {
        ContextBlockMetadata {
            semantic_kind: ContextSemanticKind::AmbientInstruction,
            retention: ContextRetention::Exact,
            event_sequence: None,
            execution_group_id: None,
            provider_owner: None,
            recoverable_for_compaction: false,
            stable_slot_id: Some(self.slot_id.clone()),
            stable_source_fingerprint: Some(self.source_fingerprint.clone()),
        }
    }
}

/// Provider that exclusively owns one opaque continuity event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderContinuityOwner {
    /// DeepSeek thinking/tool-call replay state.
    DeepSeek,
}

impl ProviderContinuityOwner {
    /// Returns whether this owner matches a configured provider id.
    pub fn matches_provider(self, provider: &str) -> bool {
        match self {
            Self::DeepSeek => provider == "deepseek",
        }
    }
}

/// Stored causal and retention properties for one canonical context block.
///
/// These values are captured when the producer commits the block. Provider
/// preparation and compaction consume them directly and must not reconstruct
/// semantics from labels or transport roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlockMetadata {
    semantic_kind: ContextSemanticKind,
    retention: ContextRetention,
    event_sequence: Option<ContextEventSequence>,
    execution_group_id: Option<ContextExecutionGroupId>,
    provider_owner: Option<ProviderContinuityOwner>,
    recoverable_for_compaction: bool,
    stable_slot_id: Option<StableContextSlotId>,
    stable_source_fingerprint: Option<StableContextSourceFingerprint>,
}

impl ContextBlockMetadata {
    /// Returns the producer-selected semantic kind.
    pub fn semantic_kind(&self) -> ContextSemanticKind {
        self.semantic_kind
    }

    /// Returns the producer-selected retention policy.
    pub fn retention(&self) -> ContextRetention {
        self.retention
    }

    /// Returns the committed chronological sequence, when applicable.
    pub fn event_sequence(&self) -> Option<ContextEventSequence> {
        self.event_sequence
    }

    /// Returns the owning execution group, when applicable.
    pub fn execution_group_id(&self) -> Option<&ContextExecutionGroupId> {
        self.execution_group_id.as_ref()
    }

    /// Returns the exclusive provider owner for opaque continuity state.
    pub fn provider_owner(&self) -> Option<ProviderContinuityOwner> {
        self.provider_owner
    }

    /// Reports whether exact content can be recovered for semantic compaction.
    pub fn recoverable_for_compaction(&self) -> bool {
        self.recoverable_for_compaction
    }

    /// Returns the replacement identity for an explicitly slotted stable block.
    pub fn stable_slot_id(&self) -> Option<&StableContextSlotId> {
        self.stable_slot_id.as_ref()
    }

    /// Returns the source fingerprint for an explicitly slotted stable block.
    pub fn stable_source_fingerprint(&self) -> Option<&StableContextSourceFingerprint> {
        self.stable_source_fingerprint.as_ref()
    }
}

/// One immutable chronological event stored by durable agent context.
///
/// Event identity, semantics, retention, execution ownership, and provider
/// ownership are captured together when the event commits. They cannot drift
/// from the event block through a parallel metadata mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationEvent {
    block: ContextBlock,
    semantic_kind: ContextSemanticKind,
    retention: ContextRetention,
    sequence: ContextEventSequence,
    execution_group_id: Option<ContextExecutionGroupId>,
    provider_owner: Option<ProviderContinuityOwner>,
    recoverable_for_compaction: bool,
}

impl ConversationEvent {
    /// Returns the exact chronological model-context block.
    pub fn block(&self) -> &ContextBlock {
        &self.block
    }

    /// Returns the monotonic commit sequence.
    pub fn sequence(&self) -> ContextEventSequence {
        self.sequence
    }

    /// Returns the producer-selected semantic kind.
    pub fn semantic_kind(&self) -> ContextSemanticKind {
        self.semantic_kind
    }

    /// Returns the producer-selected retention rule.
    pub fn retention(&self) -> ContextRetention {
        self.retention
    }

    /// Returns the owning assistant execution group, when applicable.
    pub fn execution_group_id(&self) -> Option<&ContextExecutionGroupId> {
        self.execution_group_id.as_ref()
    }

    /// Returns the exclusive provider continuity owner, when applicable.
    pub fn provider_owner(&self) -> Option<ProviderContinuityOwner> {
        self.provider_owner
    }

    /// Reports whether exact source content can be recovered after compaction.
    pub fn recoverable_for_compaction(&self) -> bool {
        self.recoverable_for_compaction
    }

    /// Builds the adapter-facing metadata projection for this event.
    fn metadata(&self) -> ContextBlockMetadata {
        ContextBlockMetadata {
            semantic_kind: self.semantic_kind,
            retention: self.retention,
            event_sequence: Some(self.sequence),
            execution_group_id: self.execution_group_id.clone(),
            provider_owner: self.provider_owner,
            recoverable_for_compaction: self.recoverable_for_compaction,
            stable_slot_id: None,
            stable_source_fingerprint: None,
        }
    }
}

/// One factual request-local block stored only by a prepared model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStateBlock {
    block: ContextBlock,
}

impl LiveStateBlock {
    /// Creates one validated request-local live-state block.
    pub fn new(block: ContextBlock) -> AgentContextResult<Self> {
        if block.placement != ContextPlacement::EphemeralTail
            || block.semantic_kind() != ContextSemanticKind::LiveState
            || block.retention() != ContextRetention::RequestLocal
        {
            return Err(AgentContextError::new(
                "live-state blocks require ephemeral-tail request-local semantics",
            ));
        }
        Ok(Self { block })
    }

    /// Returns the exact model-visible live-state block.
    pub fn block(&self) -> &ContextBlock {
        &self.block
    }

    /// Builds the adapter-facing metadata projection for this live state.
    fn metadata(&self) -> ContextBlockMetadata {
        ContextBlockMetadata {
            semantic_kind: ContextSemanticKind::LiveState,
            retention: ContextRetention::RequestLocal,
            event_sequence: None,
            execution_group_id: None,
            provider_owner: None,
            recoverable_for_compaction: false,
            stable_slot_id: None,
            stable_source_fingerprint: None,
        }
    }
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
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult => ContextSemanticKind::EvidenceEvent,
            ContextSourceKind::SkillInstruction => ContextSemanticKind::TaskPrelude,
            ContextSourceKind::LocalMessage
            | ContextSourceKind::Memory
            | ContextSourceKind::Transcript
            | ContextSourceKind::RoutedHandoff => ContextSemanticKind::ReferenceEvent,
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
            | ContextSourceKind::RuntimeHint
            | ContextSourceKind::RoutedHandoff => ContextRetention::Exact,
            ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult => ContextRetention::ExecutionGroup,
            ContextSourceKind::Memory
            | ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser => ContextRetention::Summarizable,
        }
    }

    /// Returns whether exact content can be recovered outside model context.
    pub fn recoverable_for_compaction(&self) -> bool {
        if self.placement == ContextPlacement::EphemeralTail {
            return false;
        }
        matches!(
            self.source,
            ContextSourceKind::Memory
                | ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
                | ContextSourceKind::CommittedEvidence
                | ContextSourceKind::RoutedHandoff
                | ContextSourceKind::RuntimeHint
                | ContextSourceKind::ActionResult
                | ContextSourceKind::LocalMessage
        )
    }
}

/// Typed request metadata that never becomes a model-visible context block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelContextMetadata {
    /// Live product session identity used only for diagnostics and
    /// provider-owned conversation continuity.
    pub prompt_cache_session_id: Option<String>,
    /// Stable prompt-cache lineage used only for provider cache routing.
    pub prompt_cache_lineage_id: Option<String>,
}

impl ModelContextMetadata {
    /// Builds typed non-model-visible request metadata.
    pub fn new(
        prompt_cache_session_id: Option<impl Into<String>>,
        prompt_cache_lineage_id: Option<impl Into<String>>,
    ) -> Self {
        Self {
            prompt_cache_session_id: prompt_cache_session_id.map(Into::into),
            prompt_cache_lineage_id: prompt_cache_lineage_id.map(Into::into),
        }
    }
}

/// Ordered context supplied to provider request assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    /// Replaceable reusable-prefix slots owned by explicit producers.
    stable_slots: Vec<StableContextBlock>,
    /// Immutable chronological events in actor commit order.
    chronology: Vec<ConversationEvent>,
    /// Request-local state present only on compatibility provider views.
    live_state: Vec<LiveStateBlock>,
    /// Read-only ordered projection consumed by existing provider adapters.
    blocks: Vec<ContextBlock>,
    /// Read-only metadata projection aligned one-to-one with `blocks`.
    block_metadata: Vec<ContextBlockMetadata>,
    /// Next sequence reserved for a future committed conversation event.
    next_event_sequence: u64,
    /// Typed request metadata excluded from model-message projection.
    metadata: ModelContextMetadata,
}

impl AgentContext {
    /// Creates an empty durable context for an intermediate composition stage.
    ///
    /// Provider request assembly still requires at least one model-visible
    /// block, but routed and restored builders may legitimately remove every
    /// inherited block before appending the new task's own prompt.
    pub fn empty() -> Self {
        Self {
            stable_slots: Vec::new(),
            chronology: Vec::new(),
            live_state: Vec::new(),
            blocks: Vec::new(),
            block_metadata: Vec::new(),
            next_event_sequence: CONTEXT_EVENT_SEQUENCE_STRIDE,
            metadata: ModelContextMetadata::default(),
        }
    }

    /// Imports one ordered compatibility block sequence into typed storage.
    ///
    /// This boundary is reserved for transcript restoration, staged initial
    /// composition, migration, and fixtures that do not already own typed
    /// event records. It never sorts input and applies the explicit safe import
    /// policy documented by [`compatibility_event_contract`].
    pub fn import_ordered_blocks(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        let mut context = Self::empty();
        context.initialize_typed_storage_from_blocks(blocks)?;
        context.revalidate()
    }

    /// Creates validated non-empty compatibility context.
    ///
    /// New product composition should name its import boundary explicitly with
    /// [`AgentContext::import_ordered_blocks`]. This alias remains for compact
    /// lower-crate fixtures and provider contract tests.
    pub fn new(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        Self::import_ordered_blocks(blocks)
    }

    /// Revalidates context blocks without discarding typed request metadata.
    pub fn revalidate(self) -> AgentContextResult<Self> {
        if self.blocks.is_empty() {
            return Err(AgentContextError::new(
                "agent context must contain at least one context block",
            ));
        }
        for block in &self.blocks {
            validate_context_required("context label", &block.label)?;
        }
        self.validate_stored_metadata()?;
        Ok(self)
    }

    /// Creates durable context containing only stable and append-only blocks.
    ///
    /// Runtime turn storage must use this constructor so request-local state
    /// cannot accidentally survive into later provider calls.
    pub fn import_durable_blocks(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        let context = Self::import_ordered_blocks(blocks)?;
        context.validate_durable()?;
        Ok(context)
    }

    /// Creates durable compatibility context for lower-crate fixtures.
    ///
    /// Product restoration and staged construction should use
    /// [`AgentContext::import_durable_blocks`] so inference is visibly confined
    /// to an audited import boundary.
    pub fn new_durable(blocks: Vec<ContextBlock>) -> AgentContextResult<Self> {
        Self::import_durable_blocks(blocks)
    }

    /// Attaches typed non-model-visible metadata to this context.
    pub fn with_metadata(mut self, metadata: ModelContextMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns the canonical block sequence without permitting direct mutation.
    pub fn blocks(&self) -> &[ContextBlock] {
        &self.blocks
    }

    /// Returns typed reusable-prefix slots in model-visible order.
    pub fn stable_slots(&self) -> &[StableContextBlock] {
        &self.stable_slots
    }

    /// Returns immutable chronological events in actor commit order.
    pub fn chronology(&self) -> &[ConversationEvent] {
        &self.chronology
    }

    /// Returns typed request metadata that is excluded from model messages.
    pub fn metadata(&self) -> &ModelContextMetadata {
        &self.metadata
    }

    /// Replaces typed non-model-visible request metadata.
    pub fn set_metadata(&mut self, metadata: ModelContextMetadata) {
        self.metadata = metadata;
    }

    /// Returns stored causal metadata aligned with [`AgentContext::blocks`].
    pub fn block_metadata(&self) -> &[ContextBlockMetadata] {
        &self.block_metadata
    }

    /// Returns the highest committed conversation-event sequence.
    pub fn event_sequence_high_water_mark(&self) -> u64 {
        self.chronology
            .last()
            .map_or(0, |event| event.sequence.get())
    }

    /// Returns metadata for one canonical block index.
    pub fn metadata_for_block(&self, index: usize) -> Option<&ContextBlockMetadata> {
        self.block_metadata.get(index)
    }

    /// Appends an exact user event at the next chronological sequence.
    pub fn append_user_event(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::user_event(label, content),
            ContextSemanticKind::UserEvent,
            ContextRetention::Exact,
            None,
            None,
            false,
        )
    }

    /// Appends an assistant response owned by one execution group.
    pub fn append_assistant_event(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::assistant_event(label, content),
            ContextSemanticKind::AssistantEvent,
            ContextRetention::ExecutionGroup,
            Some(execution_group_id),
            None,
            true,
        )
    }

    /// Appends settled evidence owned by one execution group.
    pub fn append_evidence_event(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
        provider_owner: Option<ProviderContinuityOwner>,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::evidence_event(source, label, content),
            ContextSemanticKind::EvidenceEvent,
            ContextRetention::ExecutionGroup,
            Some(execution_group_id),
            provider_owner,
            recoverable_for_compaction,
        )
    }

    /// Appends an exact neutral reference event.
    pub fn append_reference_event(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::reference_event(source, label, content),
            ContextSemanticKind::ReferenceEvent,
            ContextRetention::Exact,
            None,
            None,
            true,
        )
    }

    /// Appends a typed task prelude before the first direct-user event.
    pub fn append_task_prelude(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        retention: ContextRetention,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::task_prelude(source, label, content),
            ContextSemanticKind::TaskPrelude,
            retention,
            None,
            None,
            true,
        )
    }

    /// Reclassifies one exact direct-user event as an exact neutral reference
    /// without moving it in chronology.
    ///
    /// Routed child construction uses this when the ordinary pane-context
    /// builder has initially represented the controller-authored task as a
    /// direct prompt. The replacement preserves the event sequence and rejects
    /// ambiguous or multiple matches.
    pub fn reclassify_user_event_as_reference(
        &mut self,
        content: &str,
        source: ContextSourceKind,
        label: impl Into<String>,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.reclassify_user_event_as_reference_candidate(content, source, label)?;
        *self = candidate;
        Ok(())
    }

    /// Applies one already isolated user-event reclassification candidate.
    fn reclassify_user_event_as_reference_candidate(
        &mut self,
        content: &str,
        source: ContextSourceKind,
        label: impl Into<String>,
    ) -> AgentContextResult<()> {
        let matching = self
            .chronology
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.block.source == ContextSourceKind::UserInstruction
                    && event.block.content == content
                    && event.semantic_kind == ContextSemanticKind::UserEvent
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(AgentContextError::new(format!(
                "user-event reclassification requires exactly one match; found {}",
                matching.len()
            )));
        }
        let index = matching[0];
        let replacement = ContextBlock::reference_event(source, label, content);
        let event = &mut self.chronology[index];
        event.block = replacement;
        event.semantic_kind = ContextSemanticKind::ReferenceEvent;
        event.retention = ContextRetention::Exact;
        event.execution_group_id = None;
        event.provider_owner = None;
        event.recoverable_for_compaction = true;
        self.rebuild_projections();
        self.validate_stored_metadata()
    }

    /// Removes blocks matching a predicate without exposing a partially valid
    /// chronology when removal would break a causal ownership invariant.
    pub fn retain_blocks(
        &mut self,
        mut keep: impl FnMut(&ContextBlock) -> bool,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.stable_slots.retain(|slot| keep(&slot.block));
        candidate.chronology.retain(|event| keep(&event.block));
        candidate.live_state.retain(|state| keep(&state.block));
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        validate_context_placement_order(&candidate.blocks)?;
        validate_context_semantics(&candidate.blocks)?;
        *self = candidate;
        Ok(())
    }

    /// Replaces every stable slot owned by one source at the source's existing
    /// prefix anchor.
    ///
    /// Identical slot ids, fingerprints, and model-visible bytes are an exact
    /// no-op. A changed slot set replaces one contiguous stable range without
    /// moving any other stable authority or chronological event. Fragmented
    /// ownership is rejected because silently gathering it would reorder an
    /// intervening causal or authority boundary.
    pub fn replace_stable_source_slots(
        &mut self,
        source: ContextSourceKind,
        slots: Vec<StableContextBlock>,
    ) -> AgentContextResult<bool> {
        let mut candidate = self.clone();
        let changed = candidate.replace_stable_source_slots_candidate(source, slots)?;
        *self = candidate;
        Ok(changed)
    }

    /// Applies one stable-source replacement to an isolated candidate.
    fn replace_stable_source_slots_candidate(
        &mut self,
        source: ContextSourceKind,
        slots: Vec<StableContextBlock>,
    ) -> AgentContextResult<bool> {
        let mut slot_ids = BTreeSet::new();
        for slot in &slots {
            if slot.block.source != source {
                return Err(AgentContextError::new(
                    "stable slot source does not match replacement owner",
                ));
            }
            if !slot_ids.insert(slot.slot_id.as_str()) {
                return Err(AgentContextError::new(
                    "stable source replacement contains a duplicate slot id",
                ));
            }
        }

        if self
            .chronology
            .iter()
            .any(|event| event.block.source == source)
            || self
                .live_state
                .iter()
                .any(|state| state.block.source == source)
        {
            return Err(AgentContextError::new(
                "stable source replacement found the source outside the stable prefix",
            ));
        }
        let existing_indices = self
            .stable_slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.block.source == source)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if existing_indices
            .windows(2)
            .any(|pair| pair[1] != pair[0].saturating_add(1))
        {
            return Err(AgentContextError::new(
                "stable source slots are fragmented across another prefix owner",
            ));
        }

        let unchanged = existing_indices.len() == slots.len()
            && existing_indices
                .iter()
                .zip(&slots)
                .all(|(index, slot)| self.stable_slots[*index] == *slot);
        if unchanged {
            return Ok(false);
        }

        let insertion_index = existing_indices
            .first()
            .copied()
            .unwrap_or(self.stable_slots.len());
        let removal_end = existing_indices
            .last()
            .map_or(insertion_index, |index| index.saturating_add(1));
        self.stable_slots
            .splice(insertion_index..removal_end, slots);
        self.rebuild_projections();
        self.validate_stored_metadata()?;
        validate_context_placement_order(&self.blocks)?;
        validate_context_semantics(&self.blocks)?;
        Ok(true)
    }

    /// Returns the source fingerprint stored for one named stable slot.
    pub fn stable_slot_source_fingerprint(
        &self,
        slot_id: &str,
    ) -> Option<&StableContextSourceFingerprint> {
        self.stable_slots
            .iter()
            .find(|slot| slot.slot_id.as_str() == slot_id)
            .map(|slot| &slot.source_fingerprint)
    }

    /// Inserts a new block at its lifecycle boundary and records its semantics.
    ///
    /// This compatibility operation is intended for stable/task-prelude/live
    /// assembly helpers. Runtime conversation events must use the narrower
    /// append methods so execution ownership is explicit.
    pub fn insert_typed_block(
        &mut self,
        block: ContextBlock,
        semantic_kind: ContextSemanticKind,
        retention: ContextRetention,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<Option<ContextEventSequence>> {
        let mut candidate = self.clone();
        let sequence = candidate.insert_typed_block_candidate(
            block,
            semantic_kind,
            retention,
            recoverable_for_compaction,
        )?;
        *self = candidate;
        Ok(sequence)
    }

    /// Applies one compatibility insertion to an isolated candidate.
    fn insert_typed_block_candidate(
        &mut self,
        block: ContextBlock,
        semantic_kind: ContextSemanticKind,
        retention: ContextRetention,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<Option<ContextEventSequence>> {
        let sequence = match block.placement {
            ContextPlacement::StablePrefix => {
                let slot =
                    StableContextBlock::from_compatibility_block(block, self.stable_slots.len())?;
                if semantic_kind != ContextSemanticKind::AmbientInstruction
                    || retention != ContextRetention::Exact
                    || recoverable_for_compaction
                {
                    return Err(AgentContextError::new(
                        "stable compatibility insertion requires exact ambient instruction semantics",
                    ));
                }
                self.stable_slots.push(slot);
                None
            }
            ContextPlacement::ConversationAppend => {
                let provider_owner = provider_owner_for_block(&block);
                let sequence = self.allocate_event_sequence()?;
                let execution_group_id = if retention == ContextRetention::ExecutionGroup {
                    Some(
                        self.chronology
                            .last()
                            .and_then(|event| event.execution_group_id.clone())
                            .unwrap_or(ContextExecutionGroupId::new(format!(
                                "compat-insert-execution-group-{}",
                                sequence.get()
                            ))?),
                    )
                } else {
                    None
                };
                let event = ConversationEvent {
                    block,
                    semantic_kind,
                    retention,
                    sequence,
                    execution_group_id,
                    provider_owner,
                    recoverable_for_compaction,
                };
                validate_context_block_metadata(
                    self.stable_slots.len() + self.chronology.len(),
                    &event.block,
                    &event.metadata(),
                )?;
                self.chronology.push(event);
                Some(sequence)
            }
            ContextPlacement::EphemeralTail => {
                if semantic_kind != ContextSemanticKind::LiveState
                    || retention != ContextRetention::RequestLocal
                    || recoverable_for_compaction
                {
                    return Err(AgentContextError::new(
                        "live-state insertion requires request-local non-recoverable semantics",
                    ));
                }
                self.live_state.push(LiveStateBlock::new(block)?);
                None
            }
        };
        self.rebuild_projections();
        self.validate_stored_metadata()?;
        Ok(sequence)
    }

    /// Removes the request-local suffix and returns it for prepared-request
    /// construction without changing durable event identity.
    pub fn split_off_live_state(&mut self) -> Vec<ContextBlock> {
        let live_state = self
            .live_state
            .drain(..)
            .map(|state| state.block)
            .collect::<Vec<_>>();
        self.rebuild_projections();
        live_state
    }

    /// Replaces chronology for compatibility fixtures and imported snapshots.
    ///
    /// Product compaction uses [`Self::compact_execution_ranges`] and active
    /// history refresh uses [`Self::replace_imported_history_prefix`] so that
    /// unaffected event identities survive either mutation. This whole-history
    /// replacement deliberately re-sequences the supplied validated order and
    /// therefore establishes a fresh cache lineage.
    pub fn replace_after_compaction(
        &mut self,
        blocks: Vec<ContextBlock>,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.initialize_typed_storage_from_blocks(blocks)?;
        candidate.validate_durable()?;
        *self = candidate;
        Ok(())
    }

    /// Replaces one imported historical prefix without changing retained event
    /// identities.
    ///
    /// The ownership predicate must select a contiguous prefix of conversation
    /// chronology. Replacement records receive fresh sparse sequences strictly
    /// before the first retained event, while retained prompt, steering,
    /// message, assistant, and evidence records keep their original sequence
    /// and group metadata. A replacement that cannot fit in the reserved
    /// sequence interval fails atomically instead of renumbering or moving
    /// retained chronology.
    pub fn replace_imported_history_prefix(
        &mut self,
        mut owns: impl FnMut(&ContextBlock) -> bool,
        blocks: Vec<ContextBlock>,
    ) -> AgentContextResult<usize> {
        if blocks
            .iter()
            .any(|block| block.placement != ContextPlacement::ConversationAppend)
        {
            return Err(AgentContextError::new(
                "imported history replacement accepts conversation events only",
            ));
        }
        let mut candidate = self.clone();
        let owned_indices = candidate
            .chronology
            .iter()
            .enumerate()
            .filter(|(_, event)| owns(&event.block))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if owned_indices.iter().copied().ne(0..owned_indices.len()) {
            return Err(AgentContextError::new(
                "imported history ownership must be one contiguous chronology prefix",
            ));
        }
        let remove_end = owned_indices.len();
        let successor_sequence = candidate
            .chronology
            .get(remove_end)
            .map(|event| event.sequence.get());

        let mut imported = Self::empty();
        if !blocks.is_empty() {
            imported.initialize_typed_storage_from_blocks(blocks)?;
        }
        if !imported.stable_slots.is_empty() || !imported.live_state.is_empty() {
            return Err(AgentContextError::new(
                "imported history replacement produced a non-chronological block",
            ));
        }
        let replacement_count = imported.chronology.len();
        let assigned_sequences =
            history_prefix_replacement_sequences(replacement_count, successor_sequence)?;
        let mut replacement_events = imported.chronology;
        let mut replacement_groups = BTreeMap::<String, ContextExecutionGroupId>::new();
        for (event, sequence) in replacement_events.iter_mut().zip(assigned_sequences) {
            event.sequence = ContextEventSequence::new(sequence)?;
            if let Some(old_group) = event.execution_group_id.as_ref() {
                let old_group_key = old_group.as_str().to_string();
                let replacement_group = if let Some(group) = replacement_groups.get(&old_group_key)
                {
                    group.clone()
                } else {
                    let group = ContextExecutionGroupId::new(format!(
                        "history-import:{sequence}:{old_group_key}"
                    ))?;
                    replacement_groups.insert(old_group_key, group.clone());
                    group
                };
                event.execution_group_id = Some(replacement_group);
            }
        }
        candidate
            .chronology
            .splice(0..remove_end, replacement_events);
        if let Some(last_sequence) = candidate
            .chronology
            .last()
            .map(|event| event.sequence.get())
            && candidate.next_event_sequence <= last_sequence
        {
            candidate.next_event_sequence = last_sequence
                .checked_add(CONTEXT_EVENT_SEQUENCE_STRIDE)
                .ok_or_else(|| AgentContextError::new("context event sequence exhausted"))?;
        }
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        *self = candidate;
        Ok(replacement_count)
    }

    /// Replaces closed chronological ranges with semantic summaries while
    /// preserving every retained event identity.
    ///
    /// Ranges are expressed in typed chronology indexes, must be ordered and
    /// non-overlapping, and are applied from newest to oldest. Each summary
    /// inherits the first replaced sequence so later retained events keep their
    /// original identities and strict temporal order. Exact barriers and
    /// unrecoverable events cannot be replaced.
    pub fn compact_execution_ranges(
        &mut self,
        replacements: Vec<(Range<usize>, ContextBlock)>,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.compact_execution_ranges_candidate(replacements)?;
        *self = candidate;
        Ok(())
    }

    /// Applies validated compaction replacements to an isolated candidate.
    fn compact_execution_ranges_candidate(
        &mut self,
        replacements: Vec<(Range<usize>, ContextBlock)>,
    ) -> AgentContextResult<()> {
        let mut previous_end = 0usize;
        for (index, (range, summary)) in replacements.iter().enumerate() {
            if range.is_empty()
                || range.end > self.chronology.len()
                || (index > 0 && range.start < previous_end)
            {
                return Err(AgentContextError::new(
                    "compaction ranges must be non-empty, in bounds, ordered, and non-overlapping",
                ));
            }
            if summary.placement != ContextPlacement::ConversationAppend
                || summary.semantic_kind() != ContextSemanticKind::ReferenceEvent
                || summary.retention() != ContextRetention::Summarizable
            {
                return Err(AgentContextError::new(
                    "compaction replacement must be a summarizable chronological reference event",
                ));
            }
            let replaced = &self.chronology[range.clone()];
            if replaced.iter().any(|event| {
                event.retention == ContextRetention::Exact || !event.recoverable_for_compaction
            }) {
                return Err(AgentContextError::new(
                    "compaction cannot replace exact or unrecoverable chronological events",
                ));
            }
            previous_end = range.end;
        }

        for (range, summary) in replacements.into_iter().rev() {
            let sequence = self.chronology[range.start].sequence;
            let summary_event = ConversationEvent {
                block: summary,
                semantic_kind: ContextSemanticKind::ReferenceEvent,
                retention: ContextRetention::Summarizable,
                sequence,
                execution_group_id: None,
                provider_owner: None,
                recoverable_for_compaction: false,
            };
            self.chronology.splice(range, [summary_event]);
        }
        self.rebuild_projections();
        self.validate_durable()
    }

    /// Appends one producer-classified chronological event.
    fn append_conversation_event(
        &mut self,
        block: ContextBlock,
        semantic_kind: ContextSemanticKind,
        retention: ContextRetention,
        execution_group_id: Option<ContextExecutionGroupId>,
        provider_owner: Option<ProviderContinuityOwner>,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<ContextEventSequence> {
        if block.placement != ContextPlacement::ConversationAppend {
            return Err(AgentContextError::new(
                "chronological events must use conversation-append placement",
            ));
        }
        let mut candidate = self.clone();
        let sequence = candidate.allocate_event_sequence()?;
        let event = ConversationEvent {
            block,
            semantic_kind,
            retention,
            sequence,
            execution_group_id,
            provider_owner,
            recoverable_for_compaction,
        };
        validate_context_block_metadata(
            candidate.stable_slots.len() + candidate.chronology.len(),
            &event.block,
            &event.metadata(),
        )?;
        candidate.chronology.push(event);
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        *self = candidate;
        Ok(sequence)
    }

    /// Allocates the next non-zero chronological sequence.
    fn allocate_event_sequence(&mut self) -> AgentContextResult<ContextEventSequence> {
        let sequence = ContextEventSequence::new(self.next_event_sequence)?;
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(CONTEXT_EVENT_SEQUENCE_STRIDE)
            .ok_or_else(|| AgentContextError::new("context event sequence exhausted"))?;
        Ok(sequence)
    }

    /// Initializes typed storage from one already ordered compatibility block
    /// vector.
    ///
    /// This boundary exists for fixtures and legacy adapters. Active runtime
    /// producers mutate context through typed APIs and never infer semantics
    /// after an event has committed.
    fn initialize_typed_storage_from_blocks(
        &mut self,
        blocks: Vec<ContextBlock>,
    ) -> AgentContextResult<()> {
        validate_context_placement_order(&blocks)?;
        validate_context_semantics(&blocks)?;
        let compatibility_group_ids = compatibility_execution_group_ids(&blocks)?;
        let mut conversation_index = 0usize;
        self.stable_slots.clear();
        self.chronology.clear();
        self.live_state.clear();
        self.block_metadata.clear();
        self.blocks.clear();
        self.next_event_sequence = CONTEXT_EVENT_SEQUENCE_STRIDE;
        for (index, block) in blocks.into_iter().enumerate() {
            match block.placement {
                ContextPlacement::StablePrefix => self
                    .stable_slots
                    .push(StableContextBlock::from_compatibility_block(block, index)?),
                ContextPlacement::ConversationAppend => {
                    let execution_group_id = compatibility_group_ids[conversation_index].clone();
                    let (semantic_kind, retention, recoverable_for_compaction) =
                        compatibility_event_contract(&block, execution_group_id.as_ref());
                    let provider_owner = provider_owner_for_block(&block);
                    let sequence = self.allocate_event_sequence()?;
                    self.chronology.push(ConversationEvent {
                        block,
                        semantic_kind,
                        retention,
                        sequence,
                        execution_group_id,
                        provider_owner,
                        recoverable_for_compaction,
                    });
                    conversation_index = conversation_index.saturating_add(1);
                }
                ContextPlacement::EphemeralTail => {
                    self.live_state.push(LiveStateBlock::new(block)?);
                }
            }
        }
        self.rebuild_projections();
        self.validate_stored_metadata()
    }

    /// Rebuilds adapter projections from typed canonical storage.
    fn rebuild_projections(&mut self) {
        self.blocks.clear();
        self.block_metadata.clear();
        self.blocks
            .reserve(self.stable_slots.len() + self.chronology.len() + self.live_state.len());
        self.block_metadata.reserve(self.blocks.capacity());
        for slot in &self.stable_slots {
            self.blocks.push(slot.block.clone());
            self.block_metadata.push(slot.metadata());
        }
        for event in &self.chronology {
            self.blocks.push(event.block.clone());
            self.block_metadata.push(event.metadata());
        }
        for live_state in &self.live_state {
            self.blocks.push(live_state.block.clone());
            self.block_metadata.push(live_state.metadata());
        }
    }

    /// Validates stored metadata alignment and strictly increasing chronology.
    fn validate_stored_metadata(&self) -> AgentContextResult<()> {
        let typed_len = self
            .stable_slots
            .len()
            .saturating_add(self.chronology.len())
            .saturating_add(self.live_state.len());
        if self.blocks.len() != self.block_metadata.len() || self.blocks.len() != typed_len {
            return Err(AgentContextError::new(format!(
                "context projection length mismatch: stable={} chronology={} live_state={} blocks={} metadata={}; mutate context through checked APIs",
                self.stable_slots.len(),
                self.chronology.len(),
                self.live_state.len(),
                self.blocks.len(),
                self.block_metadata.len()
            )));
        }
        let typed_projection = self
            .stable_slots
            .iter()
            .map(|slot| (&slot.block, slot.metadata()))
            .chain(
                self.chronology
                    .iter()
                    .map(|event| (&event.block, event.metadata())),
            )
            .chain(
                self.live_state
                    .iter()
                    .map(|live_state| (&live_state.block, live_state.metadata())),
            );
        for (index, ((expected_block, expected_metadata), (block, metadata))) in typed_projection
            .zip(self.blocks.iter().zip(&self.block_metadata))
            .enumerate()
        {
            if expected_block != block || expected_metadata != *metadata {
                return Err(AgentContextError::new(format!(
                    "context read projection drifted from typed storage at block index {index}"
                )));
            }
        }
        let mut last_sequence = 0u64;
        let mut stable_slot_ids = BTreeSet::new();
        let mut active_prompt_count = 0usize;
        let mut assistant_execution_groups = BTreeSet::new();
        for (index, (block, metadata)) in self.blocks.iter().zip(&self.block_metadata).enumerate() {
            validate_context_block_metadata(index, block, metadata)?;
            if let Some(slot_id) = metadata.stable_slot_id.as_ref()
                && !stable_slot_ids.insert(slot_id.as_str())
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "stable slot identities must be unique",
                ));
            }
            if block.source == ContextSourceKind::UserInstruction && block.label == "user prompt" {
                active_prompt_count = active_prompt_count.saturating_add(1);
                if active_prompt_count > 1 {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "durable context may contain only one active user prompt",
                    ));
                }
            }
            if metadata.semantic_kind == ContextSemanticKind::AssistantEvent {
                let Some(group_id) = metadata.execution_group_id.as_ref() else {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "assistant execution events require an execution-group identity",
                    ));
                };
                assistant_execution_groups.insert(group_id.clone());
            }
            if matches!(
                block.source,
                ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool
            ) && metadata.retention == ContextRetention::ExecutionGroup
                && !metadata
                    .execution_group_id
                    .as_ref()
                    .is_some_and(|group_id| assistant_execution_groups.contains(group_id))
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "action and native-tool evidence requires a preceding owning assistant execution",
                ));
            }
            if let Some(sequence) = metadata.event_sequence {
                if sequence.get() <= last_sequence {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "conversation event sequences must be strictly increasing",
                    ));
                }
                last_sequence = sequence.get();
            }
        }
        if last_sequence >= self.next_event_sequence {
            return Err(AgentContextError::new(
                "next context event sequence must exceed the committed high-water mark",
            ));
        }
        Ok(())
    }

    /// Validates the semantic and lifetime contract for stored turn context.
    pub fn validate_durable(&self) -> AgentContextResult<()> {
        self.validate_stored_metadata()?;
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
        let group = self
            .block_metadata
            .iter()
            .rev()
            .find_map(|metadata| metadata.execution_group_id.clone())
            .or_else(|| {
                results.first().and_then(|result| {
                    ContextExecutionGroupId::new(format!(
                        "legacy-action-results:{}",
                        result.turn_id
                    ))
                    .ok()
                })
            })
            .ok_or_else(|| AgentContextError::new("action result commit requires a group"))?;
        self.commit_settled_action_results_in_group(results, group)
    }

    /// Atomically promotes deterministic action results into one explicit
    /// assistant execution group in caller-supplied observation order.
    pub fn commit_settled_action_results_in_group(
        &mut self,
        results: &[ActionResult],
        execution_group_id: ContextExecutionGroupId,
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

        let mut candidate = self.clone();
        let mut committed = 0usize;
        for result in results {
            let label = format!("action result {}", result.action_id);
            let content = action_result_context_content(result);
            let exact_block = candidate
                .blocks
                .iter()
                .find(|block| {
                    block.source == ContextSourceKind::ActionResult
                        && block.placement == ContextPlacement::ConversationAppend
                        && block.label == label
                        && block.content == content
                })
                .cloned();
            candidate.retain_blocks(|block| {
                let same_action = block.source == ContextSourceKind::ActionResult
                    && action_result_block_id(block).is_some_and(|id| id == result.action_id);
                !same_action || exact_block.as_ref().is_some_and(|exact| exact == block)
            })?;
            if exact_block.is_some() {
                continue;
            }
            candidate.append_evidence_event(
                ContextSourceKind::ActionResult,
                label,
                content,
                execution_group_id.clone(),
                None,
                true,
            )?;
            committed = committed.saturating_add(1);
        }
        candidate.validate_stored_metadata()?;
        validate_context_placement_order(&candidate.blocks)?;
        validate_context_semantics(&candidate.blocks)?;
        *self = candidate;
        Ok(committed)
    }
}

/// Allocates ordered identities for a replacement history prefix.
fn history_prefix_replacement_sequences(
    count: usize,
    successor_sequence: Option<u64>,
) -> AgentContextResult<Vec<u64>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let count_u64 = u64::try_from(count)
        .map_err(|_| AgentContextError::new("imported history replacement is too large"))?;
    if let Some(successor) = successor_sequence {
        let divisor = count_u64
            .checked_add(1)
            .ok_or_else(|| AgentContextError::new("imported history replacement is too large"))?;
        let step = successor / divisor;
        if step == 0 {
            return Err(AgentContextError::new(
                "imported history replacement cannot preserve retained event identities",
            ));
        }
        return (1..=count_u64)
            .map(|index| {
                step.checked_mul(index).ok_or_else(|| {
                    AgentContextError::new("imported history replacement sequence exhausted")
                })
            })
            .collect();
    }
    (1..=count_u64)
        .map(|index| {
            CONTEXT_EVENT_SEQUENCE_STRIDE
                .checked_mul(index)
                .ok_or_else(|| {
                    AgentContextError::new("imported history replacement sequence exhausted")
                })
        })
        .collect()
}

/// Assigns explicit compatibility group identities while importing an ordered
/// legacy/test block vector.
fn compatibility_execution_group_ids(
    blocks: &[ContextBlock],
) -> AgentContextResult<Vec<Option<ContextExecutionGroupId>>> {
    let chronology = blocks
        .iter()
        .filter(|block| block.placement == ContextPlacement::ConversationAppend)
        .collect::<Vec<_>>();
    let mut group_ids = vec![None; chronology.len()];
    for range in compatibility_execution_group_ranges(&chronology) {
        if !chronology[range.clone()]
            .iter()
            .any(|block| block.source == ContextSourceKind::TranscriptAssistant)
        {
            continue;
        }
        let group_id = ContextExecutionGroupId::new(format!(
            "compat-execution-group-{}",
            range.start.saturating_add(1)
        ))?;
        for index in range {
            if chronology[index].retention() == ContextRetention::ExecutionGroup {
                group_ids[index] = Some(group_id.clone());
            }
        }
    }
    Ok(group_ids)
}

/// Chooses the explicit compatibility contract for one imported event.
///
/// Persisted transcripts carry authoritative role and sequence but older
/// records do not carry an execution-group identifier. Contiguous
/// assistant/tool/result records can be grouped without changing chronology.
/// Evidence separated from its possible assistant owner by an exact barrier is
/// retained as an exact neutral reference instead of inventing a causal link or
/// rejecting otherwise complete history.
fn compatibility_event_contract(
    block: &ContextBlock,
    execution_group_id: Option<&ContextExecutionGroupId>,
) -> (ContextSemanticKind, ContextRetention, bool) {
    if matches!(
        block.source,
        ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult
    ) && execution_group_id.is_none()
    {
        return (
            ContextSemanticKind::ReferenceEvent,
            ContextRetention::Exact,
            true,
        );
    }
    (
        block.semantic_kind(),
        block.retention(),
        block.recoverable_for_compaction(),
    )
}

/// Finds indivisible execution groups at the compatibility import boundary.
fn compatibility_execution_group_ranges(blocks: &[&ContextBlock]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut has_assistant = false;
    let mut has_native_tool = false;
    for (index, block) in blocks.iter().enumerate() {
        let protected = block.retention() == ContextRetention::Exact
            || (block.source == ContextSourceKind::Memory
                && block.label == "context compaction summary");
        let attaches_to_previous = match block.source {
            ContextSourceKind::TranscriptTool => has_assistant,
            ContextSourceKind::ActionResult => has_assistant || has_native_tool,
            _ => false,
        };
        let current_group_protected = blocks[start..index].iter().any(|candidate| {
            candidate.retention() == ContextRetention::Exact
                || (candidate.source == ContextSourceKind::Memory
                    && candidate.label == "context compaction summary")
        });
        if index > start && (protected || current_group_protected || !attaches_to_previous) {
            groups.push(start..index);
            start = index;
            has_assistant = false;
            has_native_tool = false;
        }
        has_assistant |= block.source == ContextSourceKind::TranscriptAssistant;
        has_native_tool |= block.source == ContextSourceKind::TranscriptTool;
    }
    if start < blocks.len() {
        groups.push(start..blocks.len());
    }
    groups
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
        let mut context = self.durable.clone();
        for block in &self.live_state {
            context
                .insert_typed_block(
                    block.clone(),
                    ContextSemanticKind::LiveState,
                    ContextRetention::RequestLocal,
                    false,
                )
                .expect("prepared live state was validated at construction");
        }
        context
    }

    /// Consumes the prepared view and joins its two already validated phases.
    pub fn into_agent_context(mut self) -> AgentContext {
        for block in self.live_state.drain(..) {
            self.durable
                .insert_typed_block(
                    block,
                    ContextSemanticKind::LiveState,
                    ContextRetention::RequestLocal,
                    false,
                )
                .expect("prepared live state was validated at construction");
        }
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

/// Returns the exclusive owner encoded by one provider continuity payload.
fn provider_owner_for_block(block: &ContextBlock) -> Option<ProviderContinuityOwner> {
    ProviderTranscriptEvent::from_transcript_content(&block.content)
        .map(|_| ProviderContinuityOwner::DeepSeek)
}

/// Validates producer-selected metadata against structural placement rules.
fn validate_context_block_metadata(
    index: usize,
    block: &ContextBlock,
    metadata: &ContextBlockMetadata,
) -> AgentContextResult<()> {
    let stable_slot_metadata_is_partial =
        metadata.stable_slot_id.is_some() != metadata.stable_source_fingerprint.is_some();
    let invalid_reason = match block.placement {
        ContextPlacement::StablePrefix
            if metadata.semantic_kind != ContextSemanticKind::AmbientInstruction
                || metadata.event_sequence.is_some()
                || metadata.retention == ContextRetention::RequestLocal
                || stable_slot_metadata_is_partial =>
        {
            Some(
                "stable blocks must be unsequenced ambient instructions with complete slot metadata",
            )
        }
        ContextPlacement::ConversationAppend
            if metadata.semantic_kind == ContextSemanticKind::LiveState
                || metadata.retention == ContextRetention::RequestLocal
                || metadata.event_sequence.is_none()
                || metadata.stable_slot_id.is_some()
                || metadata.stable_source_fingerprint.is_some() =>
        {
            Some("conversation events require a sequence and cannot own stable-slot metadata")
        }
        ContextPlacement::EphemeralTail
            if metadata.semantic_kind != ContextSemanticKind::LiveState
                || metadata.retention != ContextRetention::RequestLocal
                || metadata.event_sequence.is_some()
                || metadata.execution_group_id.is_some()
                || metadata.stable_slot_id.is_some()
                || metadata.stable_source_fingerprint.is_some() =>
        {
            Some(
                "live state must be unsequenced request-local context without stable-slot metadata",
            )
        }
        _ => None,
    };
    if let Some(reason) = invalid_reason {
        return Err(context_semantic_error(index, block, reason));
    }
    if metadata.provider_owner.is_some()
        && ProviderTranscriptEvent::from_transcript_content(&block.content).is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "provider ownership requires a typed provider continuity payload",
        ));
    }
    if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some()
        && metadata.provider_owner.is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "provider continuity payload requires an explicit owner",
        ));
    }
    if metadata.semantic_kind == ContextSemanticKind::UserEvent
        && (block.source != ContextSourceKind::UserInstruction
            && block.source != ContextSourceKind::TranscriptUser)
    {
        return Err(context_semantic_error(
            index,
            block,
            "only direct or transcript user sources may claim user-event semantics",
        ));
    }
    if metadata.retention == ContextRetention::ExecutionGroup
        && metadata.execution_group_id.is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "execution-group retention requires an execution-group identity",
        ));
    }
    Ok(())
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
        ContextBlock, ContextCachePolicy, ContextExecutionGroupId, ContextRetention,
        ContextSemanticKind, ContextSourceKind, ContextStability, PreparedModelContext,
        StableContextBlock, StableContextSlotId, StableContextSourceFingerprint,
        validate_context_required, validate_context_semantics,
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
            permission_evaluation: None,
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
            placement: crate::ContextPlacement::ConversationAppend,
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

    /// Verifies multi-action evidence and mid-turn steering retain their exact
    /// observation order in durable chronology.
    #[test]
    fn context_semantics_preserve_multi_action_and_mid_turn_steering_order() {
        let blocks = vec![
            ContextBlock::user_event("user prompt", "implement the change"),
            ContextBlock::assistant_event("assistant action 1", "inspect owner"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 1",
                "owner found",
            ),
            ContextBlock::assistant_event("assistant action 2", "edit owner"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 2",
                "edit applied",
            ),
            ContextBlock::user_event("user steering", "also update the specification"),
            ContextBlock::assistant_event("assistant action 3", "update specification"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 3",
                "specification updated",
            ),
        ];

        let context = AgentContext::new_durable(blocks).unwrap();
        assert_eq!(
            context
                .blocks
                .iter()
                .map(|block| block.label.as_str())
                .collect::<Vec<_>>(),
            [
                "user prompt",
                "assistant action 1",
                "result 1",
                "assistant action 2",
                "result 2",
                "user steering",
                "assistant action 3",
                "result 3",
            ]
        );
        assert_eq!(context.blocks[5].content, "also update the specification");
        assert_eq!(context.blocks[5].retention(), ContextRetention::Exact);
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

    /// Verifies the typed collections are the source of truth for their
    /// read-only provider projection and retain stable replacement identity.
    #[test]
    fn agent_context_projects_typed_stable_event_and_live_state_storage_in_order() {
        let mut durable = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "inspect chronology",
        )])
        .unwrap();
        durable
            .replace_stable_source_slots(
                ContextSourceKind::ProjectGuidance,
                vec![
                    StableContextBlock::new(
                        StableContextSlotId::new("project-guidance").unwrap(),
                        StableContextSourceFingerprint::new("a".repeat(64)).unwrap(),
                        ContextBlock::stable_instruction(
                            ContextSourceKind::ProjectGuidance,
                            "active repository instructions",
                            "preserve chronology",
                        ),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let group = ContextExecutionGroupId::new("execution-1").unwrap();
        durable
            .append_assistant_event("assistant action", "inspect files", group.clone())
            .unwrap();
        durable
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result inspect",
                "files inspected",
                group,
                None,
                true,
            )
            .unwrap();
        let prepared = PreparedModelContext::new(
            durable.clone(),
            vec![ContextBlock::live_state(
                ContextSourceKind::RuntimeHint,
                "runtime state",
                "cwd=/repo",
            )],
        )
        .unwrap();
        let projected = prepared.to_agent_context();

        assert_eq!(projected.stable_slots().len(), 1);
        assert_eq!(projected.chronology().len(), 3);
        assert_eq!(projected.live_state.len(), 1);
        assert_eq!(
            projected
                .blocks()
                .iter()
                .map(|block| block.label.as_str())
                .collect::<Vec<_>>(),
            [
                "active repository instructions",
                "user prompt",
                "assistant action",
                "action result inspect",
                "runtime state",
            ]
        );
        assert_eq!(prepared.durable(), &durable);
    }

    /// Verifies duplicate active prompts are rejected atomically instead of
    /// advancing the event high-water mark or leaving a partial event behind.
    #[test]
    fn agent_context_rejects_duplicate_active_prompt_atomically() {
        let mut context =
            AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "first task")])
                .unwrap();
        let original = context.clone();

        let error = context
            .append_user_event("user prompt", "second task")
            .unwrap_err();

        assert!(error.message().contains("only one active user prompt"));
        assert_eq!(context, original);
    }

    /// Verifies the compatibility insertion boundary is transactional even
    /// when its inferred event would violate a whole-context invariant.
    #[test]
    fn agent_context_rejects_invalid_compatibility_insertion_atomically() {
        let mut context =
            AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "first task")])
                .unwrap();
        let original = context.clone();

        let error = context
            .insert_typed_block(
                ContextBlock::user_event("user prompt", "second task"),
                ContextSemanticKind::UserEvent,
                ContextRetention::Exact,
                false,
            )
            .unwrap_err();

        assert!(error.message().contains("only one active user prompt"));
        assert_eq!(context, original);
    }

    /// Verifies evidence cannot commit without a preceding assistant execution
    /// in the same ownership group and that the failed append is atomic.
    #[test]
    fn agent_context_rejects_unowned_evidence_atomically() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "run the check",
        )])
        .unwrap();
        let original = context.clone();

        let error = context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result check",
                "passed",
                ContextExecutionGroupId::new("missing-assistant").unwrap(),
                None,
                true,
            )
            .unwrap_err();

        assert!(error.message().contains("preceding owning assistant"));
        assert_eq!(context, original);
    }

    /// Verifies predicate-based cleanup cannot remove an assistant owner while
    /// leaving its evidence behind or expose the invalid intermediate state.
    #[test]
    fn agent_context_rejects_causality_breaking_retention_atomically() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "run the check",
        )])
        .unwrap();
        let group = ContextExecutionGroupId::new("execution-1").unwrap();
        context
            .append_assistant_event("assistant action", "run check", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result check",
                "passed",
                group,
                None,
                true,
            )
            .unwrap();
        let original = context.clone();

        let error = context
            .retain_blocks(|block| block.source != ContextSourceKind::TranscriptAssistant)
            .unwrap_err();

        assert!(error.message().contains("preceding owning assistant"));
        assert_eq!(context, original);
    }

    /// Verifies legacy transcript import preserves a tool result that settles
    /// after steering without moving it before the user event or inventing an
    /// execution owner across the exact barrier.
    #[test]
    fn compatibility_import_keeps_straddling_evidence_as_exact_reference() {
        let context = AgentContext::new_durable(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "previous assistant action".to_string(),
                content: "started action-1".to_string(),
            },
            ContextBlock::user_event("user steering", "change the output format"),
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "late action result action-1".to_string(),
                content: "action-1 completed".to_string(),
            },
        ])
        .unwrap();

        assert_eq!(
            context
                .chronology()
                .iter()
                .map(|event| event.block().label.as_str())
                .collect::<Vec<_>>(),
            [
                "previous assistant action",
                "user steering",
                "late action result action-1"
            ]
        );
        let late_evidence = &context.chronology()[2];
        assert_eq!(
            late_evidence.semantic_kind(),
            ContextSemanticKind::ReferenceEvent
        );
        assert_eq!(late_evidence.retention(), ContextRetention::Exact);
        assert!(late_evidence.execution_group_id().is_none());
    }

    /// Verifies a history refresh can change the imported prefix cardinality
    /// without renumbering the active prompt or any later causal event.
    #[test]
    fn imported_history_prefix_replacement_preserves_retained_event_identities() {
        let mut context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("previous assistant", "older history"),
            ContextBlock::user_event("user prompt", "continue the task"),
        ])
        .unwrap();
        context
            .append_reference_event(
                ContextSourceKind::LocalMessage,
                "local message",
                "new constraint",
            )
            .unwrap();
        let group = ContextExecutionGroupId::new("current-execution").unwrap();
        context
            .append_assistant_event("current assistant", "run check", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result check",
                "passed",
                group,
                None,
                true,
            )
            .unwrap();
        let retained = context.chronology()[1..].to_vec();

        let replacement_count = context
            .replace_imported_history_prefix(
                |block| block.label == "previous assistant",
                vec![
                    ContextBlock::reference_event(
                        ContextSourceKind::Memory,
                        "conversation compaction notice",
                        "older history was compacted",
                    ),
                    ContextBlock::reference_event(
                        ContextSourceKind::Memory,
                        "memory compact-session",
                        "semantic summary",
                    ),
                ],
            )
            .unwrap();

        assert_eq!(replacement_count, 2);
        assert_eq!(&context.chronology()[2..], retained.as_slice());
        assert!(context.chronology()[1].sequence() < retained[0].sequence());
    }

    /// Verifies history refresh rejects fragmented ownership atomically instead
    /// of gathering records across a local-message barrier.
    #[test]
    fn imported_history_prefix_replacement_rejects_fragmented_ownership_atomically() {
        let mut context = AgentContext::new_durable(vec![
            ContextBlock::reference_event(ContextSourceKind::Memory, "old history 1", "one"),
            ContextBlock::reference_event(
                ContextSourceKind::LocalMessage,
                "local message",
                "barrier",
            ),
            ContextBlock::reference_event(ContextSourceKind::Memory, "old history 2", "two"),
            ContextBlock::user_event("user prompt", "continue"),
        ])
        .unwrap();
        let original = context.clone();

        let error = context
            .replace_imported_history_prefix(
                |block| block.source == ContextSourceKind::Memory,
                vec![ContextBlock::reference_event(
                    ContextSourceKind::Memory,
                    "replacement history",
                    "summary",
                )],
            )
            .unwrap_err();

        assert!(error.message().contains("contiguous chronology prefix"));
        assert_eq!(context, original);
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
            ContextBlock::assistant_event("assistant response action-1", "execute action-1"),
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
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
