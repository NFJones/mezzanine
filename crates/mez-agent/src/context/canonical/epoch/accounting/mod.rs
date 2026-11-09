//! Incremental occurrence membership and checked, unmeasured framed estimates.
//!
//! This index owns no source text. Admission, lifecycle changes and publication
//! update only affected membership; projection rebuilds do not rebuild totals.
//! Native group indices permit targeted eligibility changes on reopen/closure.
//! These ordinary-framing totals are independent of provider-specific native
//! visibility and fixed request overhead, and never represent incurred usage.

use super::*;

mod provider;
pub use provider::ReplayProviderProjection;

/// Cached disjoint estimates excluding fixed request instructions/schemas/wrappers.
/// The owning accessor identifies ordinary framing or a selected provider-native
/// view. Eligible source is a subset of raw membership, not another charge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayEpochInputEstimates {
    stable: usize,
    summary: usize,
    raw: usize,
    eligible: usize,
}
impl ReplayEpochInputEstimates {
    /// Returns the current exact stable-prefix estimate.
    pub fn stable_input_tokens(self) -> usize {
        self.stable
    }
    /// Returns the sole summary head estimate, or zero without a head.
    pub fn summary_input_tokens(self) -> usize {
        self.summary
    }
    /// Returns all current raw members, including protected and older-born tail.
    pub fn raw_input_tokens(self) -> usize {
        self.raw
    }
    /// Returns the replaceable subset of current raw membership.
    /// This descriptive total does not grant contiguous-prefix eligibility.
    pub fn eligible_raw_input_tokens(self) -> usize {
        self.eligible
    }
    /// Adds the disjoint stable/head/raw estimates with checked arithmetic.
    /// Corrupt or exhausted aggregates return a content-free error.
    pub fn total_input_tokens(self) -> AgentContextResult<usize> {
        self.stable
            .checked_add(self.summary)
            .and_then(|n| n.checked_add(self.raw))
            .ok_or_else(invalid)
    }
}

/// Frozen selected/head and retained-raw estimates for one actual source plan.
/// Stable authority and fixed request overhead stay outside source. These costs
/// are descriptive, never incurred usage or evidence that a model was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayCompactionInputEstimates {
    pub(super) selected_raw: usize,
    pub(super) prior_summary: usize,
    pub(super) retained_raw: usize,
}
impl ReplayCompactionInputEstimates {
    /// Returns selected original raw members, excluding the previous head.
    pub fn selected_raw_input_tokens(self) -> usize {
        self.selected_raw
    }
    /// Returns the prior head removed by publication. Summary mode supplies it
    /// once to the compactor; intentional omission makes no model request.
    pub fn prior_summary_input_tokens(self) -> usize {
        self.prior_summary
    }
    /// Returns exact retained frozen raw input, including protected obligations.
    pub fn retained_raw_input_tokens(self) -> usize {
        self.retained_raw
    }
    /// Returns the selected source's replay-view estimate, failing on overflow.
    /// Auxiliary semantic rendering and fixed request overhead are separate.
    pub fn source_input_tokens(self) -> AgentContextResult<usize> {
        self.selected_raw
            .checked_add(self.prior_summary)
            .ok_or_else(invalid)
    }
}

/// One raw origin's cached association and current eligibility, without text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawEntry {
    sequence: ContextEventSequence,
    group: Option<ContextExecutionGroupId>,
    cost: usize,
    eligible: bool,
}

/// Owned membership index; methods operate inside an isolated epoch candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ReplayAccounting {
    stable: BTreeMap<ContextBlockOccurrenceId, usize>,
    summary: Option<(ContextBlockOccurrenceId, usize)>,
    raw: BTreeMap<ContextBlockOccurrenceId, RawEntry>,
    groups: BTreeMap<ContextExecutionGroupId, BTreeSet<ContextBlockOccurrenceId>>,
    totals: ReplayEpochInputEstimates,
    provider: Option<provider::ReplayProviderAccounting>,
}
impl ReplayAccounting {
    /// Adds exactly one actual committed raw occurrence. Duplicate origin or any
    /// aggregate overflow rejects the surrounding isolated owner candidate.
    fn add_raw(&mut self, event: &ConversationEvent, eligible: bool) -> AgentContextResult<()> {
        let id = event.record().occurrence_id();
        if self.raw.contains_key(&id) {
            return Err(invalid());
        }
        let cost = event.record().estimated_input_tokens();
        self.totals.raw = self.totals.raw.checked_add(cost).ok_or_else(invalid)?;
        if eligible {
            self.totals.eligible = self.totals.eligible.checked_add(cost).ok_or_else(invalid)?;
        }
        self.totals.total_input_tokens()?;
        if let Some(group) = event.execution_group_id() {
            self.groups.entry(group.clone()).or_default().insert(id);
        }
        self.raw.insert(
            id,
            RawEntry {
                sequence: event.sequence(),
                group: event.execution_group_id().cloned(),
                cost,
                eligible,
            },
        );
        Ok(())
    }

    /// Removes only a selected original member, preserving every retained index.
    fn remove_raw(&mut self, member: &ReplayEpochMember) -> AgentContextResult<()> {
        let entry = self.raw.remove(&member.occurrence_id).ok_or_else(invalid)?;
        if entry.sequence != member.origin {
            return Err(invalid());
        }
        self.totals.raw = self
            .totals
            .raw
            .checked_sub(entry.cost)
            .ok_or_else(invalid)?;
        if entry.eligible {
            self.totals.eligible = self
                .totals
                .eligible
                .checked_sub(entry.cost)
                .ok_or_else(invalid)?;
        }
        if let Some(group) = entry.group {
            let ids = self.groups.get_mut(&group).ok_or_else(invalid)?;
            if !ids.remove(&member.occurrence_id) {
                return Err(invalid());
            }
            if ids.is_empty() {
                self.groups.remove(&group);
            }
        }
        Ok(())
    }

    /// Changes only one member's eligibility; its cost and occurrence stay fixed.
    fn set_eligible(
        &mut self,
        id: ContextBlockOccurrenceId,
        eligible: bool,
    ) -> AgentContextResult<()> {
        let entry = self.raw.get_mut(&id).ok_or_else(invalid)?;
        if entry.eligible == eligible {
            return Ok(());
        }
        self.totals.eligible = if eligible {
            self.totals.eligible.checked_add(entry.cost)
        } else {
            self.totals.eligible.checked_sub(entry.cost)
        }
        .ok_or_else(invalid)?;
        if self.totals.eligible > self.totals.raw {
            return Err(invalid());
        }
        entry.eligible = eligible;
        Ok(())
    }

    /// Replaces only live stable membership, using already-owned cached costs.
    fn replace_stable(&mut self, slots: &[StableContextBlock]) -> AgentContextResult<()> {
        self.stable.clear();
        self.totals.stable = 0;
        for slot in slots {
            let cost = slot.record().estimated_input_tokens();
            if self
                .stable
                .insert(slot.record().occurrence_id(), cost)
                .is_some()
            {
                return Err(invalid());
            }
            self.totals.stable = self.totals.stable.checked_add(cost).ok_or_else(invalid)?;
        }
        self.totals.total_input_tokens()?;
        Ok(())
    }

    /// Adopts a new summary's own origin, never the covered ordering anchor.
    fn replace_summary(&mut self, event: Option<&ConversationEvent>) -> AgentContextResult<()> {
        self.summary = event.map(|event| {
            (
                event.record().occurrence_id(),
                event.record().estimated_input_tokens(),
            )
        });
        self.totals.summary = self.summary.map_or(0, |(_, cost)| cost);
        self.totals.total_input_tokens()?;
        Ok(())
    }
}

impl ReplayEpochContext {
    /// Returns cached ordinary-framing totals without visiting source text.
    pub fn input_estimates(&self) -> ReplayEpochInputEstimates {
        self.accounting.totals
    }

    /// Builds the index once from an initial or restored canonical epoch.
    /// A restored head belongs only to the summary partition, never raw source.
    pub(super) fn initialize_accounting(&mut self) -> AgentContextResult<()> {
        let mut accounting = ReplayAccounting::default();
        accounting.replace_stable(self.context.stable_slots())?;
        let offset = usize::from(self.head.is_some());
        accounting.replace_summary(self.context.chronology().first().filter(|_| offset == 1))?;
        for event in self.context.chronology().iter().skip(offset) {
            accounting.add_raw(event, self.event_is_replaceable(event))?;
        }
        self.accounting = accounting;
        Ok(())
    }

    /// Records new exact canonical members after successful batch admission.
    pub(super) fn record_arrivals(
        &mut self,
        sequences: &[ContextEventSequence],
    ) -> AgentContextResult<()> {
        for sequence in sequences {
            let event = self
                .context
                .chronology()
                .binary_search_by_key(sequence, ConversationEvent::sequence)
                .ok()
                .and_then(|index| self.context.chronology().get(index))
                .ok_or_else(invalid)?;
            let eligible = self.event_is_replaceable(event);
            self.accounting.add_raw(event, eligible)?;
            self.members.push(ReplayEpochMember {
                origin: *sequence,
                occurrence_id: event.record().occurrence_id(),
                born_in: self.revision,
            });
        }
        self.refresh_provider_arrivals(sequences)
    }

    /// Refreshes only exact task/peer source positions after actual retirement.
    pub(super) fn refresh_accounting_sequences(
        &mut self,
        sequences: impl IntoIterator<Item = ContextEventSequence>,
    ) -> AgentContextResult<()> {
        for sequence in sequences {
            let event = self
                .context
                .chronology()
                .binary_search_by_key(&sequence, ConversationEvent::sequence)
                .ok()
                .and_then(|index| self.context.chronology().get(index))
                .ok_or_else(invalid)?;
            let eligible = self.event_is_replaceable(event);
            self.accounting
                .set_eligible(event.record().occurrence_id(), eligible)?;
            if let Some(provider) = self.accounting.provider.as_mut() {
                provider.set_eligible(event.record().occurrence_id(), eligible)?;
            }
        }
        Ok(())
    }

    /// Reopening or closing one group changes only its indexed raw members.
    pub(super) fn refresh_accounting_group(
        &mut self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<()> {
        let sequences = self
            .accounting
            .groups
            .get(group)
            .into_iter()
            .flatten()
            .map(|id| {
                self.accounting
                    .raw
                    .get(id)
                    .map(|entry| entry.sequence)
                    .ok_or_else(invalid)
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        self.refresh_accounting_sequences(sequences)
    }

    /// Refreshes stable totals after the actual trusted slot replacement.
    pub(super) fn refresh_accounting_stable(&mut self) -> AgentContextResult<()> {
        let old_ids = self.accounting.stable.keys().copied().collect::<Vec<_>>();
        self.accounting
            .replace_stable(self.context.stable_slots())?;
        self.refresh_provider_stable(&old_ids)
    }

    /// Removes selected originals and exchanges the sole head in one candidate.
    pub(super) fn publish_accounting(
        &mut self,
        plan: &ReplayCompactionPlan,
    ) -> AgentContextResult<()> {
        let old_head = self.accounting.summary.map(|(id, _)| id);
        for member in plan.removed_raw_members() {
            self.accounting.remove_raw(member)?;
        }
        self.accounting.replace_summary(
            self.head
                .as_ref()
                .and_then(|_| self.context.chronology().first()),
        )?;
        self.publish_provider_accounting(plan, old_head)
    }

    /// Checks cached association/index integrity using immutable metadata only.
    /// Totals have no unchecked restoration/mutable escape and change only via
    /// checked membership operations. Validate disjoint bounds without resumming
    /// history; the independent test oracle verifies arithmetic equality.
    pub(super) fn validate_accounting(&self) -> AgentContextResult<()> {
        for slot in self.context.stable_slots() {
            let cost = slot.record().estimated_input_tokens();
            if self.accounting.stable.get(&slot.record().occurrence_id()) != Some(&cost) {
                return Err(invalid());
            }
        }
        let offset = usize::from(self.head.is_some());
        let head = self
            .context
            .chronology()
            .first()
            .filter(|_| offset == 1)
            .map(|event| {
                (
                    event.record().occurrence_id(),
                    event.record().estimated_input_tokens(),
                )
            });
        if self.accounting.summary != head
            || self.accounting.stable.len() != self.context.stable_slots().len()
            || self.accounting.raw.len() != self.members.len()
            || self.accounting.totals.summary != head.map_or(0, |(_, cost)| cost)
            || self.accounting.totals.eligible > self.accounting.totals.raw
        {
            return Err(invalid());
        }
        for event in self.context.chronology().iter().skip(offset) {
            let id = event.record().occurrence_id();
            let entry = self.accounting.raw.get(&id).ok_or_else(invalid)?;
            if entry.sequence != event.sequence()
                || entry.group.as_ref() != event.execution_group_id()
                || entry.cost != event.record().estimated_input_tokens()
                || entry.eligible != self.event_is_replaceable(event)
            {
                return Err(invalid());
            }
            if let Some(group) = event.execution_group_id()
                && !self
                    .accounting
                    .groups
                    .get(group)
                    .is_some_and(|ids| ids.contains(&id))
            {
                return Err(invalid());
            }
        }
        if self.accounting.groups.iter().any(|(group, ids)| {
            ids.is_empty()
                || ids.iter().any(|id| {
                    self.accounting
                        .raw
                        .get(id)
                        .is_none_or(|entry| entry.group.as_ref() != Some(group))
                })
        }) {
            return Err(invalid());
        }
        self.accounting.totals.total_input_tokens()?;
        self.validate_provider_accounting()
    }
}

/// Accounting diagnostics contain no source or opaque owner labels.
fn invalid() -> AgentContextError {
    AgentContextError::new("replay occurrence accounting is inconsistent or exhausted")
}

#[cfg(test)]
mod tests;
