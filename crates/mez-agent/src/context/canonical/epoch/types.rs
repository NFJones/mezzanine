//! Replay identities and immutable publication proposals, separate from worker
//! dispatch generations and provider-cache epoch fingerprints.

use super::*;

/// Local owner allocation fence. Cloned candidates share the fence; constructing
/// a replacement owner allocates a distinct one without global counters or RNG.
#[derive(Debug, Clone)]
pub(super) struct ReplayOwnerIdentity(std::sync::Arc<()>);

impl ReplayOwnerIdentity {
    /// Allocates one immutable in-memory incarnation identity.
    pub(super) fn new() -> Self {
        Self(std::sync::Arc::new(()))
    }
}

impl PartialEq for ReplayOwnerIdentity {
    /// Compares owner allocations, not the unit payload or conversation spelling.
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ReplayOwnerIdentity {}

/// Conversation namespace for canonical replay membership and task owners.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplayConversationId(String);

impl ReplayConversationId {
    /// Creates a nonempty, trimmed, control-free conversation identity.
    pub fn new(value: impl Into<String>) -> AgentContextResult<Self> {
        let value = value.into();
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(AgentContextError::new(
                "invalid replay conversation identity",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the controller-supplied conversation namespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Committed replay revision; zero is the initial unsummarized epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplayEpochRevision(pub(super) u64);

impl ReplayEpochRevision {
    /// Returns the committed revision, never a provider attempt generation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Summary identity belongs to a publication, not to a covered raw occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySummaryIdentity {
    pub(super) conversation: ReplayConversationId,
    pub(super) revision: ReplayEpochRevision,
    pub(super) occurrence_id: ContextBlockOccurrenceId,
}

impl ReplaySummaryIdentity {
    /// Returns the actual summary occurrence, independent of its ordering anchor.
    pub fn occurrence_id(&self) -> ContextBlockOccurrenceId {
        self.occurrence_id
    }
    /// Returns the summary's conversation namespace.
    pub fn conversation(&self) -> &ReplayConversationId {
        &self.conversation
    }
    /// Returns the publication revision which created this head.
    pub fn revision(&self) -> ReplayEpochRevision {
        self.revision
    }
}

/// One current raw member and its immutable birth revision. Membership in the
/// owner's current epoch is independent of this birth value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayEpochMember {
    pub(super) origin: ContextEventSequence,
    pub(super) occurrence_id: ContextBlockOccurrenceId,
    pub(super) born_in: ReplayEpochRevision,
}

impl ReplayEpochMember {
    /// Returns immutable origin across local resequencing and epoch transfer.
    pub fn occurrence_id(&self) -> ContextBlockOccurrenceId {
        self.occurrence_id
    }
    /// Returns the local canonical ordering position, separate from durable origin.
    pub fn origin(&self) -> ContextEventSequence {
        self.origin
    }
    /// Returns the birth revision; this never filters current membership.
    pub fn born_in(&self) -> ReplayEpochRevision {
        self.born_in
    }
}

/// Exact locally allocated task handle. Completed generations are never reused,
/// so a stale completion cannot release a later task's instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayInstructionOwner {
    pub(super) owner_identity: ReplayOwnerIdentity,
    pub(super) conversation: ReplayConversationId,
    pub(super) generation: u64,
}

/// Exact source fence for a pending execution group. It identifies admitted
/// source and one protection allocation, not a native process, worker generation
/// or completion receipt. Renewed protection invalidates a retired handle even
/// when its source frontier is unchanged.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplayExecutionOwner {
    pub(super) owner_identity: ReplayOwnerIdentity,
    pub(super) protection: ContextExecutionProtection,
    pub(super) group: ContextExecutionGroupId,
    pub(super) frontier: ContextEventSequence,
}

impl ReplayExecutionOwner {
    /// Returns the exact source occurrence bound to this closure fence.
    pub fn frontier(&self) -> ContextEventSequence {
        self.frontier
    }
}

impl std::fmt::Debug for ReplayExecutionOwner {
    /// Reports only a numeric source frontier, not opaque provider/group labels.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayExecutionOwner")
            .field("frontier", &self.frontier)
            .finish_non_exhaustive()
    }
}

/// Required head policy for a publication. Summary is the default. The explicit
/// omission contract is for later policy-qualified adapters, not a product
/// configuration option or an alternate runtime compaction engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayHeadPolicy {
    /// A completed, nonempty, bounded model summary must be adopted.
    #[default]
    Summary,
    /// A trusted policy deliberately omits the head; blank model output cannot
    /// acquire this meaning on a summary-policy owner.
    IntentionalOmission,
}

/// Completed publication material. No variant dispatches model or native work.
#[derive(Clone, PartialEq, Eq)]
pub enum ReplayHeadPublication {
    /// Completed model-authored text, subject only to usability/resource checks.
    Summary(String),
    /// Explicit policy-qualified absence, never an empty successful model reply.
    IntentionalOmission,
}

/// Shared tail selection inputs for manual, observed-input and provider-limit
/// triggers. The budget determines retained raw size, not a summary shrink goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayTailPolicy {
    pub(super) context_tokens: usize,
    pub(super) retained_percent: u8,
    /// None uses the committed owner's policy. Product admission captures an
    /// explicit choice so later configuration changes cannot switch the plan.
    pub(super) head_policy: Option<ReplayHeadPolicy>,
    pub(super) fixed_input_tokens: usize,
    pub(super) optional_unit_limit: Option<usize>,
}

impl ReplayTailPolicy {
    /// Creates a positive context allowance and a raw retention percentage in
    /// 0..=100. Invalid input does not prepare model work.
    /// Without an explicit choice the committed owner's head policy applies.
    pub fn new(context_tokens: usize, retained_percent: u8) -> AgentContextResult<Self> {
        if context_tokens == 0 || retained_percent > 100 {
            return Err(AgentContextError::new("invalid replay raw-tail policy"));
        }
        Ok(Self {
            context_tokens,
            retained_percent,
            head_policy: None,
            fixed_input_tokens: 0,
            optional_unit_limit: None,
        })
    }

    /// Captures an explicit next-publication policy without rewriting history.
    /// The committed policy changes only after this plan is actually published.
    pub fn with_head_policy(mut self, policy: ReplayHeadPolicy) -> Self {
        self.head_policy = Some(policy);
        self
    }

    /// Captures summary-free request overhead and an optional upper bound on
    /// retained whole optional units. The controller may lower that bound after
    /// an actual input rejection; this method initiates no request or retry.
    /// Protected units are independent of the optional limit and never dropped.
    pub fn with_raw_tail_limits(
        mut self,
        fixed_input_tokens: usize,
        optional_unit_limit: Option<usize>,
    ) -> Self {
        self.head_policy = Some(ReplayHeadPolicy::IntentionalOmission);
        self.fixed_input_tokens = fixed_input_tokens;
        self.optional_unit_limit = optional_unit_limit;
        self
    }

    /// Returns the optional raw reservation, rounding fractional tokens upward
    /// without overflow. Protected source may exceed it; optional closed units
    /// must fit whole instead of being sliced or overretained.
    pub fn retained_raw_tokens(self) -> usize {
        let percent = usize::from(self.retained_percent);
        self.context_tokens / 100 * percent + (self.context_tokens % 100 * percent).div_ceil(100)
    }
}

/// Content-free reason why no source can be summarized under the common policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayCompactionNoWork {
    /// There are no raw members; a prior head alone is not new source work.
    NoRawMembers,
    /// Exact instructions, unresolved work or unrecoverable source blocks prefix selection.
    ProtectedPrefix,
    /// Exact stable/protected input and fixed framing exceed model capacity.
    ProtectedInputExceedsBudget,
    /// Every optional member already fits the captured raw allowance/frontier
    /// and there is no prior summary to remove. Publishing would change nothing.
    RetainedSourceUnchanged,
}

/// Planning either freezes genuine work or reports an explicit no-work result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayCompactionPreparation {
    /// Frozen immutable source awaiting one publication decision.
    Ready(ReplayCompactionPlan),
    /// No provider dispatch or epoch publication is warranted.
    NoWork(ReplayCompactionNoWork),
}

/// Immutable source and expected parent for one atomic epoch publication.
/// Frozen source may coexist with append-only arrivals, but cannot be rewritten.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplayCompactionPlan {
    pub(super) owner_identity: ReplayOwnerIdentity,
    pub(super) conversation: ReplayConversationId,
    pub(super) parent: ReplayEpochRevision,
    pub(super) source_policy: ReplayHeadPolicy,
    pub(super) policy: ReplayHeadPolicy,
    pub(super) head: Option<ReplaySummaryIdentity>,
    pub(super) frozen: Vec<ConversationEvent>,
    pub(super) members: Vec<ReplayEpochMember>,
    /// Keeps no-work preparation compact without changing the frozen selection.
    pub(super) selection: Box<ReplayCompactionSelection>,
    pub(super) input_estimates: ReplayCompactionInputEstimates,
    /// Keeps the preparation enum compact while freezing an owned descriptor.
    pub(super) provider_projection: Option<Box<ReplayProviderProjection>>,
    /// Original unselected closed tail, excluding later arrivals/protection changes.
    pub(super) reentry: Box<ReplayCompactionReentryInput>,
}

/// Policy-specific source selection inside the same frozen owner/publication
/// contract. Raw omission may surround exact obligations; it never publishes a
/// second history or infers lifecycle completion for those retained obligations.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum ReplayCompactionSelection {
    Prefix {
        members: usize,
        events: usize,
    },
    RawTail {
        omitted: BTreeSet<ContextBlockOccurrenceId>,
        tail_start: usize,
        retained_optional_units: usize,
    },
}

impl ReplayCompactionPlan {
    /// Returns the exact budget projection frozen for this source selection.
    /// None denotes ordinary framing; switching after planning rejects publication.
    pub fn provider_projection(&self) -> Option<&ReplayProviderProjection> {
        self.provider_projection.as_deref()
    }
    /// Returns frozen source/tail estimates, excluding stable and fixed overhead.
    pub fn input_estimates(&self) -> ReplayCompactionInputEstimates {
        self.input_estimates
    }
    /// Returns the exact committed parent required at publication.
    pub fn expected_parent(&self) -> ReplayEpochRevision {
        self.parent
    }
    /// Returns the frozen arrival frontier, including its original occurrence ID.
    pub fn arrival_frontier(&self) -> Option<ContextEventSequence> {
        self.frozen.last().map(ConversationEvent::sequence)
    }
    /// Returns the sole prior head plus selected summary prefix, never covered
    /// ancestors. Intentional omission returns no model input.
    pub fn summary_input(&self) -> &[ConversationEvent] {
        match self.selection.as_ref() {
            ReplayCompactionSelection::Prefix { events, .. } => &self.frozen[..*events],
            ReplayCompactionSelection::RawTail { .. } => &[],
        }
    }
    /// Returns whole source units in chronological order, with the prior head
    /// first when present. Noncontiguous/overlapping execution groups use the
    /// same ownership spans as source selection. These slices are auxiliary
    /// input boundaries, never permission to publish a partial replacement.
    /// Intentional omission returns no model units.
    pub fn summary_units(&self) -> Vec<&[ConversationEvent]> {
        if !self.requires_model_summary() {
            return Vec::new();
        }
        let offset = usize::from(self.head.is_some());
        let mut units = Vec::new();
        if offset != 0 {
            units.push(&self.frozen[..offset]);
        }
        let raw = &self.summary_input()[offset..];
        units.extend(
            super::planning::execution_units(raw)
                .into_iter()
                .map(|range| &raw[range]),
        );
        units
    }
    /// Returns the newest contiguous retained suffix. Summary-free selection may
    /// also retain earlier protected units; `retained_raw_events` includes those.
    pub fn retained_tail(&self) -> &[ConversationEvent] {
        let start = match self.selection.as_ref() {
            ReplayCompactionSelection::Prefix { events, .. } => *events,
            ReplayCompactionSelection::RawTail { tail_start, .. } => *tail_start,
        };
        &self.frozen[start..]
    }
    /// Reports whether the configured publication needs a model summary.
    pub fn requires_model_summary(&self) -> bool {
        self.policy == ReplayHeadPolicy::Summary
    }

    /// Returns the captured policy for the next committed epoch.
    pub fn head_policy(&self) -> ReplayHeadPolicy {
        self.policy
    }

    /// Visits exact frozen raw members removed by this publication, excluding
    /// the previous summary head. Descriptive membership grants no execution.
    pub fn removed_raw_members(&self) -> impl Iterator<Item = &ReplayEpochMember> {
        let offset = usize::from(self.head.is_some());
        self.members
            .iter()
            .enumerate()
            .filter(move |(index, _)| self.removes_event(index + offset))
            .map(|(_, member)| member)
    }

    /// Visits removed original events, including a prior head when present.
    /// Raw omission has no model source; its receipt still records exact loss.
    pub fn removed_events(&self) -> impl Iterator<Item = &ConversationEvent> {
        self.frozen
            .iter()
            .enumerate()
            .filter(|(index, _)| self.removes_event(*index))
            .map(|(_, event)| event)
    }

    /// Visits all retained frozen raw events in original order, including exact
    /// obligations outside the optional suffix. Later arrivals are not frozen.
    pub fn retained_raw_events(&self) -> impl Iterator<Item = &ConversationEvent> {
        self.frozen
            .iter()
            .enumerate()
            .skip(usize::from(self.head.is_some()))
            .filter(|(index, _)| !self.removes_event(*index))
            .map(|(_, event)| event)
    }

    /// Returns the retained optional-unit frontier for a summary-free plan.
    /// None identifies summary work, not an unknown number of provider calls.
    pub fn retained_optional_units(&self) -> Option<usize> {
        match self.selection.as_ref() {
            ReplayCompactionSelection::Prefix { .. } => None,
            ReplayCompactionSelection::RawTail {
                retained_optional_units,
                ..
            } => Some(*retained_optional_units),
        }
    }

    /// Checks a frozen event position; post-frontier arrivals are never selected.
    pub(super) fn removes_event(&self, index: usize) -> bool {
        match self.selection.as_ref() {
            ReplayCompactionSelection::Prefix { events, .. } => index < *events,
            ReplayCompactionSelection::RawTail { omitted, .. } => {
                self.head.is_some() && index == 0
                    || self
                        .frozen
                        .get(index)
                        .is_some_and(|event| omitted.contains(&event.record().occurrence_id()))
            }
        }
    }
}

impl std::fmt::Debug for ReplayCompactionPlan {
    /// Reports frozen source counts and parent identity, never source bytes.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayCompactionPlan")
            .field("conversation", &self.conversation)
            .field("parent", &self.parent)
            .field("policy", &self.policy)
            .field("frozen_events", &self.frozen.len())
            .field("removed_members", &self.removed_raw_members().count())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ReplayHeadPublication {
    /// Reports the publication category and byte count without model text.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Summary(text) => formatter
                .debug_struct("Summary")
                .field("bytes", &text.len())
                .finish(),
            Self::IntentionalOmission => formatter.write_str("IntentionalOmission"),
        }
    }
}
