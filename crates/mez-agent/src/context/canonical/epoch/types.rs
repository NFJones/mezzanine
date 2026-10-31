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
}

impl ReplaySummaryIdentity {
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
    pub(super) born_in: ReplayEpochRevision,
}

impl ReplayEpochMember {
    /// Returns the original canonical occurrence, including for equal text.
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
/// source, not a native process, worker generation or completion receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplayExecutionOwner {
    pub(super) owner_identity: ReplayOwnerIdentity,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
}

impl ReplayTailPolicy {
    /// Creates a positive context allowance and a raw retention percentage in
    /// 0..=100. Invalid input does not prepare model work.
    pub fn new(context_tokens: usize, retained_percent: u8) -> AgentContextResult<Self> {
        if context_tokens == 0 || retained_percent > 100 {
            return Err(AgentContextError::new("invalid replay raw-tail policy"));
        }
        Ok(Self {
            context_tokens,
            retained_percent,
        })
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
    pub(super) policy: ReplayHeadPolicy,
    pub(super) head: Option<ReplaySummaryIdentity>,
    pub(super) frozen: Vec<ConversationEvent>,
    pub(super) members: Vec<ReplayEpochMember>,
    pub(super) prefix_members: usize,
    pub(super) prefix_events: usize,
}

impl ReplayCompactionPlan {
    /// Returns the exact committed parent required at publication.
    pub fn expected_parent(&self) -> ReplayEpochRevision {
        self.parent
    }
    /// Returns the frozen arrival frontier, including its original occurrence ID.
    pub fn arrival_frontier(&self) -> Option<ContextEventSequence> {
        self.frozen.last().map(ConversationEvent::sequence)
    }
    /// Returns the sole prior head plus selected raw prefix, never covered ancestors.
    pub fn summary_input(&self) -> &[ConversationEvent] {
        &self.frozen[..self.prefix_events]
    }
    /// Returns exact raw tail retained outside this model summary input.
    pub fn retained_tail(&self) -> &[ConversationEvent] {
        &self.frozen[self.prefix_events..]
    }
    /// Reports whether the configured publication needs a model summary.
    pub fn requires_model_summary(&self) -> bool {
        self.policy == ReplayHeadPolicy::Summary
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
            .field("prefix_members", &self.prefix_members)
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
