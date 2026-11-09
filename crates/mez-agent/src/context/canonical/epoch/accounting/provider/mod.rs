//! One owned provider/API/model estimate view over canonical occurrences.
//!
//! Source bytes and estimates remain on canonical blocks. This view retains
//! only costs, membership and qualified native groups. New source refreshes its
//! own group (including earlier neutral/native representations); lifecycle-only
//! changes update eligibility without parsing groups. A provider/model switch
//! replaces this one view from owned caches, never retokenizes source or starts
//! provider work. Fixed system/schema/wire overhead and billing stay separate.

use super::*;
use crate::context_compaction::{
    ProviderBudgetProjection, projected_context_metadata_input_tokens,
};
use crate::{ProviderApiCompatibility, ProviderContinuityOwner};

/// Exact configured projection, separate from a context occurrence or execution
/// authority. Model changes invalidate the view even though the current local
/// byte-based counting method itself is model-independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayProviderProjection {
    owner: ProviderContinuityOwner,
    model: String,
}
impl ReplayProviderProjection {
    /// Captures a nonempty, trimmed, control-free provider/model descriptor.
    /// Invalid descriptors return a content-free error without selecting a view.
    pub fn new(
        api: ProviderApiCompatibility,
        provider_id: impl Into<String>,
        model: impl Into<String>,
    ) -> AgentContextResult<Self> {
        let owner = ProviderContinuityOwner::new(api, provider_id).ok_or_else(invalid)?;
        let model = model.into();
        if model.is_empty() || model.trim() != model || model.chars().any(char::is_control) {
            return Err(invalid());
        }
        Ok(Self { owner, model })
    }
    /// Returns the configured request API, not a native execution capability.
    pub fn api(&self) -> ProviderApiCompatibility {
        self.owner.api()
    }
    /// Returns the exact configured provider namespace.
    pub fn provider_id(&self) -> &str {
        self.owner.provider_id()
    }
    /// Returns the configured model whose projection was selected.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Borrows the existing provider visibility policy without inventing a
    /// second native/neutral accounting rule.
    fn budget_projection(&self) -> ProviderBudgetProjection<'_> {
        ProviderBudgetProjection::new(self.api(), self.provider_id())
    }
}

/// Disjoint owned membership; eligible raw cost is a subset, not another charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stable,
    Head,
    Raw,
}

/// Descriptive scalar projection for one actual canonical occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    cost: usize,
    eligible: bool,
}

/// A single bounded-by-context view, never a global or per-model history cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplayProviderAccounting {
    projection: ReplayProviderProjection,
    entries: BTreeMap<ContextBlockOccurrenceId, Entry>,
    complete_groups: BTreeSet<ContextExecutionGroupId>,
    totals: ReplayEpochInputEstimates,
}
impl ReplayProviderAccounting {
    /// Applies a checked delta to one disjoint aggregate inside a candidate.
    fn adjust(&mut self, entry: Entry, add: bool) -> AgentContextResult<()> {
        let value = match entry.kind {
            Kind::Stable => &mut self.totals.stable,
            Kind::Head => &mut self.totals.summary,
            Kind::Raw => &mut self.totals.raw,
        };
        *value = if add {
            value.checked_add(entry.cost)
        } else {
            value.checked_sub(entry.cost)
        }
        .ok_or_else(invalid)?;
        if entry.kind == Kind::Raw && entry.eligible {
            self.totals.eligible = if add {
                self.totals.eligible.checked_add(entry.cost)
            } else {
                self.totals.eligible.checked_sub(entry.cost)
            }
            .ok_or_else(invalid)?;
        }
        if self.totals.eligible > self.totals.raw {
            return Err(invalid());
        }
        self.totals.total_input_tokens()?;
        Ok(())
    }
    /// Replaces only the supplied occurrence, preserving its membership class.
    fn put(&mut self, id: ContextBlockOccurrenceId, entry: Entry) -> AgentContextResult<()> {
        if let Some(previous) = self.entries.remove(&id) {
            if previous.kind != entry.kind {
                return Err(invalid());
            }
            self.adjust(previous, false)?;
        }
        self.adjust(entry, true)?;
        self.entries.insert(id, entry);
        Ok(())
    }
    /// Removes one exact selected occurrence or superseded stable/head member.
    fn remove(&mut self, id: ContextBlockOccurrenceId, kind: Kind) -> AgentContextResult<()> {
        let entry = self.entries.remove(&id).ok_or_else(invalid)?;
        if entry.kind != kind {
            return Err(invalid());
        }
        self.adjust(entry, false)
    }
    /// Lifecycle retirement affects eligibility only, without requalifying or
    /// parsing immutable native payloads.
    pub(super) fn set_eligible(
        &mut self,
        id: ContextBlockOccurrenceId,
        eligible: bool,
    ) -> AgentContextResult<()> {
        let mut entry = *self.entries.get(&id).ok_or_else(invalid)?;
        if entry.kind != Kind::Raw {
            return Err(invalid());
        }
        if entry.eligible == eligible {
            return Ok(());
        }
        entry.eligible = eligible;
        self.put(id, entry)
    }
}

impl ReplayEpochContext {
    /// Selects one provider/API/model view atomically. Selecting the same key is
    /// a cache hit. Changed API/provider selection reuses block estimates and
    /// requalifies native groups; a model-only change retains currently model-
    /// independent costs. Source, usage and task owners never change.
    pub fn select_provider_projection(
        &mut self,
        projection: ReplayProviderProjection,
    ) -> AgentContextResult<()> {
        if self.provider_projection() == Some(&projection) {
            return Ok(());
        }
        self.validate_membership()?;
        // Current byte estimates and native compatibility depend on API/provider,
        // not model spelling. Keep those valid costs while changing the frozen
        // descriptor; a future model-specific counting method must revisit this.
        if let Some(view) = self.accounting.provider.as_mut()
            && view.projection.owner == projection.owner
        {
            view.projection = projection;
            return Ok(());
        }
        let mut view = ReplayProviderAccounting {
            projection,
            entries: BTreeMap::new(),
            complete_groups: BTreeSet::new(),
            totals: ReplayEpochInputEstimates::default(),
        };
        for group in self.accounting.groups.keys() {
            let indices = self.provider_group_indices(group)?;
            if self.qualify_provider_group(&indices, &view.projection)? {
                view.complete_groups.insert(group.clone());
            }
        }
        for index in 0..self.context.blocks().len() {
            let (id, entry) = self.projected_entry(index, &view)?;
            view.put(id, entry)?;
        }
        self.validate_provider_view(&view)?;
        self.accounting.provider = Some(view);
        Ok(())
    }

    /// Returns the current owned descriptor, or none for ordinary framing.
    pub fn provider_projection(&self) -> Option<&ReplayProviderProjection> {
        self.accounting
            .provider
            .as_ref()
            .map(|view| &view.projection)
    }
    /// Returns the selected provider's cached totals without visiting source.
    /// None denotes ordinary-framing-only ownership, not a zero-cost request.
    pub fn provider_input_estimates(&self) -> Option<ReplayEpochInputEstimates> {
        self.accounting.provider.as_ref().map(|view| view.totals)
    }
    /// Returns a cached selected-view cost for one frozen source member.
    pub(in crate::context::canonical::epoch) fn planning_input_tokens(
        &self,
        event: &ConversationEvent,
    ) -> AgentContextResult<usize> {
        match &self.accounting.provider {
            Some(view) => view
                .entries
                .get(&event.record().occurrence_id())
                .map(|entry| entry.cost)
                .ok_or_else(invalid),
            None => Ok(event.record().estimated_input_tokens()),
        }
    }

    /// Resolves only the group's indexed original members, in actual chronology.
    /// Sparse local sequence values do not establish adjacency by themselves.
    fn provider_group_indices(
        &self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<Vec<usize>> {
        let ids = self.accounting.groups.get(group).ok_or_else(invalid)?;
        let mut indices = ids
            .iter()
            .map(|id| {
                let sequence = self.accounting.raw.get(id).ok_or_else(invalid)?.sequence;
                self.context
                    .chronology()
                    .binary_search_by_key(&sequence, ConversationEvent::sequence)
                    .map_err(|_| invalid())
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        indices.sort_unstable();
        if indices.is_empty() {
            return Err(invalid());
        }
        Ok(indices)
    }
    /// Uses the request renderer's existing completeness policy after checking
    /// actual adjacency. Unrelated chronology between members forces neutrality.
    fn qualify_provider_group(
        &self,
        indices: &[usize],
        projection: &ReplayProviderProjection,
    ) -> AgentContextResult<bool> {
        #[cfg(test)]
        GROUP_QUALIFICATIONS.with(|count| count.set(count.get() + 1));
        let first = indices.first().copied().ok_or_else(invalid)?;
        let last = indices.last().copied().ok_or_else(invalid)?;
        if last.checked_sub(first).and_then(|n| n.checked_add(1)) != Some(indices.len()) {
            return Ok(false);
        }
        let prefix = self.context.stable_slots().len();
        let events = indices
            .iter()
            .map(|index| {
                let index = prefix.checked_add(*index).ok_or_else(invalid)?;
                Ok((
                    self.context.blocks().get(index).ok_or_else(invalid)?,
                    self.context.metadata_for_block(index).ok_or_else(invalid)?,
                ))
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        Ok(crate::context_assembly::provider_native_group_is_complete(
            &events,
            Some(projection.api()),
            projection.provider_id(),
        ))
    }
    /// Projects existing immutable metadata under one qualified native-group set.
    /// No text hashing/counting or vendor measurements occur here.
    fn projected_entry(
        &self,
        index: usize,
        view: &ReplayProviderAccounting,
    ) -> AgentContextResult<(ContextBlockOccurrenceId, Entry)> {
        let block = self.context.blocks().get(index).ok_or_else(invalid)?;
        let metadata = self.context.metadata_for_block(index).ok_or_else(invalid)?;
        let id = metadata.record().occurrence_id();
        let prefix = self.context.stable_slots().len();
        let kind = if index < prefix {
            Kind::Stable
        } else if index == prefix && self.head.is_some() {
            Kind::Head
        } else {
            Kind::Raw
        };
        let eligible = kind == Kind::Raw
            && self.event_is_replaceable(
                self.context
                    .chronology()
                    .get(index - prefix)
                    .ok_or_else(invalid)?,
            );
        let complete = metadata
            .execution_group_id()
            .is_some_and(|group| view.complete_groups.contains(group));
        let cost = projected_context_metadata_input_tokens(
            block.source,
            metadata,
            Some(view.projection.budget_projection()),
            complete,
        );
        Ok((
            id,
            Entry {
                kind,
                cost,
                eligible,
            },
        ))
    }

    /// Temporarily moves a view inside an isolated candidate to avoid cloning or
    /// retaining source. Failure discards that candidate; no partial view escapes.
    fn update_provider_view(
        &mut self,
        update: impl FnOnce(&Self, &mut ReplayProviderAccounting) -> AgentContextResult<()>,
    ) -> AgentContextResult<()> {
        let Some(mut view) = self.accounting.provider.take() else {
            return Ok(());
        };
        update(self, &mut view)?;
        self.accounting.provider = Some(view);
        Ok(())
    }
    /// New source refreshes only touched groups and ungrouped occurrences. Earlier
    /// neutral/native rows in a newly complete or interleaved group change too.
    pub(super) fn refresh_provider_arrivals(
        &mut self,
        sequences: &[ContextEventSequence],
    ) -> AgentContextResult<()> {
        self.update_provider_view(|owner, view| {
            let mut groups = BTreeSet::new();
            for sequence in sequences {
                let index = owner
                    .context
                    .chronology()
                    .binary_search_by_key(sequence, ConversationEvent::sequence)
                    .map_err(|_| invalid())?;
                let event = &owner.context.chronology()[index];
                if let Some(group) = event.execution_group_id() {
                    groups.insert(group.clone());
                } else {
                    let index = owner
                        .context
                        .stable_slots()
                        .len()
                        .checked_add(index)
                        .ok_or_else(invalid)?;
                    let (id, entry) = owner.projected_entry(index, view)?;
                    view.put(id, entry)?;
                }
            }
            for group in groups {
                let indices = owner.provider_group_indices(&group)?;
                if owner.qualify_provider_group(&indices, &view.projection)? {
                    view.complete_groups.insert(group.clone());
                } else {
                    view.complete_groups.remove(&group);
                }
                for index in indices {
                    let index = owner
                        .context
                        .stable_slots()
                        .len()
                        .checked_add(index)
                        .ok_or_else(invalid)?;
                    let (id, entry) = owner.projected_entry(index, view)?;
                    view.put(id, entry)?;
                }
            }
            Ok(())
        })
    }
    /// Changes only actual stable-prefix members; raw source costs stay cached.
    pub(super) fn refresh_provider_stable(
        &mut self,
        old_ids: &[ContextBlockOccurrenceId],
    ) -> AgentContextResult<()> {
        self.update_provider_view(|owner, view| {
            for id in old_ids {
                view.remove(*id, Kind::Stable)?;
            }
            for index in 0..owner.context.stable_slots().len() {
                let (id, entry) = owner.projected_entry(index, view)?;
                view.put(id, entry)?;
            }
            Ok(())
        })
    }
    /// Exchanges exact selected members and the head without reprojecting tails.
    pub(super) fn publish_provider_accounting(
        &mut self,
        plan: &ReplayCompactionPlan,
        old_head: Option<ContextBlockOccurrenceId>,
    ) -> AgentContextResult<()> {
        self.update_provider_view(|owner, view| {
            for member in plan.removed_raw_members() {
                view.remove(member.occurrence_id(), Kind::Raw)?;
            }
            if let Some(id) = old_head {
                view.remove(id, Kind::Head)?;
            }
            if owner.head.is_some() {
                let (id, entry) =
                    owner.projected_entry(owner.context.stable_slots().len(), view)?;
                view.put(id, entry)?;
            }
            for event in plan.removed_events() {
                if let Some(group) = event.execution_group_id()
                    && !owner.accounting.groups.contains_key(group)
                {
                    view.complete_groups.remove(group);
                }
            }
            Ok(())
        })
    }
    /// Validates associations and cached visibility without parsing native source
    /// or resumming totals. The view has no unchecked restoration/mutable escape.
    pub(super) fn validate_provider_accounting(&self) -> AgentContextResult<()> {
        match &self.accounting.provider {
            Some(view) => self.validate_provider_view(view),
            None => Ok(()),
        }
    }
    /// Checks a fully built local candidate before replacing the active view,
    /// without cloning or replacing any canonical source bytes.
    fn validate_provider_view(&self, view: &ReplayProviderAccounting) -> AgentContextResult<()> {
        if view.entries.len() != self.context.blocks().len()
            || view.totals.eligible > view.totals.raw
            || view
                .complete_groups
                .iter()
                .any(|group| !self.accounting.groups.contains_key(group))
        {
            return Err(invalid());
        }
        for index in 0..self.context.blocks().len() {
            let (id, expected) = self.projected_entry(index, view)?;
            if view.entries.get(&id) != Some(&expected) {
                return Err(invalid());
            }
        }
        let head_cost = self
            .head
            .as_ref()
            .map(|head| {
                view.entries
                    .get(&head.occurrence_id())
                    .map(|entry| entry.cost)
                    .ok_or_else(invalid)
            })
            .transpose()?
            .unwrap_or(0);
        if view.totals.summary != head_cost {
            return Err(invalid());
        }
        view.totals.total_input_tokens()?;
        Ok(())
    }
}

/// Keeps projection/descriptor failures free of private source and labels.
fn invalid() -> AgentContextError {
    AgentContextError::new("replay provider estimate projection is invalid or exhausted")
}

#[cfg(test)]
thread_local! {
    /// Current-thread instrumentation only; production has no global cache/counter.
    static GROUP_QUALIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
