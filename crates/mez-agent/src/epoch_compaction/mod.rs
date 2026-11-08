//! Model-work reduction for one frozen canonical compaction epoch.
//!
//! The controller owns no transport, executor, persistence or usage ledger.
//! Adapters render/redact whole canonical units once, measure actual auxiliary
//! requests, and dispatch admitted work through their existing resource owners.
//! Source and intermediate summaries remain private until a single typed result
//! covers the entire frozen selection. A usable final is never regenerated for
//! compression ratio, advisory output size or prospective ordinary-request fit.

use crate::{
    AgentContextError, AgentContextResult, ContextBlockOccurrenceId, ConversationEvent,
    ReplayCompactionPlan,
};
use std::{collections::BTreeMap, ops::Range, sync::Arc};

mod controller;
mod packing;
mod scheduling;
pub use scheduling::{CompactionSchedule, CompactionScheduleEstimate};
mod source;
pub use source::{CompactionSourceUnit, FrozenEpochCompactionSource};
#[cfg(test)]
mod tests;

/// Genuine resource bounds, supplied by the existing request/worker owner.
/// Output bounds are parser/memory limits, not semantic compression targets.
#[derive(Debug, Clone, Copy)]
pub struct EpochCompactionLimits {
    /// Maximum measured complete auxiliary request input, including framing.
    pub input_tokens: usize,
    /// Fixed instructions/framing used only for the initial source lower bound.
    /// Exact admission still measures the complete rendered request.
    pub framing_tokens: usize,
    /// Maximum total rendered redacted source bytes held by this operation.
    pub source_bytes: usize,
    /// Maximum bytes in one decoded completed provider summary.
    pub summary_bytes: usize,
    /// Maximum total retained intermediate summary bytes.
    pub buffered_summary_bytes: usize,
    /// Maximum simultaneously retained graph nodes, including raw fragments.
    pub work_nodes: usize,
    /// Existing resource owner's maximum concurrent auxiliary dispatches.
    pub in_flight: usize,
    /// Advisory expected summary input cost for scheduling estimates only.
    pub expected_summary_tokens: usize,
}

impl EpochCompactionLimits {
    fn validate(self) -> AgentContextResult<()> {
        if self.input_tokens <= self.framing_tokens
            || self.source_bytes == 0
            || self.summary_bytes == 0
            || self.buffered_summary_bytes < self.summary_bytes
            || self.work_nodes == 0
            || self.in_flight == 0
            || self.expected_summary_tokens == 0
        {
            return Err(invalid("invalid epoch compaction resource bounds"));
        }
        Ok(())
    }
}

/// Opaque logical work identity, separate from physical provider dispatch IDs,
/// runtime generations and canonical source/publication occurrence identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpochCompactionWorkId(u64);

/// One exact private auxiliary input. Cloning for a transport retry preserves
/// the same input and work identity; incurred usage belongs to each dispatch.
#[derive(Clone)]
pub struct EpochCompactionWork {
    id: EpochCompactionWorkId,
    input: Arc<str>,
    coverage: Range<usize>,
    raw_coverage: Vec<Range<usize>>,
    depth: usize,
}

impl EpochCompactionWork {
    /// Returns the logical completion fence, never a transport attempt ID.
    pub fn id(&self) -> EpochCompactionWorkId {
        self.id
    }
    /// Returns the exact redacted data input to the common compactor prompt.
    pub fn input(&self) -> &str {
        &self.input
    }
    /// Returns the contiguous original rendered-byte interval represented.
    pub fn coverage(&self) -> Range<usize> {
        self.coverage.clone()
    }
    /// Returns original byte intervals supplied raw by this work. Other inputs
    /// are model-authored private intermediates covering earlier intervals.
    pub fn raw_coverage(&self) -> &[Range<usize>] {
        &self.raw_coverage
    }
    /// Returns the longest necessary model dependency chain through this work.
    pub fn serial_depth(&self) -> usize {
        self.depth
    }
}

impl std::fmt::Debug for EpochCompactionWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochCompactionWork")
            .field("id", &self.id)
            .field("bytes", &self.input.len())
            .field("coverage", &self.coverage)
            .field("depth", &self.depth)
            .finish()
    }
}

/// One completed summary, still carrying its exact original source plan.
/// Only the canonical publisher may adopt it after checking live source fences.
pub struct AcceptedEpochCompaction {
    plan: ReplayCompactionPlan,
    summary: String,
    units: Vec<CompactionSourceUnit>,
    work_count: u64,
    serial_depth: usize,
}

impl std::fmt::Debug for AcceptedEpochCompaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptedEpochCompaction")
            .field("plan", &self.plan)
            .field("summary_bytes", &self.summary.len())
            .field("units", &self.units.len())
            .field("work_count", &self.work_count)
            .field("serial_depth", &self.serial_depth)
            .finish()
    }
}

impl AcceptedEpochCompaction {
    /// Returns the immutable source/parent fence to validate at publication.
    pub fn plan(&self) -> &ReplayCompactionPlan {
        &self.plan
    }
    /// Returns the model-authored completed summary without local rewriting.
    pub fn summary(&self) -> &str {
        &self.summary
    }
    /// Returns complete original unit/occurrence and rendered-byte witnesses.
    pub fn source_units(&self) -> &[CompactionSourceUnit] {
        &self.units
    }
    /// Returns logical work nodes, excluding physical retries and repairs.
    pub fn work_count(&self) -> u64 {
        self.work_count
    }
    /// Returns actual longest model dependency chain, excluding retry delays.
    pub fn serial_depth(&self) -> usize {
        self.serial_depth
    }
    /// Transfers final material and original source to the publication adapter.
    pub fn into_parts(self) -> (ReplayCompactionPlan, String) {
        (self.plan, self.summary)
    }
}

/// Work available now, a bounded wait for claimed completions, or a final result.
#[derive(Debug)]
pub enum EpochCompactionStep {
    /// Dispatch through the existing provider/usage/worker ownership boundary.
    Work(EpochCompactionWork),
    /// No new work can run until already claimed work completes.
    Waiting,
    /// All selected source is represented by one usable completed summary.
    Complete,
}

#[derive(Clone)]
enum Material {
    Raw(Range<usize>),
    Summary(Arc<str>),
    Claimed(EpochCompactionWorkId),
}

#[derive(Clone)]
struct Node {
    coverage: Range<usize>,
    material: Material,
    depth: usize,
}

struct Claim {
    work: EpochCompactionWork,
    children: Vec<Node>,
}

/// Deterministic bounded work controller. Dropping it cancels private work and
/// cannot alter the original epoch. Runtime fences remain adapter-owned.
pub struct EpochCompactionEngine {
    source: FrozenEpochCompactionSource,
    limits: EpochCompactionLimits,
    schedule: CompactionScheduleEstimate,
    nodes: BTreeMap<usize, Node>,
    claims: BTreeMap<EpochCompactionWorkId, Claim>,
    next_id: u64,
    work_count: u64,
    buffered_summary_bytes: usize,
}

fn invalid(message: &'static str) -> AgentContextError {
    AgentContextError::new(message)
}
