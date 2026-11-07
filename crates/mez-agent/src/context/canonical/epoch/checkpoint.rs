//! Versioned, descriptive replay state at the explicit persistence boundary.
//!
//! This is a snapshot of the sole canonical chronology, not a second journal or
//! a durable-publication receipt. Storage must bind its bytes to a committed
//! manifest and validate integrity before adoption. Decoding alone grants no
//! authority. Restoration checks membership, causal metadata and accounting and
//! allocates a fresh owner incarnation. Current stable instructions and request
//! metadata come from the caller; the checkpoint contains only stable accounting.
//! Active task handles, process ownership, request-cache state and execution/peer
//! rights never cross this boundary. Version two preserves restrictive MCP
//! retrieval denials; original version-one imports derive the minimum denials
//! from checked member births. Exact instructions can be bound to fresh tasks;
//! pending flags preserve source protection, without reissuing effects or ACKs.
//! Version three also retains descriptive execution intervals. Live allocation
//! identities and controller-minted closure witnesses still never deserialize.

use super::super::{
    ContextBlockRecord, ContextSemanticKind, ProviderContinuityOwner, StableContextBlockRecord,
};
use super::*;
use serde::{Deserialize, Serialize};

/// Portable replay membership and source. Deserialization is structural only;
/// use [`ReplayEpochContext::from_checkpoint`] to validate and adopt it. Storage
/// owns payload integrity, publication CAS and durability, not this pure DTO.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayEpochCheckpoint<P = String> {
    version: u32,
    conversation: String,
    revision: u64,
    policy: ReplayHeadPolicy,
    head: Option<ContextBlockOccurrenceId>,
    members: Vec<Member>,
    events: Vec<Event<P>>,
    next_event_sequence: u64,
    stable_records: Vec<StableContextBlockRecord>,
    pending_execution_groups: Vec<String>,
    /// Version three original intervals, including closed retained groups.
    /// Older formats remain explicit unknown ancestry rather than guessed IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_intervals: Option<Vec<(String, ExecutionInterval)>>,
    pending_peer_messages: Vec<u64>,
    historical_instruction_events: Vec<u64>,
    /// Version one omitted this restrictive state. Its minimum denials are
    /// derived from actual member births when importing that original format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retired_mcp_retrievals: Option<Vec<ContextBlockOccurrenceId>>,
}

/// Ordered raw membership, including the birth revision of inherited tail.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    origin: u64,
    occurrence_id: ContextBlockOccurrenceId,
    born_in: u64,
}

/// Canonical event bytes and metadata, never a reconstructed label heuristic.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event<P> {
    record: ContextBlockRecord,
    block: Block<P>,
    semantic_kind: ContextSemanticKind,
    retention: ContextRetention,
    sequence: u64,
    execution_group_id: Option<String>,
    provider_owner: Option<ProviderOwner>,
    recoverable_for_compaction: bool,
}

/// Separates immutable semantic framing from inline or referenced payload bytes.
/// String payloads retain the original version-one JSON block representation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Block<P> {
    source: ContextSourceKind,
    placement: ContextPlacement,
    label: String,
    content: P,
}

impl<P> ReplayEpochCheckpoint<P> {
    /// Borrows descriptive producer-keyed stable accounting, never instructions.
    pub fn stable_records(&self) -> &[StableContextBlockRecord] {
        &self.stable_records
    }

    /// Selects an explicitly later checked stable snapshot at the persistence
    /// boundary. Actual live source still supplies all instruction bytes; empty
    /// records retire the earlier snapshot. Malformed/duplicate records refuse.
    pub fn with_stable_records(
        mut self,
        records: Vec<StableContextBlockRecord>,
    ) -> AgentContextResult<Self> {
        super::super::block_record::validated_stable_records(&records)?;
        self.stable_records = records;
        Ok(self)
    }

    /// Returns the descriptive append reservation carried by this snapshot.
    /// It grants no admission rights; checked restoration validates it against
    /// actual canonical chronology before the owner may allocate new events.
    pub fn next_event_sequence(&self) -> u64 {
        self.next_event_sequence
    }

    /// Transforms each event payload once, in canonical order, without changing
    /// membership, semantic framing, source IDs or lifecycle metadata. Storage
    /// can replace bytes with checked immutable references and resolve them
    /// before [`ReplayEpochContext::from_checkpoint`]. The callback's error
    /// aborts transformation; this operation itself performs no I/O or adoption.
    pub fn try_map_payloads<Q, E>(
        self,
        mut map: impl FnMut(ContextBlockOccurrenceId, P) -> Result<Q, E>,
    ) -> Result<ReplayEpochCheckpoint<Q>, E> {
        let events = self
            .events
            .into_iter()
            .map(|event| {
                Ok(Event {
                    block: Block {
                        source: event.block.source,
                        placement: event.block.placement,
                        label: event.block.label,
                        content: map(event.record.occurrence_id(), event.block.content)?,
                    },
                    record: event.record,
                    semantic_kind: event.semantic_kind,
                    retention: event.retention,
                    sequence: event.sequence,
                    execution_group_id: event.execution_group_id,
                    provider_owner: event.provider_owner,
                    recoverable_for_compaction: event.recoverable_for_compaction,
                })
            })
            .collect::<Result<_, E>>()?;
        Ok(ReplayEpochCheckpoint {
            events,
            version: self.version,
            conversation: self.conversation,
            revision: self.revision,
            policy: self.policy,
            head: self.head,
            members: self.members,
            next_event_sequence: self.next_event_sequence,
            stable_records: self.stable_records,
            pending_execution_groups: self.pending_execution_groups,
            execution_intervals: self.execution_intervals,
            pending_peer_messages: self.pending_peer_messages,
            historical_instruction_events: self.historical_instruction_events,
            retired_mcp_retrievals: self.retired_mcp_retrievals,
        })
    }
}

/// Exact API/provider identity. The legacy bit preserves an already-decoded
/// historical representation; it cannot introduce a new scalar provider kind.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderOwner {
    api: String,
    provider_id: String,
    legacy: bool,
}

impl ReplayEpochContext {
    /// Captures the current checked head, raw members, arrival high-water mark,
    /// lifecycle witnesses and original accounting. This performs no I/O and
    /// does not claim restart durability or ordinary transcript append success.
    pub fn checkpoint(&self) -> AgentContextResult<ReplayEpochCheckpoint> {
        self.validate_membership()?;
        Ok(ReplayEpochCheckpoint {
            version: 3,
            conversation: self.conversation.as_str().to_owned(),
            revision: self.revision.get(),
            policy: self.policy,
            head: self.head.as_ref().map(ReplaySummaryIdentity::occurrence_id),
            members: self
                .members
                .iter()
                .map(|member| Member {
                    origin: member.origin.get(),
                    occurrence_id: member.occurrence_id,
                    born_in: member.born_in.get(),
                })
                .collect(),
            events: self.context.chronology.iter().map(Event::capture).collect(),
            next_event_sequence: self.context.next_event_sequence,
            stable_records: self.context.stable_block_records(),
            pending_execution_groups: self
                .context
                .pending_execution_groups
                .keys()
                .map(|group| group.as_str().to_owned())
                .collect(),
            execution_intervals: Some(
                self.context
                    .execution_intervals
                    .iter()
                    .map(|(group, interval)| (group.as_str().into(), interval.clone()))
                    .collect(),
            ),
            pending_peer_messages: self
                .context
                .pending_peer_messages
                .iter()
                .map(|sequence| sequence.get())
                .collect(),
            historical_instruction_events: self
                .context
                .historical_instruction_events
                .iter()
                .map(|sequence| sequence.get())
                .collect(),
            retired_mcp_retrievals: Some(
                self.context
                    .retired_mcp_retrievals
                    .iter()
                    .copied()
                    .collect(),
            ),
        })
    }

    /// Restores a checked epoch for the externally selected conversation.
    /// `current_prefix` must contain only current trusted stable slots and the
    /// caller's request metadata; saved instructions or session/cache metadata
    /// are never adopted. Matching stable slots reuse original accounting after
    /// checking actual current bytes; removed slots remain absent.
    ///
    /// Unsupported formats, foreign conversations, malformed lifecycle/source,
    /// missing members, invalid heads or nonclosed settled native groups refuse
    /// the whole candidate. Stale costs repair from actual bytes. Restoration
    /// allocates a fresh incarnation and no task handles, so prior plans and
    /// closure handles cannot act on it. Pending work stays protected and is
    /// never executed. Storage must verify source integrity before calling.
    pub fn from_checkpoint(
        conversation: ReplayConversationId,
        checkpoint: ReplayEpochCheckpoint,
        mut current_prefix: AgentContext,
    ) -> AgentContextResult<Self> {
        if current_prefix.has_replay_owner() {
            return Err(invalid("checkpoint prefix already has a live owner"));
        }
        if !matches!(
            (
                checkpoint.version,
                checkpoint.retired_mcp_retrievals.is_some(),
                checkpoint.execution_intervals.is_some()
            ),
            (1, false, false) | (2, true, false) | (3, true, true)
        ) || checkpoint.conversation != conversation.as_str()
        {
            return Err(invalid("unsupported format or foreign conversation"));
        }
        current_prefix
            .validate_durable()
            .map_err(|_| invalid("invalid current stable prefix"))?;
        if !current_prefix.chronology.is_empty() {
            return Err(invalid("current prefix contains conversation source"));
        }
        let saved =
            super::super::block_record::validated_stable_records(&checkpoint.stable_records)?;
        for slot in &mut current_prefix.stable_slots {
            if let Some(record) = saved.get(slot.slot_id.as_str()) {
                // A prefix constructed with this same occurrence already owns
                // a checked estimate for its current bytes. Do not replace it
                // with stale saved costs only to recompute them a second time.
                if slot.record.occurrence_id() != record.occurrence_id() {
                    slot.record = record.restored(&slot.block)?;
                }
            }
        }
        current_prefix.chronology = checkpoint
            .events
            .into_iter()
            .map(Event::restore)
            .collect::<AgentContextResult<_>>()?;
        current_prefix.next_event_sequence = checkpoint.next_event_sequence;
        current_prefix.execution_intervals = if let Some(intervals) = checkpoint.execution_intervals
        {
            let mut restored = BTreeMap::new();
            for (group, interval) in intervals {
                interval.validate()?;
                if restored
                    .insert(ContextExecutionGroupId::new(group)?, interval)
                    .is_some()
                {
                    return Err(invalid("duplicate execution interval"));
                }
            }
            restored
        } else {
            current_prefix
                .chronology
                .iter()
                .filter_map(|event| event.execution_group_id().cloned())
                .map(|group| (group, ExecutionInterval::unknown()))
                .collect()
        };
        if current_prefix
            .chronology
            .iter()
            .filter(|event| !super::super::pending::observation_only_event(event))
            .filter_map(|event| event.execution_group_id())
            .any(|group| !current_prefix.execution_intervals.contains_key(group))
        {
            return Err(invalid("missing original execution interval"));
        }
        let pending = unique(
            checkpoint
                .pending_execution_groups
                .into_iter()
                .map(ContextExecutionGroupId::new),
        )?;
        current_prefix.pending_execution_groups = pending
            .into_iter()
            .map(|group| {
                let interval = current_prefix
                    .execution_intervals
                    .get(&group)
                    .ok_or_else(|| invalid("missing pending execution interval"))?
                    .clone();
                Ok((group, ContextExecutionProtection::new(interval)))
            })
            .collect::<AgentContextResult<_>>()?;
        current_prefix.pending_peer_messages = sequences(checkpoint.pending_peer_messages)?;
        current_prefix.historical_instruction_events =
            sequences(checkpoint.historical_instruction_events)?;
        // Ordinary projection rebuilds prune witnesses for deliberately removed
        // source. At an import boundary, such pruning would hide malformed
        // persisted state, so require the complete witness set to survive.
        let historical_count = current_prefix.historical_instruction_events.len();
        let interval_count = current_prefix.execution_intervals.len();
        current_prefix.rebuild_projections();
        if current_prefix.historical_instruction_events.len() != historical_count
            || current_prefix.execution_intervals.len() != interval_count
        {
            return Err(invalid(
                "historical instruction witness has no matching source",
            ));
        }
        let revision = ReplayEpochRevision(checkpoint.revision);
        let mut owner = Self {
            context: current_prefix,
            owner_identity: ReplayOwnerIdentity::new(),
            head: checkpoint.head.map(|occurrence_id| ReplaySummaryIdentity {
                conversation: conversation.clone(),
                revision,
                occurrence_id,
            }),
            conversation,
            revision,
            policy: checkpoint.policy,
            members: checkpoint
                .members
                .into_iter()
                .map(|member| {
                    Ok(ReplayEpochMember {
                        origin: ContextEventSequence::new(member.origin)?,
                        occurrence_id: member.occurrence_id,
                        born_in: ReplayEpochRevision(member.born_in),
                    })
                })
                .collect::<AgentContextResult<_>>()?,
            accounting: ReplayAccounting::default(),
            instruction_owners: BTreeMap::new(),
            next_instruction_owner: 1,
            source_lifecycle: ReplaySourceLifecycle::default(),
        };
        if owner.revision.get() == 0 && owner.head.is_some()
            || owner.revision.get() > 0
                && owner.head.is_none()
                && owner.policy == ReplayHeadPolicy::Summary
            || owner.head.is_some() && owner.policy == ReplayHeadPolicy::IntentionalOmission
        {
            return Err(invalid("head does not match revision and policy"));
        }
        let retired = checkpoint
            .retired_mcp_retrievals
            .unwrap_or_else(|| owner.older_mcp_retrievals().into_iter().collect());
        let unique = retired.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != retired.len() {
            return Err(invalid("duplicate MCP retrieval revocation"));
        }
        owner.context.retired_mcp_retrievals = unique;
        owner.bind_mcp_retrieval_scope();
        owner.initialize_accounting()?;
        owner.validate_membership()?;
        if let Some(head) = owner
            .context
            .chronology
            .first()
            .filter(|_| owner.head.is_some())
        {
            if head.semantic_kind != ContextSemanticKind::ReferenceEvent
                || head.retention != ContextRetention::Summarizable
                || head.execution_group_id.is_some()
                || head.provider_owner.is_some()
                || !head.recoverable_for_compaction
            {
                return Err(invalid("invalid summary metadata"));
            }
            crate::context_compaction::validate_model_compaction_summary(
                &head.block.content,
                usize::MAX,
                false,
            )
            .map_err(|_| invalid("unusable summary head"))?;
        }
        let groups = owner
            .context
            .chronology
            .iter()
            .filter_map(|event| event.execution_group_id())
            .collect::<BTreeSet<_>>();
        for group in groups {
            if !owner.context.execution_group_is_pending(group) {
                owner.validate_native_group_closure(group)?;
            }
        }
        Ok(owner)
    }
}

impl ReplayEpochCheckpoint {
    /// Copies checked replay into a distinct conversation scope without changing
    /// source bytes, occurrence records, costs, membership or birth revisions.
    /// The copy grants no task, process, execution, peer or publication rights.
    /// Retrieval evidence is explicitly revoked, including at revision zero;
    /// current trusted instructions and request metadata remain caller inputs.
    pub fn for_conversation_copy(self, target: ReplayConversationId) -> AgentContextResult<Self> {
        let stable = self.stable_records.clone();
        let original = ReplayConversationId::new(self.conversation.clone())?;
        let mut owner = ReplayEpochContext::from_checkpoint(original, self, AgentContext::empty())?;
        owner.revoke_retrieved_mcp_grants();
        owner.conversation = target.clone();
        if let Some(head) = owner.head.as_mut() {
            head.conversation = target;
        }
        owner.bind_mcp_retrieval_scope();
        owner.validate_membership()?;
        owner.checkpoint()?.with_stable_records(stable)
    }
}

impl Event<String> {
    /// Copies only immutable source metadata, excluding live process ownership.
    fn capture(event: &ConversationEvent) -> Self {
        Self {
            record: event.record,
            block: Block {
                source: event.block.source,
                placement: event.block.placement,
                label: event.block.label.clone(),
                content: event.block.content.clone(),
            },
            semantic_kind: event.semantic_kind,
            retention: event.retention,
            sequence: event.sequence.get(),
            execution_group_id: event
                .execution_group_id
                .as_ref()
                .map(|group| group.as_str().to_owned()),
            provider_owner: event.provider_owner.as_ref().map(|owner| ProviderOwner {
                api: owner.api().as_str().to_owned(),
                provider_id: owner.provider_id().to_owned(),
                legacy: owner.is_legacy(),
            }),
            recoverable_for_compaction: event.recoverable_for_compaction,
        }
    }

    /// Validates primitive identities and repairs estimates against actual bytes.
    /// Owner-wide semantic, lifecycle and membership checks follow reconstruction.
    fn restore(self) -> AgentContextResult<ConversationEvent> {
        if self.block.placement != ContextPlacement::ConversationAppend {
            return Err(invalid("event cannot contain a stable instruction"));
        }
        let block = ContextBlock {
            source: self.block.source,
            placement: self.block.placement,
            label: self.block.label,
            content: self.block.content,
        };
        Ok(ConversationEvent {
            record: self.record.restored(&block)?,
            block,
            semantic_kind: self.semantic_kind,
            retention: self.retention,
            sequence: ContextEventSequence::new(self.sequence)?,
            execution_group_id: self
                .execution_group_id
                .map(ContextExecutionGroupId::new)
                .transpose()?,
            provider_owner: self
                .provider_owner
                .map(ProviderOwner::restore)
                .transpose()?,
            recoverable_for_compaction: self.recoverable_for_compaction,
        })
    }
}

impl ProviderOwner {
    /// Preserves exact API ownership and refuses unknown or contradictory legacy
    /// representations before native source validation.
    fn restore(self) -> AgentContextResult<ProviderContinuityOwner> {
        let api = crate::ProviderApiCompatibility::from_id(&self.api)
            .ok_or_else(|| invalid("unknown provider API"))?;
        let owner = if self.legacy {
            ProviderContinuityOwner::from_legacy_provider_id(&self.provider_id)
        } else {
            ProviderContinuityOwner::new(api, self.provider_id)
        }
        .ok_or_else(|| invalid("invalid provider owner"))?;
        if owner.api() != api {
            return Err(invalid("contradictory legacy provider API"));
        }
        Ok(owner)
    }
}

/// Rejects duplicate lifecycle witnesses instead of silently dropping them.
fn unique<T: Ord>(
    items: impl IntoIterator<Item = AgentContextResult<T>>,
) -> AgentContextResult<BTreeSet<T>> {
    let mut unique = BTreeSet::new();
    for item in items {
        if !unique.insert(item?) {
            return Err(invalid("duplicate lifecycle witness"));
        }
    }
    Ok(unique)
}

/// Validates nonzero event positions before constructing a lifecycle set.
fn sequences(values: Vec<u64>) -> AgentContextResult<BTreeSet<ContextEventSequence>> {
    unique(values.into_iter().map(ContextEventSequence::new))
}

/// Keeps rejected source content and provider labels out of diagnostics.
fn invalid(reason: &str) -> AgentContextError {
    AgentContextError::new(format!("invalid replay epoch checkpoint: {reason}"))
}

impl<P> std::fmt::Debug for ReplayEpochCheckpoint<P> {
    /// Reports structural counts without source text or private provider IDs.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayEpochCheckpoint")
            .field("version", &self.version)
            .field("revision", &self.revision)
            .field("events", &self.events.len())
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}
