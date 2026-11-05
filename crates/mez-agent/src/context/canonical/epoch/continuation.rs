//! In-memory replay ownership while the live runtime holds canonical context.
//!
//! There is still one mutable history body. Moving it out of the replay owner
//! carries only private controller metadata and content-free event seals inside
//! AgentContext. Returning it checks the original source before reconciling
//! genuine appends. Checkpoints cannot manufacture this continuation, preserve
//! an incarnation, or mint lifecycle transitions. Stable instructions and request
//! metadata remain live inputs. No continuation is serialized or projected.

use super::super::ContextBlockMetadata;
use super::*;
use sha2::{Digest, Sha256};

/// Metadata retained beside, never instead of, the sole canonical history body.
#[derive(Clone, PartialEq, Eq)]
pub(in super::super) struct ReplayEpochContinuation {
    owner_identity: ReplayOwnerIdentity,
    conversation: ReplayConversationId,
    revision: ReplayEpochRevision,
    policy: ReplayHeadPolicy,
    head: Option<ReplaySummaryIdentity>,
    members: Vec<ReplayEpochMember>,
    instruction_owners: BTreeMap<u64, BTreeSet<ContextEventSequence>>,
    next_instruction_owner: u64,
    source_lifecycle: ReplaySourceLifecycle,
    provider_projection: Option<ReplayProviderProjection>,
    seals: Vec<EventSeal>,
    prelude_seals: BTreeMap<ContextEventSequence, EventSeal>,
    next_event_sequence: u64,
    pending_groups: BTreeSet<ContextExecutionGroupId>,
    pending_peers: BTreeSet<ContextEventSequence>,
    historical_instructions: BTreeSet<ContextEventSequence>,
}

/// Exact source framing and metadata without retaining a second event body.
#[derive(Clone, PartialEq, Eq)]
struct EventSeal {
    metadata: ContextBlockMetadata,
    source: ContextSourceKind,
    placement: ContextPlacement,
    label_sha256: [u8; 32],
    content_sha256: [u8; 32],
}

impl EventSeal {
    /// Seals actual canonical source, including framing not used by provider text.
    fn capture(event: &ConversationEvent) -> Self {
        Self {
            metadata: event.metadata(),
            source: event.block.source,
            placement: event.block.placement,
            label_sha256: Sha256::digest(event.block.label.as_bytes()).into(),
            content_sha256: Sha256::digest(event.block.content.as_bytes()).into(),
        }
    }
}

impl ReplayEpochContinuation {
    /// Remembers actual re-protection immediately; stale body snapshots cannot
    /// erase it or reuse a closure witness from before this obligation.
    pub(in super::super) fn protect_group(&mut self, group: &ContextExecutionGroupId) {
        self.pending_groups.insert(group.clone());
        self.source_lifecycle.closed_groups.remove(group);
    }

    /// Keeps the exact peer protection fence and invalidates its former handoff.
    pub(in super::super) fn protect_peer(
        &mut self,
        sequence: ContextEventSequence,
        occurrence: ContextBlockOccurrenceId,
    ) {
        self.pending_peers.insert(sequence);
        self.source_lifecycle.handed_off_peers.remove(&occurrence);
    }

    /// Records only preludes minted by the actual prompt-boundary insertion.
    /// These may occupy an older reserved gap without moving existing source.
    pub(in super::super) fn admit_preludes(&mut self, events: &[ConversationEvent]) {
        for event in events {
            self.prelude_seals
                .insert(event.sequence(), EventSeal::capture(event));
        }
    }
}

impl std::fmt::Debug for ReplayEpochContinuation {
    /// Omits private seals, task handles, source bytes and lifecycle witnesses.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayEpochContinuation")
            .field("revision", &self.revision)
            .field("raw_members", &self.members.len())
            .finish_non_exhaustive()
    }
}

impl ReplayEpochContext {
    /// Moves the sole history body to its live runtime owner, preserving private
    /// incarnation, task handles and controller witnesses as in-memory metadata.
    /// No checkpoint, durable receipt, authority transfer or effect is created.
    pub fn into_owned_context(self) -> AgentContext {
        let provider_projection = self.provider_projection().cloned();
        let Self {
            mut context,
            owner_identity,
            conversation,
            revision,
            policy,
            head,
            members,
            instruction_owners,
            next_instruction_owner,
            source_lifecycle,
            accounting: _,
        } = self;
        let continuation = ReplayEpochContinuation {
            owner_identity,
            conversation,
            revision,
            policy,
            head,
            members,
            instruction_owners,
            next_instruction_owner,
            source_lifecycle,
            provider_projection,
            seals: context.chronology.iter().map(EventSeal::capture).collect(),
            prelude_seals: BTreeMap::new(),
            next_event_sequence: context.next_event_sequence,
            pending_groups: context.pending_execution_groups.clone(),
            pending_peers: context.pending_peer_messages.clone(),
            historical_instructions: context.historical_instruction_events.clone(),
        };
        context.replay_continuation = Some(Box::new(continuation));
        context
    }

    /// Returns an actual in-memory owner for the externally selected conversation.
    /// Existing source must match its private seals and order, reservations may
    /// not rewind and pending protection may not silently disappear. Only genuine
    /// appends or controller-minted late preludes become new current-born members.
    /// New execution/peer source is conservatively pending until explicit release.
    /// Failure consumes this input; clone before calling when rollback is needed.
    pub fn from_owned_context(
        conversation: ReplayConversationId,
        mut context: AgentContext,
    ) -> AgentContextResult<Self> {
        let state = *context.replay_continuation.take().ok_or_else(invalid)?;
        context.validate_durable()?;
        if state.conversation != conversation
            || context.next_event_sequence < state.next_event_sequence
            || !state
                .pending_groups
                .is_subset(&context.pending_execution_groups)
            || !state
                .pending_peers
                .is_subset(&context.pending_peer_messages)
            || state.historical_instructions != context.historical_instruction_events
        {
            return Err(invalid());
        }
        let mut original = state.seals.iter().peekable();
        let mut preludes = state.prelude_seals.clone();
        let mut inserted_preludes = BTreeSet::new();
        let mut members = Vec::with_capacity(context.chronology.len());
        let saved_members = state
            .members
            .iter()
            .map(|member| (member.occurrence_id, member))
            .collect::<BTreeMap<_, _>>();
        for event in &context.chronology {
            let seal = EventSeal::capture(event);
            let existing = original
                .peek()
                .is_some_and(|saved| saved.metadata.event_sequence == Some(event.sequence()));
            if existing {
                if original.next() != Some(&seal) {
                    return Err(invalid());
                }
            } else {
                let prelude = preludes.remove(&event.sequence());
                if prelude.as_ref().is_some_and(|saved| saved != &seal)
                    || (prelude.is_none() && event.sequence().get() < state.next_event_sequence)
                {
                    return Err(invalid());
                }
                if prelude.is_some() {
                    inserted_preludes.insert(event.record().occurrence_id());
                }
                if let Some(group) = event.execution_group_id() {
                    context.pending_execution_groups.insert(group.clone());
                }
                if event.block.source == ContextSourceKind::PeerMessage {
                    context.pending_peer_messages.insert(event.sequence());
                }
            }
            if state
                .head
                .as_ref()
                .is_some_and(|head| head.occurrence_id() == event.record().occurrence_id())
            {
                continue;
            }
            members.push(match saved_members.get(&event.record().occurrence_id()) {
                Some(member) => (*member).clone(),
                None => ReplayEpochMember {
                    origin: event.sequence(),
                    occurrence_id: event.record().occurrence_id(),
                    born_in: state.revision,
                },
            });
        }
        if original.next().is_some() || !preludes.is_empty() {
            return Err(invalid());
        }
        let mut owner = Self {
            context,
            owner_identity: state.owner_identity,
            conversation,
            revision: state.revision,
            policy: state.policy,
            head: state.head,
            members,
            accounting: ReplayAccounting::default(),
            instruction_owners: state.instruction_owners,
            next_instruction_owner: state.next_instruction_owner,
            source_lifecycle: state.source_lifecycle,
        };
        owner
            .source_lifecycle
            .inserted_preludes
            .extend(inserted_preludes);
        owner.prune_source_lifecycle();
        owner.initialize_accounting()?;
        if let Some(projection) = state.provider_projection {
            owner.select_provider_projection(projection)?;
        }
        owner.validate_membership()?;
        Ok(owner)
    }
}

impl AgentContext {
    /// Applies conservative protection at actual live admission, before either
    /// legacy readers or a later checked owner conversion can observe the body.
    pub(in super::super) fn protect_replay_arrival(&mut self, event: &ConversationEvent) {
        if let Some(continuation) = &mut self.replay_continuation {
            if let Some(group) = event.execution_group_id() {
                self.pending_execution_groups.insert(group.clone());
                continuation.protect_group(group);
            }
            if event.block.source == ContextSourceKind::PeerMessage {
                self.pending_peer_messages.insert(event.sequence());
                continuation.protect_peer(event.sequence(), event.record().occurrence_id());
            }
        }
    }

    /// Reports an actual retained live incarnation, never inferred from a summary.
    pub fn has_replay_owner(&self) -> bool {
        self.replay_continuation.is_some()
    }

    /// Applies a typed lifecycle/publication mutation to the actual retained
    /// owner, atomically returning its single body to this context. Errors leave
    /// the original context and private continuation unchanged. No checkpoint is
    /// decoded and no effect, persistence acknowledgment or permission is created.
    pub fn update_replay_owner<T>(
        &mut self,
        update: impl FnOnce(&mut ReplayEpochContext) -> AgentContextResult<T>,
    ) -> AgentContextResult<T> {
        let conversation = self
            .replay_continuation
            .as_ref()
            .ok_or_else(invalid)?
            .conversation
            .clone();
        let mut owner = ReplayEpochContext::from_owned_context(conversation, self.clone())?;
        let result = update(&mut owner)?;
        *self = owner.into_owned_context();
        Ok(result)
    }
}

/// Refuses only this ownership conversion without disclosing source or handles.
fn invalid() -> AgentContextError {
    AgentContextError::new("live replay continuation does not match its original source")
}
