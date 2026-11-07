//! Descriptive execution intervals at the qualified replay boundary.
//!
//! Persisted identity orders original protection renewals without serializing a
//! live allocation or effect handle. The storage adapter must independently
//! qualify producer custody and original membership before restoring a state.
//! Checkpoint decoding alone never creates a controller closure witness.

use super::*;
use serde::{Deserialize, Serialize};

/// Latest descriptive interval or unqualified lower bound for a retained group.
/// Generation zero is unknown legacy ancestry. Passive imports extending a closed
/// group retain the last known interval provisionally; they cannot invent the
/// original producer's renewal identity or export a new producer claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct ExecutionInterval {
    generation: u64,
    /// Reuses the strict UUID codec; this is not a model-block occurrence.
    identity: Option<ContextBlockOccurrenceId>,
    provisional: bool,
}

impl ExecutionInterval {
    pub(in super::super) fn unknown() -> Self {
        Self {
            generation: 0,
            identity: None,
            provisional: true,
        }
    }

    pub(in super::super) fn unqualified_after(previous: Option<&Self>) -> Self {
        let mut interval = previous.cloned().unwrap_or_else(Self::unknown);
        interval.provisional = true;
        interval
    }

    /// A closed interval with unknown ancestry cannot safely choose a renewal
    /// number. Its original source must be qualified before new work can reuse it.
    pub(in super::super) fn after(previous: Option<&Self>) -> AgentContextResult<Self> {
        if previous.is_some_and(|previous| !previous.known()) {
            return Err(invalid());
        }
        Ok(Self {
            generation: previous
                .map_or(Some(1), |previous| previous.generation.checked_add(1))
                .ok_or_else(invalid)?,
            identity: Some(ContextBlockOccurrenceId::allocate()),
            provisional: false,
        })
    }

    pub(in super::super) fn known(&self) -> bool {
        self.identity.is_some() && !self.provisional
    }

    pub(in super::super) fn validate(&self) -> AgentContextResult<()> {
        if (self.generation == 0) != self.identity.is_none()
            || !self.provisional && self.identity.is_none()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn extends(&self, previous: &Self) -> bool {
        previous.identity.is_none()
            || self == previous
            || self.provisional
                && self.generation == previous.generation
                && self.identity == previous.identity
            || self.generation > previous.generation && self.identity != previous.identity
    }
}

/// Original producer's descriptive source state, never a live completion handle.
/// Decoders must call `validate`; adapters must also qualify original System-row
/// custody, exact group framing and physical source order before restoration.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayExecutionState {
    group: String,
    interval: ExecutionInterval,
    pending: bool,
    members: Vec<(ContextBlockOccurrenceId, u64)>,
}

impl ReplayExecutionState {
    /// Original causal group spelling; it grants no execution or effect rights.
    pub fn group(&self) -> &str {
        &self.group
    }
    /// Original immutable occurrence/sequence pairs in group order.
    pub fn members(&self) -> &[(ContextBlockOccurrenceId, u64)] {
        &self.members
    }
    /// Whether the original producer retained obligations at this frontier.
    pub fn is_pending(&self) -> bool {
        self.pending
    }
    /// Monotonic original interval number, separate from epoch and dispatch IDs.
    pub fn generation(&self) -> u64 {
        self.interval.generation
    }

    /// Checks bounded metadata only. This does not qualify source or retirement.
    pub fn validate(&self) -> AgentContextResult<()> {
        ContextExecutionGroupId::new(&self.group)?;
        self.interval.validate()?;
        if self.group.len() > 4096
            || !self.interval.known()
            || self.members.is_empty()
            || self.members.len() > 65536
        {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        let mut sequence = 0;
        for (id, next) in &self.members {
            if !ids.insert(*id) || *next <= sequence {
                return Err(invalid());
            }
            sequence = *next;
        }
        Ok(())
    }
}

impl std::fmt::Debug for ReplayExecutionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayExecutionState")
            .field("generation", &self.generation())
            .field("pending", &self.pending)
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}

impl ReplayEpochContext {
    /// Captures actual pending intervals and locally witnessed closures. A cold
    /// descriptive closed flag cannot be re-exported as original producer proof.
    /// Imported groups with unknown intervals remain protected and emit no claim.
    pub fn execution_states(&self) -> AgentContextResult<Vec<ReplayExecutionState>> {
        self.validate_membership()?;
        let mut groups = BTreeMap::<ContextExecutionGroupId, Vec<_>>::new();
        for event in self.context.chronology() {
            if let Some(group) = event.execution_group_id() {
                groups
                    .entry(group.clone())
                    .or_default()
                    .push((event.record().occurrence_id(), event.sequence().get()));
            }
        }
        let mut states = Vec::new();
        for (group, members) in groups {
            let Some(interval) = self
                .context
                .execution_intervals
                .get(&group)
                .filter(|interval| interval.known())
            else {
                continue;
            };
            let pending = self.context.execution_group_is_pending(&group);
            if !pending
                && self
                    .source_lifecycle
                    .closed_groups
                    .get(&group)
                    .is_none_or(|closure| {
                        Some(closure.frontier.get())
                            != members.last().map(|(_, sequence)| *sequence)
                            || closure.protection.interval() != interval
                    })
            {
                continue;
            }
            let state = ReplayExecutionState {
                group: group.as_str().into(),
                interval: interval.clone(),
                pending,
                members,
            };
            state.validate()?;
            states.push(state);
        }
        Ok(states)
    }

    /// Restores independently qualified original producer lifecycle metadata.
    /// The adapter must verify original authored row/escrow custody and control
    /// ordering first; this pure port cannot establish persistence provenance.
    /// Exact member IDs/sequences and interval ordering are checked again here.
    /// Older frontiers/intervals are inert. Conflicting equal generations refuse.
    /// Closing requires complete native pairing and creates only a source proof,
    /// never an original live handle, effect result or ordinary append ACK.
    /// Returns true for changed descriptive state, false for stale/idempotent input.
    pub fn restore_execution_state(
        &mut self,
        state: &ReplayExecutionState,
    ) -> AgentContextResult<bool> {
        state.validate()?;
        let group = ContextExecutionGroupId::new(state.group())?;
        let members = self
            .context
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&group))
            .map(|event| (event.record().occurrence_id(), event.sequence().get()))
            .collect::<Vec<_>>();
        if !members.starts_with(state.members()) {
            return Err(invalid());
        }
        if members.len() != state.members.len() {
            return Ok(false);
        }
        let previous = self
            .context
            .execution_intervals
            .get(&group)
            .ok_or_else(invalid)?;
        let pending = self.context.execution_group_is_pending(&group);
        if !previous.known() && !pending && state.pending {
            // Unknown closed ancestry cannot distinguish an earlier pending
            // record from a real renewal. Qualify its closed interval first.
            return Err(invalid());
        }
        if previous.identity.is_some() {
            if state.interval.generation < previous.generation
                || previous.provisional && state.interval.generation == previous.generation
            {
                return Ok(false);
            }
            if !state.interval.extends(previous) {
                return Err(invalid());
            }
        }
        if &state.interval == previous && (!pending || state.pending) {
            return Ok(false);
        }
        let mut candidate = self.clone();
        if &state.interval != previous {
            candidate.source_lifecycle.closed_groups.remove(&group);
            candidate
                .context
                .execution_intervals
                .insert(group.clone(), state.interval.clone());
            if pending || state.pending {
                let protection = if !previous.known() && pending {
                    self.context
                        .pending_execution_groups
                        .get(&group)
                        .ok_or_else(invalid)?
                        .with_interval(state.interval.clone())
                } else {
                    ContextExecutionProtection::new(state.interval.clone())
                };
                candidate
                    .context
                    .pending_execution_groups
                    .insert(group.clone(), protection);
            }
        }
        if state.pending {
            candidate.refresh_accounting_group(&group)?;
        } else if candidate.context.execution_group_is_pending(&group) {
            let frontier = candidate.group_frontier(&group).ok_or_else(invalid)?;
            let receipt = candidate.capture_execution_owner(&group, frontier)?;
            candidate.finish_execution_group(&receipt)?;
        }
        candidate.validate_membership()?;
        *self = candidate;
        Ok(true)
    }
}

fn invalid() -> AgentContextError {
    AgentContextError::new("execution lifecycle source is not qualified")
}
