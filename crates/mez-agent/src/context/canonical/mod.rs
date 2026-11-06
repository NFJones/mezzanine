//! Provider-independent agent context validation contracts.
//!
//! This module owns typed stable slots, monotonic conversation events, and
//! deterministic validation failures for context and model-profile selection.
//! Typed collections are authoritative; ordered
//! blocks and metadata are read-only projections rebuilt after a checked
//! mutation. Mutations validate an isolated candidate before commit, direct
//! user events are exact, evidence requires a preceding causal owner, and all
//! model-visible state belongs to the stable prefix or durable chronology.
//! Product prompt assets,
//! transcript persistence, and provider execution remain outside this crate
//! and adapt these contracts at their composition boundaries.
//! Runtime-owned pending execution groups protect unresolved source through
//! compaction and staged adoption without entering transcript or model metadata.
//! Prefix and selective summaries carry explicit provenance through ordinary
//! block/message projection. Their reference semantics and visible bytes remain
//! unchanged; neither arbitrary memory UUIDs nor summary presence imply a new
//! compaction operation or a durable publication receipt.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::action_result::ActionResult;
use crate::{ProviderApiCompatibility, ProviderTranscriptEvent};

mod block_record;
pub use block_record::{ContextBlockOccurrenceId, ContextBlockRecord, StableContextBlockRecord};
mod chronology;
mod compaction;
mod epoch;
mod errors;
pub use epoch::*;
mod history;
mod imported_history;
mod messages;
mod pending;
use pending::ContextExecutionProtection;
mod projection;
mod request;
mod validation;
pub use errors::{
    AgentContextError, AgentContextResult, AgentRequestAssemblyError,
    AgentRequestAssemblyErrorKind, AgentRequestAssemblyResult, validate_context_required,
};
use history::{
    compatibility_event_contract, compatibility_execution_group_ids,
    history_prefix_replacement_sequences,
};
pub(crate) use messages::ProviderRequestEpoch;
pub use messages::{
    ContextEpochComponent, ContextEpochIdentity, ContextEpochTransition, ModelMessage,
    ModelMessageRole, ModelMessages, ModelMessagesIter,
};
pub use projection::{PreparedModelContext, model_context_block_header};
pub use request::ModelRequest;
mod summary;
pub use summary::{context_block_is_compaction_summary, model_message_is_compaction_summary};
pub use validation::{
    context_placement_insertion_index, insert_context_block_by_placement,
    validate_context_placement_order, validate_context_semantics,
};
use validation::{provider_owner_for_block, validate_context_block_metadata};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// A peer agent message delivered through the local message protocol.
    ///
    /// Peer messages are untrusted data. They carry reference-event semantics
    /// and summarizable retention, so direct user prompts and mid-turn steering
    /// always rank above them during compaction.
    PeerMessage,
    /// Runtime-generated controller guidance or state.
    RuntimeHint,
    /// An immutable snapshot of configured always-exposed MCP metadata.
    McpCatalogSnapshot,
    /// An exact explicit reference to one MCP server.
    McpServerReference,
    /// Exact safe directory records returned by an MCP server search.
    McpServerSearchResult,
    /// A complete MCP tool manifest retrieved from the live registry.
    McpRetrievedManifest,
    /// Repository or project guidance.
    ProjectGuidance,
    /// Retrieved durable memory context.
    Memory,
    /// Explicitly included user-owned persisted context document.
    PersistedContextDocument,
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
    /// Controller-owned summary of a conversation prefix. Labels and ordinary
    /// memory UUIDs never confer this provenance, nor is it a publication receipt.
    CompactionPrefix,
    /// Controller-owned summary replacing an anchored selective history range.
    CompactionSelective,
}

/// Trust domain assigned to one model-context block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDomain {
    /// User-provided instructions: user prompts, steering, and user-owned records.
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
            | ContextSourceKind::Configuration
            | ContextSourceKind::McpCatalogSnapshot
            | ContextSourceKind::McpServerReference
            | ContextSourceKind::McpServerSearchResult => Self::Configuration,
            ContextSourceKind::UserInstruction | ContextSourceKind::LocalMessage => Self::UserInput,
            // Peer mail is untrusted data written by another agent, so it is
            // never user instruction. `WebContent` is the closest existing
            // non-user variant: it marks the block untrusted by default and
            // reaches provider framing as `[untrusted:web-content]`, which is
            // accurate for text another agent chose to send.
            ContextSourceKind::PeerMessage => Self::WebContent,
            ContextSourceKind::SkillInstruction | ContextSourceKind::ProjectGuidance => {
                Self::ProjectFile
            }
            ContextSourceKind::RuntimeHint => Self::Configuration,
            ContextSourceKind::Memory
            | ContextSourceKind::CompactionPrefix
            | ContextSourceKind::CompactionSelective
            | ContextSourceKind::PersistedContextDocument
            | ContextSourceKind::TranscriptUser => Self::UserInput,
            ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::RoutedHandoff
            | ContextSourceKind::ActionResult
            | ContextSourceKind::McpRetrievedManifest => Self::ModelOutput,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPlacement {
    /// Invariant instructions and configuration that form the reusable prefix.
    StablePrefix,
    /// Immutable chronological conversation material appended after the prefix.
    ConversationAppend,
}

/// Model-facing meaning of one context block, independent of provider role.
///
/// Providers may need to wrap neutral context in a supported transport role,
/// but they must preserve this canonical meaning and cannot turn controller or
/// repository context into direct user authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
}

/// Retention and compaction treatment for one context block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRetention {
    /// Preserve the block byte-for-byte across request preparation and active
    /// turn compaction.
    Exact,
    /// Compact the block only with the closed execution group it belongs to.
    ExecutionGroup,
    /// The block may participate in chronological historical summarization.
    Summarizable,
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
    record: ContextBlockRecord,
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
        Self::with_record(slot_id, source_fingerprint, block, None)
    }

    /// Validates one new or restored slot against its actual live source.
    fn with_record(
        slot_id: StableContextSlotId,
        source_fingerprint: StableContextSourceFingerprint,
        block: ContextBlock,
        record: Option<ContextBlockRecord>,
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
            record: match record {
                Some(record) => record.restored(&block)?,
                None => ContextBlockRecord::allocate(&block)?,
            },
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

    /// Returns durable occurrence and its owned, non-model-visible accounting.
    pub fn record(&self) -> ContextBlockRecord {
        self.record
    }

    /// Returns the exact model-visible stable block.
    pub fn block(&self) -> &ContextBlock {
        &self.block
    }

    /// Builds a deterministic compatibility slot for an already ordered block
    /// vector at a legacy/test construction boundary.
    fn from_compatibility_block(block: ContextBlock, index: usize) -> AgentContextResult<Self> {
        Self::from_compatibility_block_with_record(block, index, None, &BTreeMap::new())
    }

    /// Restores an explicitly associated record at the compatibility boundary.
    fn from_compatibility_block_with_record(
        block: ContextBlock,
        index: usize,
        record: Option<ContextBlockRecord>,
        stable_records: &BTreeMap<&str, ContextBlockRecord>,
    ) -> AgentContextResult<Self> {
        let (slot_id, source_fingerprint) = Self::compatibility_identity(&block, index)?;
        let record =
            Self::select_import_record(record, stable_records.get(slot_id.as_str()).copied())?;
        Self::with_record(slot_id, source_fingerprint, block, record)
    }

    /// Computes the genuine old positional import key without allocating costs.
    fn compatibility_identity(
        block: &ContextBlock,
        index: usize,
    ) -> AgentContextResult<(StableContextSlotId, StableContextSourceFingerprint)> {
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
        let slot_id = StableContextSlotId::new(format!("compat-{identity_digest}"))?;
        Ok((
            slot_id,
            StableContextSourceFingerprint::new(source_fingerprint)?,
        ))
    }

    /// Computes only the old structural key when migration actually needs it.
    fn compatibility_slot_id(
        block: &ContextBlock,
        index: usize,
    ) -> AgentContextResult<StableContextSlotId> {
        let material = format!("{index}:{:?}:{}", block.source, block.label);
        let digest = Sha256::digest(material.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        StableContextSlotId::new(format!("compat-{digest}"))
    }

    /// Selects an actual producer key before construction, with an explicit
    /// same-position legacy import fallback. Saved fingerprints are not source.
    fn from_producer_block(
        block: ContextBlock,
        index: usize,
        slot_id: StableContextSlotId,
        record: Option<ContextBlockRecord>,
        stable_records: &BTreeMap<&str, ContextBlockRecord>,
    ) -> AgentContextResult<Self> {
        let saved = if let Some(record) = stable_records.get(slot_id.as_str()) {
            Some(*record)
        } else {
            let legacy = Self::compatibility_slot_id(&block, index)?;
            stable_records.get(legacy.as_str()).copied()
        };
        let record = Self::select_import_record(record, saved)?;
        let source = format!("{:?}", block.source);
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"mez-stable-producer-v1\0");
        for field in [
            slot_id.as_str(),
            source.as_str(),
            block.label.as_str(),
            block.content.as_str(),
        ] {
            fingerprint.update((field.len() as u64).to_le_bytes());
            fingerprint.update(field.as_bytes());
        }
        let fingerprint = fingerprint
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self::with_record(
            slot_id,
            StableContextSourceFingerprint::new(fingerprint)?,
            block,
            record,
        )
    }

    /// Refuses contradictory indexed/named claims before selecting an estimate.
    fn select_import_record(
        indexed: Option<ContextBlockRecord>,
        saved: Option<ContextBlockRecord>,
    ) -> AgentContextResult<Option<ContextBlockRecord>> {
        if let Some(saved) = saved {
            if indexed.is_some_and(|indexed| indexed != saved) {
                return Err(AgentContextError::new(
                    "conflicting indexed and named stable records",
                ));
            }
            Ok(Some(saved))
        } else {
            Ok(indexed)
        }
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
            record: self.record,
        }
    }
}

/// Opaque provider API and configured provider id that own native continuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuityOwner(ProviderContinuityOwnerRepr);

/// Private representation preserving decoder-only historical scalar owners.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderContinuityOwnerRepr {
    LegacyOpenAi,
    LegacyDeepSeek,
    Exact {
        api: ProviderApiCompatibility,
        provider_id: String,
    },
}

impl ProviderContinuityOwner {
    /// Builds one exact continuity owner for a configured provider and API.
    pub fn new(api: ProviderApiCompatibility, provider_id: impl Into<String>) -> Option<Self> {
        let provider_id = provider_id.into();
        if provider_id.is_empty()
            || provider_id.trim() != provider_id
            || provider_id.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self(ProviderContinuityOwnerRepr::Exact {
            api,
            provider_id,
        }))
    }

    /// Returns the exact configured provider identifier.
    pub fn provider_id(&self) -> &str {
        match &self.0 {
            ProviderContinuityOwnerRepr::LegacyOpenAi => "openai",
            ProviderContinuityOwnerRepr::LegacyDeepSeek => "deepseek",
            ProviderContinuityOwnerRepr::Exact { provider_id, .. } => provider_id,
        }
    }

    /// Returns the API compatibility owned by this continuity state.
    pub fn api(&self) -> ProviderApiCompatibility {
        match &self.0 {
            ProviderContinuityOwnerRepr::LegacyOpenAi => ProviderApiCompatibility::OpenAiResponses,
            ProviderContinuityOwnerRepr::LegacyDeepSeek => {
                ProviderApiCompatibility::DeepSeekChatCompletions
            }
            ProviderContinuityOwnerRepr::Exact { api, .. } => *api,
        }
    }

    /// Decodes one legacy durable provider identifier.
    pub(crate) fn from_legacy_provider_id(provider: &str) -> Option<Self> {
        match provider {
            "openai" => Some(Self(ProviderContinuityOwnerRepr::LegacyOpenAi)),
            "deepseek" => Some(Self(ProviderContinuityOwnerRepr::LegacyDeepSeek)),
            _ => None,
        }
    }

    /// Returns the historical scalar durable representation, when applicable.
    pub(crate) fn legacy_provider_id(&self) -> Option<&'static str> {
        match self.0 {
            ProviderContinuityOwnerRepr::LegacyOpenAi => Some("openai"),
            ProviderContinuityOwnerRepr::LegacyDeepSeek => Some("deepseek"),
            ProviderContinuityOwnerRepr::Exact { .. } => None,
        }
    }

    /// Reports whether this owner came from a historical scalar record.
    pub(crate) fn is_legacy(&self) -> bool {
        self.legacy_provider_id().is_some()
    }

    /// Returns whether both owner dimensions match the selected provider.
    pub fn matches_provider(&self, api: ProviderApiCompatibility, provider_id: &str) -> bool {
        self.api() == api && self.provider_id() == provider_id
    }

    /// Returns whether a typed provider event belongs to this owner's API family.
    pub fn accepts_transcript_event(&self, event: &ProviderTranscriptEvent) -> bool {
        matches!(
            (self.api(), event),
            (
                ProviderApiCompatibility::OpenAiResponses,
                ProviderTranscriptEvent::OpenAiResponseOutput { .. }
                    | ProviderTranscriptEvent::OpenAiFunctionCallOutput { .. }
            )
        ) || matches!(
            (self.api(), event),
            (
                ProviderApiCompatibility::OpenAiChatCompletions,
                ProviderTranscriptEvent::OpenAiChatCompletionsAssistantToolCall {
                    provider_id,
                    ..
                } | ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                    provider_id,
                    ..
                }
            ) if provider_id == self.provider_id()
        ) || matches!(
            (self.api(), event),
            (
                ProviderApiCompatibility::DeepSeekChatCompletions,
                ProviderTranscriptEvent::DeepSeekAssistantToolCall { .. }
                    | ProviderTranscriptEvent::DeepSeekToolResult { .. }
            )
        )
    }
}

/// Exact execution metadata decoded from one typed transcript record.
///
/// The model-visible block remains the matching [`ContextBlock`]. This value
/// restores causal ownership that is not rendered into provider input, so a
/// resumed conversation selects the same native or provider-neutral execution
/// projection and compacts the same atomic group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedExecutionEvent {
    record: Option<ContextBlockRecord>,
    block: ContextBlock,
    execution_group_id: ContextExecutionGroupId,
    ordinal: u64,
    provider_owner: Option<ProviderContinuityOwner>,
}

impl ImportedExecutionEvent {
    /// Creates one validated imported execution event.
    pub fn new(
        block: ContextBlock,
        execution_group_id: ContextExecutionGroupId,
        ordinal: u64,
        provider_owner: Option<ProviderContinuityOwner>,
    ) -> AgentContextResult<Self> {
        if block.placement != ContextPlacement::ConversationAppend
            || block.retention() != ContextRetention::ExecutionGroup
            || ordinal == 0
        {
            return Err(AgentContextError::new(
                "imported execution events require append-only execution-group blocks and a non-zero ordinal",
            ));
        }
        if provider_owner.is_some()
            && ProviderTranscriptEvent::from_transcript_content(&block.content).is_none()
        {
            return Err(AgentContextError::new(
                "imported provider ownership requires a typed provider continuity payload",
            ));
        }
        if let Some(owner) = provider_owner.as_ref()
            && ProviderTranscriptEvent::from_transcript_content(&block.content)
                .is_none_or(|event| !owner.accepts_transcript_event(&event))
        {
            return Err(AgentContextError::new(
                "imported provider ownership API must match the typed continuity payload family",
            ));
        }
        Ok(Self {
            record: None,
            block,
            execution_group_id,
            ordinal,
            provider_owner,
        })
    }

    /// Associates exact persistent origin rather than searching by equal text.
    /// Cost validation uses the real imported source without granting ownership.
    pub fn with_record(mut self, record: ContextBlockRecord) -> AgentContextResult<Self> {
        self.record = Some(record.restored(&self.block)?);
        Ok(self)
    }

    /// Returns the structurally associated imported occurrence, when present.
    /// This descriptive record grants no execution or provider authority.
    pub fn record(&self) -> Option<ContextBlockRecord> {
        self.record
    }

    /// Returns the exact model-visible block matched during import.
    pub fn block(&self) -> &ContextBlock {
        &self.block
    }

    /// Returns the original execution-group identity.
    pub fn execution_group_id(&self) -> &ContextExecutionGroupId {
        &self.execution_group_id
    }

    /// Returns the original ordinal within the execution group.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Returns the provider owner for opaque native continuity, when any.
    pub fn provider_owner(&self) -> Option<&ProviderContinuityOwner> {
        self.provider_owner.as_ref()
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
    record: ContextBlockRecord,
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
    pub fn provider_owner(&self) -> Option<&ProviderContinuityOwner> {
        self.provider_owner.as_ref()
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

    /// Returns a deterministic, unmeasured token estimate for this rendered block.
    ///
    /// Provider-reported usage covers whole requests, not individual blocks.
    /// This value is accounting-only and never enters provider messages.
    pub fn estimated_input_tokens(&self) -> usize {
        self.record.estimated_input_tokens()
    }

    /// Returns the cached content-only estimate for native provider projection.
    pub fn estimated_native_input_tokens(&self) -> usize {
        self.record.estimated_native_input_tokens()
    }

    /// Returns the immutable block origin and its versioned accounting snapshot.
    pub fn record(&self) -> ContextBlockRecord {
        self.record
    }
}

/// One chronological occurrence with immutable source and checked lifecycle.
///
/// Event identity, authorship, execution and provider ownership commit together.
/// Explicit task completion may change instruction retention to historical;
/// source bytes and origin remain unchanged. No parallel metadata mutation may
/// infer that transition from labels, newer text or a summary's presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationEvent {
    record: ContextBlockRecord,
    block: ContextBlock,
    semantic_kind: ContextSemanticKind,
    retention: ContextRetention,
    sequence: ContextEventSequence,
    execution_group_id: Option<ContextExecutionGroupId>,
    provider_owner: Option<ProviderContinuityOwner>,
    recoverable_for_compaction: bool,
}

/// One producer-classified chronological event awaiting atomic commitment.
///
/// Batched provider settlement uses this value to retain event order while
/// rebuilding the adapter projections only once for the complete batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConversationAppend {
    record: Option<ContextBlockRecord>,
    block: ContextBlock,
    semantic_kind: ContextSemanticKind,
    retention: ContextRetention,
    execution_group_id: Option<ContextExecutionGroupId>,
    provider_owner: Option<ProviderContinuityOwner>,
    recoverable_for_compaction: bool,
}

impl ContextConversationAppend {
    /// Builds an untrusted peer reference; it grants no delivery or ACK rights.
    pub fn peer_message(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            record: None,
            block: ContextBlock::reference_event(ContextSourceKind::PeerMessage, label, content),
            semantic_kind: ContextSemanticKind::ReferenceEvent,
            retention: ContextRetention::Summarizable,
            execution_group_id: None,
            provider_owner: None,
            recoverable_for_compaction: true,
        }
    }

    /// Builds a neutral summarizable runtime fact without task or input rights.
    pub fn runtime_fact(label: impl Into<String>, content: impl Into<String>) -> Self {
        let mut event = Self::peer_message(label, content);
        event.block.source = ContextSourceKind::RuntimeHint;
        event
    }

    /// Restores accounting at the explicit source boundary. The record changes
    /// neither source bytes nor causal/retention ownership; append still checks
    /// canonical semantics and duplicate occurrences atomically.
    pub fn with_record(mut self, record: ContextBlockRecord) -> AgentContextResult<Self> {
        self.record = Some(record.restored(&self.block)?);
        Ok(self)
    }

    /// Builds one assistant event owned by an execution group.
    pub fn assistant(
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
    ) -> Self {
        Self {
            record: None,
            block: ContextBlock::assistant_event(label, content),
            semantic_kind: ContextSemanticKind::AssistantEvent,
            retention: ContextRetention::ExecutionGroup,
            execution_group_id: Some(execution_group_id),
            provider_owner: None,
            recoverable_for_compaction: true,
        }
    }

    /// Builds a provider-native assistant occurrence with explicit continuity
    /// ownership. Canonical append validates the typed payload and API family;
    /// invalid or mismatched material fails atomically at that boundary.
    pub fn provider_assistant(
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
        provider_owner: ProviderContinuityOwner,
    ) -> Self {
        Self {
            record: None,
            block: ContextBlock::assistant_event(label, content),
            semantic_kind: ContextSemanticKind::AssistantEvent,
            retention: ContextRetention::ExecutionGroup,
            execution_group_id: Some(execution_group_id),
            provider_owner: Some(provider_owner),
            recoverable_for_compaction: true,
        }
    }

    /// Builds one execution-group evidence event.
    pub fn evidence(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
        provider_owner: Option<ProviderContinuityOwner>,
        recoverable_for_compaction: bool,
    ) -> Self {
        Self {
            record: None,
            block: ContextBlock::evidence_event(source, label, content),
            semantic_kind: ContextSemanticKind::EvidenceEvent,
            retention: ContextRetention::ExecutionGroup,
            execution_group_id: Some(execution_group_id),
            provider_owner,
            recoverable_for_compaction,
        }
    }
}

impl ConversationEvent {
    /// Returns the durable occurrence and its owned accounting, independently
    /// of the local ordering sequence or current epoch membership.
    pub fn record(&self) -> ContextBlockRecord {
        self.record
    }

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
    pub fn provider_owner(&self) -> Option<&ProviderContinuityOwner> {
        self.provider_owner.as_ref()
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
            provider_owner: self.provider_owner.clone(),
            recoverable_for_compaction: self.recoverable_for_compaction,
            stable_slot_id: None,
            stable_source_fingerprint: None,
            record: self.record,
        }
    }
}

/// One ordered unit of model-visible context.
/// Serde decoding is structural only; canonical import or checkpoint adoption
/// must validate provenance, placement, lifecycle and accounting before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// Returns the block's derived trust domain.
    pub fn trust_domain(&self) -> TrustDomain {
        TrustDomain::for_source(self.source)
    }

    /// Returns the compatibility stability class for this block's placement.
    pub fn stability(&self) -> ContextStability {
        match self.placement {
            ContextPlacement::StablePrefix => ContextStability::Static,
            ContextPlacement::ConversationAppend => ContextStability::SessionStable,
        }
    }

    /// Returns the provider-cache policy for this block.
    pub fn cache_policy(&self) -> ContextCachePolicy {
        match self.placement {
            ContextPlacement::StablePrefix | ContextPlacement::ConversationAppend => {
                ContextCachePolicy::Eligible
            }
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
            | ContextSourceKind::ActionResult
            | ContextSourceKind::McpRetrievedManifest => ContextSemanticKind::EvidenceEvent,
            ContextSourceKind::SkillInstruction => ContextSemanticKind::TaskPrelude,
            ContextSourceKind::LocalMessage
            | ContextSourceKind::PeerMessage
            | ContextSourceKind::Memory
            | ContextSourceKind::CompactionPrefix
            | ContextSourceKind::CompactionSelective
            | ContextSourceKind::Transcript
            | ContextSourceKind::RoutedHandoff
            | ContextSourceKind::McpCatalogSnapshot
            | ContextSourceKind::McpServerReference
            | ContextSourceKind::McpServerSearchResult => ContextSemanticKind::ReferenceEvent,
            ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::ProjectGuidance
            | ContextSourceKind::PersistedContextDocument => {
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
                ContextPlacement::ConversationAppend => ContextSemanticKind::ReferenceEvent,
            },
        }
    }

    /// Returns the canonical retention treatment of this block.
    pub fn retention(&self) -> ContextRetention {
        match self.source {
            ContextSourceKind::UserInstruction
            | ContextSourceKind::SkillInstruction
            | ContextSourceKind::LocalMessage
            | ContextSourceKind::System
            | ContextSourceKind::DeveloperInstruction
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration
            | ContextSourceKind::ProjectGuidance
            | ContextSourceKind::PersistedContextDocument
            | ContextSourceKind::RuntimeHint
            | ContextSourceKind::RoutedHandoff
            | ContextSourceKind::McpCatalogSnapshot
            | ContextSourceKind::McpServerReference
            | ContextSourceKind::McpServerSearchResult => ContextRetention::Exact,
            ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult
            | ContextSourceKind::McpRetrievedManifest => ContextRetention::ExecutionGroup,
            ContextSourceKind::Memory
            | ContextSourceKind::CompactionPrefix
            | ContextSourceKind::CompactionSelective
            | ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser
            | ContextSourceKind::PeerMessage => ContextRetention::Summarizable,
        }
    }

    /// Returns whether exact content can be recovered outside model context.
    pub fn recoverable_for_compaction(&self) -> bool {
        matches!(
            self.source,
            ContextSourceKind::Memory
                | ContextSourceKind::CompactionPrefix
                | ContextSourceKind::CompactionSelective
                | ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
                | ContextSourceKind::CommittedEvidence
                | ContextSourceKind::RoutedHandoff
                | ContextSourceKind::RuntimeHint
                | ContextSourceKind::ActionResult
                | ContextSourceKind::McpRetrievedManifest
                | ContextSourceKind::LocalMessage
                | ContextSourceKind::PeerMessage
                | ContextSourceKind::McpCatalogSnapshot
                | ContextSourceKind::McpServerReference
                | ContextSourceKind::McpServerSearchResult
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
    /// Read-only ordered projection consumed by existing provider adapters.
    blocks: Vec<ContextBlock>,
    /// Read-only metadata projection aligned one-to-one with `blocks`.
    block_metadata: Vec<ContextBlockMetadata>,
    /// Next sequence reserved for a future committed conversation event.
    next_event_sequence: u64,
    /// Typed request metadata excluded from model-message projection.
    metadata: ModelContextMetadata,
    /// Unresolved source protection, never projected. An explicit epoch
    /// checkpoint may preserve this descriptive state without execution rights.
    pending_execution_groups: BTreeMap<ContextExecutionGroupId, ContextExecutionProtection>,
    /// Exact committed peer references awaiting local request handoff. Runtime
    /// protection only; checkpointing it grants no delivery or ACK rights.
    pending_peer_messages: BTreeSet<ContextEventSequence>,
    /// Explicitly completed instruction occurrences. Their bytes and authorship
    /// remain immutable while their retention becomes historical. This typed
    /// lifecycle is not reconstructed from labels or bare block imports.
    historical_instruction_events: BTreeSet<ContextEventSequence>,
    /// Restrictive tool-availability metadata for retained retrieval evidence.
    /// Typed checkpoints preserve this set; source bytes and records do not
    /// change, and an old occurrence cannot regain its grant through replay.
    retired_mcp_retrievals: BTreeSet<ContextBlockOccurrenceId>,
    /// Actual live replay incarnation and source seals, never serialized/projected.
    replay_continuation: Option<Box<epoch::ReplayEpochContinuation>>,
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
            blocks: Vec::new(),
            block_metadata: Vec::new(),
            next_event_sequence: CONTEXT_EVENT_SEQUENCE_STRIDE,
            metadata: ModelContextMetadata::default(),
            pending_execution_groups: BTreeMap::new(),
            pending_peer_messages: BTreeSet::new(),
            historical_instruction_events: BTreeSet::new(),
            retired_mcp_retrievals: BTreeSet::new(),
            replay_continuation: None,
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

    /// Restores typed execution ownership decoded from durable transcript rows.
    ///
    /// Records are matched in transcript order against exact block identity.
    /// The operation fails atomically on missing, reordered, duplicated, or
    /// non-monotonic group metadata instead of falling back to inferred
    /// ownership that could change provider projection or compaction behavior.
    pub fn restore_imported_execution_events(
        &mut self,
        imported: &[ImportedExecutionEvent],
    ) -> AgentContextResult<()> {
        if imported.is_empty() {
            return Ok(());
        }
        let mut candidate = self.clone();
        candidate.restore_imported_metadata(imported, 0, BTreeMap::new())?;
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        *self = candidate;
        Ok(())
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

    /// Inserts task-scoped chronological preludes immediately before the one
    /// active direct-user prompt during initial request construction.
    ///
    /// The supplied blocks remain durable conversation events. Existing
    /// historical events retain their sequence and order; sparse sequence
    /// values are assigned only in the interval before the not-yet-sent active
    /// prompt. Callers must use this construction boundary before provider
    /// dispatch, never to rewrite a sent request chain.
    pub fn insert_task_preludes_before_active_user(
        &mut self,
        preludes: Vec<ContextBlock>,
    ) -> AgentContextResult<()> {
        if preludes.is_empty() {
            return Ok(());
        }
        if preludes.iter().any(|block| {
            block.placement != ContextPlacement::ConversationAppend
                || block.semantic_kind() != ContextSemanticKind::TaskPrelude
                || block.retention() != ContextRetention::Exact
        }) {
            return Err(AgentContextError::new(
                "prompt-boundary insertion requires exact chronological task preludes",
            ));
        }
        let mut candidate = self.clone();
        let active_users = candidate
            .chronology
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.block.source == ContextSourceKind::UserInstruction
                    && event.semantic_kind == ContextSemanticKind::UserEvent
                    && event.retention == ContextRetention::Exact
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if active_users.len() > 1 {
            return Err(AgentContextError::new(
                "prompt-boundary insertion accepts at most one active user prompt",
            ));
        }
        let Some(active_user_index) = active_users.first().copied() else {
            for block in preludes {
                candidate.append_conversation_event(
                    block,
                    ContextSemanticKind::TaskPrelude,
                    ContextRetention::Exact,
                    None,
                    None,
                    true,
                )?;
            }
            *self = candidate;
            return Ok(());
        };
        let previous_sequence = active_user_index
            .checked_sub(1)
            .and_then(|index| candidate.chronology.get(index))
            .map_or(0, |event| event.sequence.get());
        let active_user_sequence = candidate.chronology[active_user_index].sequence.get();
        let required_gaps = u64::try_from(preludes.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| AgentContextError::new("prompt-boundary prelude count is too large"))?;
        let available_gap = active_user_sequence.saturating_sub(previous_sequence);
        if available_gap <= required_gaps {
            return Err(AgentContextError::new(
                "prompt-boundary prelude insertion exhausted the sequence interval before the active user prompt",
            ));
        }
        let step = available_gap / required_gaps;
        let events = preludes
            .into_iter()
            .enumerate()
            .map(|(index, block)| {
                let offset = u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(1))
                    .and_then(|index| index.checked_mul(step))
                    .ok_or_else(|| {
                        AgentContextError::new("prompt-boundary prelude sequence overflow")
                    })?;
                Ok(ConversationEvent {
                    record: ContextBlockRecord::allocate(&block)?,
                    block,
                    semantic_kind: ContextSemanticKind::TaskPrelude,
                    retention: ContextRetention::Exact,
                    sequence: ContextEventSequence::new(
                        previous_sequence.checked_add(offset).ok_or_else(|| {
                            AgentContextError::new("prompt-boundary prelude sequence overflow")
                        })?,
                    )?,
                    execution_group_id: None,
                    provider_owner: None,
                    recoverable_for_compaction: true,
                })
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        if let Some(continuation) = &mut candidate.replay_continuation {
            continuation.admit_preludes(&events);
        }
        candidate
            .chronology
            .splice(active_user_index..active_user_index, events);
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        *self = candidate;
        Ok(())
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

    /// Archives an active prompt as prior user-authored transcript while
    /// preserving its chronological identity.
    ///
    /// Interrupted-turn continuation uses this before appending the correcting
    /// prompt. The prior prompt remains a user-role event, but no longer owns
    /// the unique active `UserInstruction` slot for the resumed context. Routed
    /// worker contexts may contain no active prompt because their task is a
    /// controller-authored reference event; that case is an exact no-op.
    pub fn archive_active_user_prompt(&mut self) -> AgentContextResult<bool> {
        let mut candidate = self.clone();
        let matching = candidate
            .chronology
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.block.source == ContextSourceKind::UserInstruction
                    && event.block.label == "user prompt"
                    && event.semantic_kind == ContextSemanticKind::UserEvent
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Ok(false);
        }
        if matching.len() > 1 {
            return Err(AgentContextError::new(format!(
                "active prompt archival accepts at most one match; found {}",
                matching.len()
            )));
        }
        let event = &mut candidate.chronology[matching[0]];
        event.block.source = ContextSourceKind::TranscriptUser;
        event.block.label = "interrupted user prompt".to_string();
        event.record = event.record.revised(&event.block)?;
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        *self = candidate;
        Ok(true)
    }

    /// Replaces the complete reusable prefix without changing chronology.
    ///
    /// Turn continuation uses the freshly assembled prefix so repository,
    /// preference, and persisted-document authority can refresh while the
    /// interrupted conversation events remain byte-for-byte intact.
    pub fn replace_stable_slots(
        &mut self,
        mut slots: Vec<StableContextBlock>,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.preserve_stable_record_origins(&mut slots)?;
        candidate.stable_slots = slots;
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        validate_context_placement_order(&candidate.blocks)?;
        candidate.validate_owned_semantics()?;
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
        event.record = event.record.revised(&event.block)?;
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
        self.retain_blocks_with_metadata(|block, _| keep(block))
    }

    /// Retains blocks using their captured causal metadata, without rebuilding
    /// ownership from visible text or source labels. Surviving slots and events
    /// retain their identities and order. An invalid causal projection returns
    /// an error and leaves the original context unchanged.
    pub fn retain_blocks_with_metadata(
        &mut self,
        mut keep: impl FnMut(&ContextBlock, &ContextBlockMetadata) -> bool,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate
            .stable_slots
            .retain(|slot| keep(&slot.block, &slot.metadata()));
        candidate
            .chronology
            .retain(|event| keep(&event.block, &event.metadata()));
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        validate_context_placement_order(&candidate.blocks)?;
        candidate.validate_owned_semantics()?;
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
        mut slots: Vec<StableContextBlock>,
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

        self.preserve_stable_record_origins(&mut slots)?;
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
        self.validate_owned_semantics()?;
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
    /// This compatibility operation is intended for stable and task-prelude
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
                    record: ContextBlockRecord::allocate(&block)?,
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
        };
        self.rebuild_projections();
        self.validate_stored_metadata()?;
        Ok(sequence)
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
        self.replace_imported_history_prefix_events(|event| owns(event.block()), blocks, &[])
    }

    /// Replaces all imported historical events through a stable sequence boundary.
    ///
    /// The active turn records this boundary when transcript history is first
    /// assembled. Unlike an event count, it remains valid when compaction
    /// replaces several historical events with one summary while preserving
    /// later prompt and same-turn chronology.
    pub fn replace_imported_history_prefix_through_sequence(
        &mut self,
        sequence_high_water: u64,
        blocks: Vec<ContextBlock>,
    ) -> AgentContextResult<usize> {
        self.replace_imported_history_prefix_events(
            |event| event.sequence().get() <= sequence_high_water,
            blocks,
            &[],
        )
    }

    /// Replaces a prefix selected from immutable chronology event identities.
    fn replace_imported_history_prefix_events(
        &mut self,
        mut owns: impl FnMut(&ConversationEvent) -> bool,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
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
            .filter(|(_, event)| owns(event))
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
            imported.initialize_typed_storage_with_records(blocks, records, &[], &[])?;
        }
        if !imported.stable_slots.is_empty() {
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
        self.initialize_typed_storage_with_records(blocks, &[], &[], &[])
    }

    /// Imports checked occurrence records directly, avoiding an initial recount
    /// that would immediately be discarded when valid cached data is restored.
    fn initialize_typed_storage_with_records(
        &mut self,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        stable_records: &[StableContextBlockRecord],
        stable_sources: &[(usize, StableContextSlotId)],
    ) -> AgentContextResult<()> {
        validate_context_placement_order(&blocks)?;
        validate_context_semantics(&blocks)?;
        let compatibility_group_ids = compatibility_execution_group_ids(&blocks)?;
        let stable_records = block_record::validated_stable_records(stable_records)?;
        let stable_sources = block_record::validate_stable_sources(&blocks, stable_sources)?;
        let mut conversation_index = 0usize;
        self.stable_slots.clear();
        self.chronology.clear();
        self.block_metadata.clear();
        self.blocks.clear();
        self.next_event_sequence = CONTEXT_EVENT_SEQUENCE_STRIDE;
        for (index, block) in blocks.into_iter().enumerate() {
            let record = records
                .binary_search_by_key(&index, |(index, _)| *index)
                .ok()
                .map(|found| records[found].1);
            match block.placement {
                ContextPlacement::StablePrefix => {
                    let slot = if let Some(key) = stable_sources.get(&index) {
                        StableContextBlock::from_producer_block(
                            block,
                            index,
                            key.clone(),
                            record,
                            &stable_records,
                        )?
                    } else {
                        StableContextBlock::from_compatibility_block_with_record(
                            block,
                            index,
                            record,
                            &stable_records,
                        )?
                    };
                    self.stable_slots.push(slot);
                }
                ContextPlacement::ConversationAppend => {
                    let execution_group_id = compatibility_group_ids[conversation_index].clone();
                    let (semantic_kind, retention, recoverable_for_compaction) =
                        compatibility_event_contract(&block, execution_group_id.as_ref());
                    let provider_owner = provider_owner_for_block(&block);
                    let sequence = self.allocate_event_sequence()?;
                    self.chronology.push(ConversationEvent {
                        record: match record {
                            Some(record) => record.restored(&block)?,
                            None => ContextBlockRecord::allocate(&block)?,
                        },
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
            }
        }
        self.rebuild_projections();
        self.validate_stored_metadata()
    }

    /// Rebuilds adapter projections from typed canonical storage.
    fn rebuild_projections(&mut self) {
        self.prune_retired_mcp_retrievals();
        self.historical_instruction_events.retain(|sequence| {
            self.chronology.iter().any(|event| {
                event.sequence == *sequence
                    && epoch::instruction_source_can_complete(&event.block)
                    && event.retention == ContextRetention::Summarizable
            })
        });
        self.blocks.clear();
        self.block_metadata.clear();
        self.blocks
            .reserve(self.stable_slots.len() + self.chronology.len());
        self.block_metadata.reserve(self.blocks.capacity());
        for slot in &self.stable_slots {
            self.blocks.push(slot.block.clone());
            self.block_metadata.push(slot.metadata());
        }
        for event in &self.chronology {
            self.blocks.push(event.block.clone());
            self.block_metadata.push(event.metadata());
        }
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
            let content = crate::action_result_context_content(result);
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
        candidate.validate_owned_semantics()?;
        *self = candidate;
        Ok(committed)
    }
}

/// Extracts the action id from canonical and legacy result-block labels.
fn action_result_block_id(block: &ContextBlock) -> Option<&str> {
    block
        .label
        .strip_prefix("action result ")
        .or_else(|| block.label.strip_prefix("action failure "))
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

#[cfg(test)]
mod tests;
