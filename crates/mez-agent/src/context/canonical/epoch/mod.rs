//! Single-head replay epochs on the existing canonical context store.
//!
//! This owner moves an AgentContext rather than maintaining a second model
//! history. Typed head identity and raw membership are controller metadata;
//! provider projection still reads the same checked canonical context. A head's
//! chronological anchor is only an ordering slot, never a covered raw origin.
//! Active instruction handles, pending groups and peer receipts protect exact
//! source. Only explicit task retirement makes instructions historical. Frozen
//! plans retain their parent and exact source; publication validates an isolated
//! candidate and adopts one head plus retained tail and post-frontier arrivals.
//!
//! This pure foundation performs no I/O, provider dispatch, billing or action
//! replay. Product cutover, durable epoch formats and user-selectable summary-free
//! policy belong to their adapters. Bare block reimport cannot reconstruct task
//! completion or replay membership; those require a typed durable witness.

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextConversationAppend,
    ContextEventSequence, ContextExecutionGroupId, ContextPlacement, ContextRetention,
    ContextSourceKind, ConversationEvent, StableContextBlock, context_block_is_compaction_summary,
};
use std::collections::{BTreeMap, BTreeSet};

mod types;
pub use types::*;
mod lifecycle;
mod native_groups;
mod planning;
mod publication;
#[cfg(test)]
mod tests;

/// One head plus explicit raw members for a conversation's committed replay.
/// Context bytes have one mutable owner; metadata never grants execution rights.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplayEpochContext {
    context: AgentContext,
    owner_identity: ReplayOwnerIdentity,
    conversation: ReplayConversationId,
    revision: ReplayEpochRevision,
    policy: ReplayHeadPolicy,
    head: Option<ReplaySummaryIdentity>,
    members: Vec<ReplayEpochMember>,
    instruction_owners: BTreeMap<u64, BTreeSet<ContextEventSequence>>,
    next_instruction_owner: u64,
}

impl ReplayEpochContext {
    /// Moves initial unsummarized canonical context into a summary-policy owner.
    /// Existing summary provenance rejects: labels cannot restore a replay head.
    /// Unowned exact instructions remain protected until explicitly bound.
    pub fn new(
        conversation: ReplayConversationId,
        context: AgentContext,
    ) -> AgentContextResult<Self> {
        Self::new_for_head_policy(conversation, context, ReplayHeadPolicy::Summary)
    }

    /// Creates the pure publication contract for an explicitly selected policy.
    /// This does not install a product mode, run a model, or infer omission from
    /// empty text. Product configuration/format selection remains adapter work.
    pub fn new_for_head_policy(
        conversation: ReplayConversationId,
        mut context: AgentContext,
        policy: ReplayHeadPolicy,
    ) -> AgentContextResult<Self> {
        context.validate_durable().map_err(|_| {
            AgentContextError::new("initial replay source failed canonical validation")
        })?;
        if context
            .chronology()
            .iter()
            .any(|event| context_block_is_compaction_summary(event.block()))
        {
            return Err(AgentContextError::new(
                "initial replay owner requires unsummarized source",
            ));
        }
        let groups = context
            .chronology()
            .iter()
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        for group in groups {
            context.retain_pending_execution_group(&group)?;
        }
        let peers = context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::PeerMessage)
            .map(ConversationEvent::sequence)
            .collect::<Vec<_>>();
        for sequence in peers {
            context.retain_pending_peer_message(sequence)?;
        }
        let revision = ReplayEpochRevision(0);
        let members = context
            .chronology()
            .iter()
            .map(|event| ReplayEpochMember {
                origin: event.sequence(),
                born_in: revision,
            })
            .collect();
        let owner = Self {
            context,
            owner_identity: ReplayOwnerIdentity::new(),
            conversation,
            revision,
            policy,
            head: None,
            members,
            instruction_owners: BTreeMap::new(),
            next_instruction_owner: 1,
        };
        owner.validate_membership()?;
        Ok(owner)
    }

    /// Returns the sole canonical provider-facing context, without a mutable escape.
    pub fn context(&self) -> &AgentContext {
        &self.context
    }
    /// Returns the committed replay revision, independent of attempt generations.
    pub fn revision(&self) -> ReplayEpochRevision {
        self.revision
    }
    /// Returns the one current summary identity, if this policy has a head.
    pub fn head(&self) -> Option<&ReplaySummaryIdentity> {
        self.head.as_ref()
    }
    /// Returns current epoch membership, including inherited older-born raw tail.
    pub fn members(&self) -> &[ReplayEpochMember] {
        &self.members
    }

    /// Refreshes exact system/developer/project slots without touching raw source
    /// or the replay revision. A pending plan preserves the latest live slots.
    pub fn replace_stable_slots(
        &mut self,
        slots: Vec<StableContextBlock>,
    ) -> AgentContextResult<()> {
        self.context.replace_stable_slots(slots).map_err(|_| {
            AgentContextError::new("replay stable-prefix replacement failed canonical validation")
        })
    }

    /// Appends actual execution events and conservatively protects every touched
    /// group until its runtime owner explicitly finishes it. A later append to
    /// an old group reopens protection; model text cannot declare completion.
    pub fn append_execution_events(
        &mut self,
        events: impl IntoIterator<Item = ContextConversationAppend>,
    ) -> AgentContextResult<Vec<ContextEventSequence>> {
        let mut candidate = self.clone();
        let events = events.into_iter().collect::<Vec<_>>();
        let groups = events
            .iter()
            .filter_map(|event| event.execution_group_id.clone())
            .collect::<BTreeSet<_>>();
        let sequences = candidate
            .context
            .append_conversation_events(events)
            .map_err(|_| {
                AgentContextError::new("replay execution admission failed canonical validation")
            })?;
        for group in groups {
            candidate.context.retain_pending_execution_group(&group)?;
        }
        candidate.record_arrivals(&sequences);
        candidate.validate_membership()?;
        *self = candidate;
        Ok(sequences)
    }

    /// Captures an exact pending group's admitted source frontier. The caller
    /// supplies the occurrence from its actual append/restore receipt, not a
    /// guessed latest generation. This fence is not execution/reap evidence.
    pub fn capture_execution_owner(
        &self,
        group: &ContextExecutionGroupId,
        frontier: ContextEventSequence,
    ) -> AgentContextResult<ReplayExecutionOwner> {
        if !self.context.execution_group_is_pending(group)
            || self.group_frontier(group) != Some(frontier)
        {
            return Err(AgentContextError::new(
                "execution owner does not match pending source frontier",
            ));
        }
        Ok(ReplayExecutionOwner {
            owner_identity: self.owner_identity.clone(),
            group: group.clone(),
            frontier,
        })
    }

    /// Releases group protection only for the captured source incarnation after
    /// runtime retirement and complete native pairing. A later append invalidates
    /// an older closure. This supplies no native launch/reap/worker authority.
    pub fn finish_execution_group(
        &mut self,
        owner: &ReplayExecutionOwner,
    ) -> AgentContextResult<()> {
        if owner.owner_identity != self.owner_identity
            || !self.context.execution_group_is_pending(&owner.group)
            || self.group_frontier(&owner.group) != Some(owner.frontier)
        {
            return Err(AgentContextError::new(
                "execution closure has a stale owner or source frontier",
            ));
        }
        self.validate_native_group_closure(&owner.group)?;
        self.context.release_pending_execution_group(&owner.group);
        Ok(())
    }

    /// Reads the last original occurrence of one group without changing it.
    fn group_frontier(&self, group: &ContextExecutionGroupId) -> Option<ContextEventSequence> {
        self.context
            .chronology()
            .iter()
            .rev()
            .find(|event| event.execution_group_id() == Some(group))
            .map(ConversationEvent::sequence)
    }

    /// Appends a settled neutral fact with a fresh original occurrence identity.
    /// Equal text remains separate source; no provider or native work is replayed.
    pub fn append_fact(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        let mut candidate = self.clone();
        let sequence = candidate
            .context
            .append_runtime_fact_event(label, content)
            .map_err(|_| {
                AgentContextError::new("replay fact admission failed canonical validation")
            })?;
        candidate.record_arrivals(&[sequence]);
        candidate.validate_membership()?;
        *self = candidate;
        Ok(sequence)
    }

    /// Records fresh members only after their canonical append commits.
    fn record_arrivals(&mut self, sequences: &[ContextEventSequence]) {
        self.members
            .extend(sequences.iter().map(|origin| ReplayEpochMember {
                origin: *origin,
                born_in: self.revision,
            }));
    }
}

/// Lists instruction families whose explicit task owner may retire their exact
/// chronology retention. Stable authority and arbitrary runtime policy cannot
/// acquire this lifecycle merely through a label or a completion request.
pub(super) fn instruction_source_can_complete(block: &ContextBlock) -> bool {
    block.placement == ContextPlacement::ConversationAppend
        && matches!(
            block.source,
            ContextSourceKind::UserInstruction
                | ContextSourceKind::SkillInstruction
                | ContextSourceKind::PersistedContextDocument
                | ContextSourceKind::LocalMessage
        )
}

impl std::fmt::Debug for ReplayEpochContext {
    /// Emits controller identities and counts without prompt or source content.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayEpochContext")
            .field("conversation", &self.conversation)
            .field("revision", &self.revision)
            .field("policy", &self.policy)
            .field("head", &self.head)
            .field("raw_members", &self.members.len())
            .field("active_instruction_owners", &self.instruction_owners.len())
            .finish_non_exhaustive()
    }
}
