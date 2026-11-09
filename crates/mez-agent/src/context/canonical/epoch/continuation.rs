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
    compaction_reentry: Option<ReplayCompactionReentry>,
    provider_projection: Option<ReplayProviderProjection>,
    seals: Vec<EventSeal>,
    prelude_seals: BTreeMap<ContextEventSequence, EventSeal>,
    next_event_sequence: u64,
    pending_groups: BTreeMap<ContextExecutionGroupId, ContextExecutionProtection>,
    execution_intervals: BTreeMap<ContextExecutionGroupId, ExecutionInterval>,
    /// Fences captured at typed body transfer or actual canonical append.
    execution_frontiers: BTreeMap<ContextExecutionGroupId, ReplayExecutionOwner>,
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
    pub(in super::super) fn protect_group(
        &mut self,
        group: &ContextExecutionGroupId,
        protection: &ContextExecutionProtection,
        renewed: bool,
    ) -> AgentContextResult<()> {
        if !renewed
            && self
                .pending_groups
                .get(group)
                .is_some_and(|held| held != protection)
        {
            return Err(invalid_source("pending execution allocation changed"));
        }
        if self
            .execution_frontiers
            .get(group)
            .is_some_and(|held| &held.protection != protection)
        {
            self.execution_frontiers.remove(group);
        }
        self.pending_groups
            .insert(group.clone(), protection.clone());
        self.execution_intervals
            .insert(group.clone(), protection.interval().clone());
        self.source_lifecycle.closed_groups.remove(group);
        Ok(())
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
            compaction_reentry,
            accounting: _,
        } = self;
        let continuation = ReplayEpochContinuation {
            execution_frontiers: context
                .chronology
                .iter()
                .filter_map(|event| {
                    let group = event.execution_group_id()?;
                    let protection = context.pending_execution_groups.get(group)?;
                    Some((
                        group.clone(),
                        ReplayExecutionOwner {
                            owner_identity: owner_identity.clone(),
                            protection: protection.clone(),
                            group: group.clone(),
                            frontier: event.sequence(),
                        },
                    ))
                })
                .collect(),
            owner_identity,
            conversation,
            revision,
            policy,
            head,
            members,
            instruction_owners,
            next_instruction_owner,
            source_lifecycle,
            compaction_reentry,
            provider_projection,
            seals: context.chronology.iter().map(EventSeal::capture).collect(),
            prelude_seals: BTreeMap::new(),
            next_event_sequence: context.next_event_sequence,
            pending_groups: context.pending_execution_groups.clone(),
            execution_intervals: context.execution_intervals.clone(),
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
        let state = *context
            .replay_continuation
            .take()
            .ok_or_else(|| invalid_source("live owner is missing"))?;
        context.validate_durable()?;
        if state.conversation != conversation {
            return Err(invalid_source("conversation binding changed"));
        }
        if context.next_event_sequence < state.next_event_sequence {
            return Err(invalid_source("source reservation moved backward"));
        }
        if state.pending_groups.iter().any(|(group, protection)| {
            context.pending_execution_groups.get(group) != Some(protection)
        }) {
            return Err(invalid_source("pending execution protection changed"));
        }
        if state
            .execution_intervals
            .iter()
            .any(|(group, interval)| context.execution_intervals.get(group) != Some(interval))
        {
            return Err(invalid_source("execution interval changed"));
        }
        if !state
            .pending_peers
            .is_subset(&context.pending_peer_messages)
        {
            return Err(invalid_source("pending peer protection changed"));
        }
        if state.historical_instructions != context.historical_instruction_events {
            return Err(invalid_source("instruction lifecycle changed"));
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
                    return Err(invalid_source("previous source occurrence changed"));
                }
            } else {
                let prelude = preludes.remove(&event.sequence());
                if prelude.as_ref().is_some_and(|saved| saved != &seal) {
                    return Err(invalid_source("admitted prompt prelude changed"));
                }
                if prelude.is_none() && event.sequence().get() < state.next_event_sequence {
                    return Err(invalid_source("new source precedes its reserved frontier"));
                }
                if prelude.is_some() {
                    inserted_preludes.insert(event.record().occurrence_id());
                }
                if let Some(group) = event.execution_group_id()
                    && !context.execution_group_is_observational(group)
                    && !context.pending_execution_groups.contains_key(group)
                {
                    let interval =
                        ExecutionInterval::after(context.execution_intervals.get(group))?;
                    context
                        .execution_intervals
                        .insert(group.clone(), interval.clone());
                    context
                        .pending_execution_groups
                        .insert(group.clone(), ContextExecutionProtection::new(interval));
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
            return Err(invalid_source(
                "previous source or admitted prelude is missing",
            ));
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
            compaction_reentry: state.compaction_reentry,
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
    /// Returns the first original suffix occurrence of an actual task admission.
    /// This descriptive producer fact grants no task or execution capability;
    /// cold checkpoints do not manufacture it from prompt labels.
    pub fn replay_task_admission_start(
        &self,
        prompt: ContextBlockOccurrenceId,
    ) -> Option<ContextBlockOccurrenceId> {
        self.replay_continuation
            .as_ref()?
            .source_lifecycle
            .task_admissions
            .get(&prompt)
            .copied()
    }

    /// Protects actual execution at live admission. Standalone runtime
    /// observations create no effect fence; adding an assistant/native event
    /// later protects the whole group, and existing protection is never released.
    pub(in super::super) fn protect_replay_arrival(
        &mut self,
        event: &ConversationEvent,
    ) -> AgentContextResult<()> {
        if self.replay_continuation.is_none() {
            return Ok(());
        }
        let protection = event
            .execution_group_id()
            .filter(|group| {
                self.execution_group_is_pending(group)
                    || !super::super::pending::observation_only_event(event)
                    || !self
                        .chronology()
                        .iter()
                        .filter(|existing| existing.execution_group_id() == Some(*group))
                        .all(super::super::pending::observation_only_event)
            })
            .map(|group| self.activate_execution_protection(group))
            .transpose()?;
        if let Some(continuation) = &mut self.replay_continuation {
            if let (Some(group), Some(protection)) = (event.execution_group_id(), protection) {
                continuation.execution_frontiers.insert(
                    group.clone(),
                    ReplayExecutionOwner {
                        owner_identity: continuation.owner_identity.clone(),
                        protection,
                        group: group.clone(),
                        frontier: event.sequence(),
                    },
                );
            }
            if event.block.source == ContextSourceKind::PeerMessage {
                self.pending_peer_messages.insert(event.sequence());
                continuation.protect_peer(event.sequence(), event.record().occurrence_id());
            }
        }
        Ok(())
    }

    /// Reports an actual retained live incarnation, never inferred from a summary.
    pub fn has_replay_owner(&self) -> bool {
        self.replay_continuation.is_some()
    }

    /// Borrows an original source fence captured at typed body transfer or actual
    /// append. Runtime admission/result owners must retain their own clone before
    /// yielding; a later callback must not replace its fence with this latest one.
    /// This grants no execution, retirement, publication or durability authority.
    pub fn replay_execution_append_owner(
        &self,
        group: &ContextExecutionGroupId,
    ) -> Option<&ReplayExecutionOwner> {
        self.replay_continuation
            .as_ref()?
            .execution_frontiers
            .get(group)
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
            .ok_or_else(|| invalid_source("live update owner is missing"))?
            .conversation
            .clone();
        let mut owner = ReplayEpochContext::from_owned_context(conversation, self.clone())?;
        let result = update(&mut owner)?;
        *self = owner.into_owned_context();
        Ok(result)
    }
}

/// Reports only the violated invariant, never source bytes, IDs or live fences.
fn invalid_source(reason: &'static str) -> AgentContextError {
    AgentContextError::new(format!(
        "live replay continuation does not match its original source: {reason}"
    ))
}
