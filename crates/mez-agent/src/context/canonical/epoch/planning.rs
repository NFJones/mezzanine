//! Shared frozen-prefix/raw-tail policy. No trigger-specific retry ladder or
//! advisory summary-size target participates in source eligibility.

use super::*;

impl ReplayEpochContext {
    /// Handles an authoritative ordinary input rejection in raw mode. Normal
    /// retention gets the first opportunity to omit source. If it would keep
    /// every optional unit, descend by one complete unit instead. The frontier
    /// derives only from current membership, so rejected retries terminate and
    /// never resurrect covered source or require another persistent ledger.
    /// Callers must not use this stronger policy for estimates or manual work.
    pub fn prepare_raw_compaction_after_input_rejection(
        &self,
        policy: ReplayTailPolicy,
    ) -> AgentContextResult<ReplayCompactionPreparation> {
        if policy.head_policy != Some(ReplayHeadPolicy::IntentionalOmission) {
            return Err(AgentContextError::new(
                "raw rejection requires an explicit omission policy",
            ));
        }
        let prepared = self.prepare_compaction(policy)?;
        if matches!(&prepared, ReplayCompactionPreparation::Ready(plan) if plan.removed_raw_members().next().is_some())
        {
            return Ok(prepared);
        }
        if matches!(
            prepared,
            ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::ProtectedInputExceedsBudget
            )
        ) {
            return Ok(prepared);
        }
        let raw = &self.context.chronology()[usize::from(self.head.is_some())..];
        let optional = execution_units(raw)
            .into_iter()
            .filter(|range| {
                raw[range.clone()]
                    .iter()
                    .all(|event| self.event_is_replaceable(event))
            })
            .count();
        if optional == 0 {
            return Ok(prepared);
        }
        self.prepare_compaction(
            policy.with_raw_tail_limits(policy.fixed_input_tokens, Some(optional - 1)),
        )
    }

    /// Freezes one source selection under the captured head policy, or the
    /// committed policy when no explicit target was supplied. Summary selection
    /// uses a closed prefix and raw tail; exact instructions, pending peer/native
    /// groups and unrecoverable evidence push that prefix boundary earlier.
    /// Intentional omission retains mandatory whole units independently of one
    /// optional suffix, after reserving stable/protected input and fixed framing.
    /// A selected provider view supplies cached visibility-aware costs and is
    /// frozen with the plan; ordinary framing is used when no view is selected.
    /// Empty/irreducible source returns NoWork without creating model activity.
    /// Only summary policy forces an oldest closed prefix unit when the tail
    /// reservation fits everything. Unchanged raw retention is explicit NoWork.
    pub fn prepare_compaction(
        &self,
        policy: ReplayTailPolicy,
    ) -> AgentContextResult<ReplayCompactionPreparation> {
        self.validate_membership()?;
        if policy.head_policy.unwrap_or(self.policy) == ReplayHeadPolicy::IntentionalOmission {
            return self.prepare_raw_tail(policy);
        }
        if policy.fixed_input_tokens != 0 || policy.optional_unit_limit.is_some() {
            return Err(AgentContextError::new(
                "raw limits require intentional omission",
            ));
        }
        if self.members.is_empty() {
            return Ok(ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::NoRawMembers,
            ));
        }
        let offset = usize::from(self.head.is_some());
        let raw = &self.context.chronology()[offset..];
        let units = execution_units(raw);
        let protected = self.closed_prefix_unit_count(raw, &units);
        let mut unit_totals = Vec::with_capacity(units.len() + 1);
        unit_totals.push(0usize);
        for unit in &units {
            let cost = raw[unit.clone()].iter().try_fold(0usize, |sum, event| {
                sum.checked_add(self.planning_input_tokens(event)?)
                    .ok_or_else(|| AgentContextError::new("replay unit estimate exhausted"))
            })?;
            let total = unit_totals
                .last()
                .copied()
                .unwrap_or(0)
                .checked_add(cost)
                .ok_or_else(|| AgentContextError::new("replay source estimate exhausted"))?;
            unit_totals.push(total);
        }
        let estimates = self
            .provider_input_estimates()
            .unwrap_or(self.input_estimates());
        if unit_totals.last().copied() != Some(estimates.raw_input_tokens()) {
            return Err(AgentContextError::new("replay source aggregate drifted"));
        }
        let target = policy.retained_raw_tokens();
        let mut selected = units.len();
        let mut retained = 0usize;
        for index in (0..units.len()).rev() {
            let cost = unit_totals[index + 1]
                .checked_sub(unit_totals[index])
                .ok_or_else(|| AgentContextError::new("replay unit estimate drifted"))?;
            let total = retained
                .checked_add(cost)
                .ok_or_else(|| AgentContextError::new("replay retained estimate exhausted"))?;
            if total > target {
                break;
            }
            retained = total;
            selected = index;
        }
        // Protected source is mandatory raw tail, even if it exceeds optional
        // retention. Never skip its unit to gather later otherwise eligible rows.
        selected = selected.min(protected);
        if selected == 0 {
            if protected == 0 {
                return Ok(ReplayCompactionPreparation::NoWork(
                    ReplayCompactionNoWork::ProtectedPrefix,
                ));
            }
            // A reservation that fits all source is not an eligibility cause
            // for no-work. Every trigger selects the oldest whole closed unit.
            selected = 1;
        }
        let cut = units
            .get(selected - 1)
            .ok_or_else(|| AgentContextError::new("replay unit selection is inconsistent"))?
            .end;
        let selected_raw = unit_totals[selected];
        let retained_raw = estimates
            .raw_input_tokens()
            .checked_sub(selected_raw)
            .ok_or_else(|| AgentContextError::new("replay selected estimate drifted"))?;
        let input_estimates = ReplayCompactionInputEstimates {
            selected_raw,
            prior_summary: estimates.summary_input_tokens(),
            retained_raw,
        };
        input_estimates.source_input_tokens()?;
        Ok(ReplayCompactionPreparation::Ready(ReplayCompactionPlan {
            owner_identity: self.owner_identity.clone(),
            conversation: self.conversation.clone(),
            parent: self.revision,
            source_policy: self.policy,
            policy: ReplayHeadPolicy::Summary,
            head: self.head.clone(),
            frozen: self.context.chronology().to_vec(),
            members: self.members.clone(),
            selection: Box::new(ReplayCompactionSelection::Prefix {
                members: cut,
                events: offset + cut,
            }),
            input_estimates,
            provider_projection: self.provider_projection().cloned().map(Box::new),
            reentry: Box::new(ReplayCompactionReentryInput::capture(
                policy,
                self.provider_projection(),
                &raw[cut..units.get(protected).map_or(raw.len(), |unit| unit.start)],
            )),
        }))
    }

    /// Selects one optional suffix independently of mandatory whole units.
    /// Fixed framing and stable/protected input reserve context capacity first;
    /// the optional percentage is a global upper bound, never a per-gap budget.
    /// This pure plan never constructs a model request or retires an obligation.
    fn prepare_raw_tail(
        &self,
        policy: ReplayTailPolicy,
    ) -> AgentContextResult<ReplayCompactionPreparation> {
        if self.members.is_empty() && self.head.is_none() {
            return Ok(ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::NoRawMembers,
            ));
        }
        let offset = usize::from(self.head.is_some());
        let raw = &self.context.chronology()[offset..];
        let estimates = self
            .provider_input_estimates()
            .unwrap_or(self.input_estimates());
        let mut units = Vec::new();
        let mut raw_total = 0usize;
        let mut protected_raw = 0usize;
        let mut omitted = BTreeSet::new();
        for range in execution_units(raw) {
            let events = &raw[range.clone()];
            let cost = events.iter().try_fold(0usize, |sum, event| {
                sum.checked_add(self.planning_input_tokens(event)?)
                    .ok_or_else(|| AgentContextError::new("raw-tail unit estimate exhausted"))
            })?;
            raw_total = raw_total
                .checked_add(cost)
                .ok_or_else(|| AgentContextError::new("raw-tail source estimate exhausted"))?;
            let protected = events.iter().any(|event| !self.event_is_replaceable(event));
            if protected {
                protected_raw = protected_raw.checked_add(cost).ok_or_else(|| {
                    AgentContextError::new("raw-tail protected estimate exhausted")
                })?;
            } else {
                omitted.extend(events.iter().map(|event| event.record().occurrence_id()));
            }
            units.push((range, cost, protected));
        }
        if raw_total != estimates.raw_input_tokens() {
            return Err(AgentContextError::new("raw-tail source aggregate drifted"));
        }
        let mandatory = estimates
            .stable_input_tokens()
            .checked_add(policy.fixed_input_tokens)
            .and_then(|cost| cost.checked_add(protected_raw))
            .ok_or_else(|| AgentContextError::new("raw-tail mandatory estimate exhausted"))?;
        let Some(available) = policy.context_tokens.checked_sub(mandatory) else {
            return Ok(ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::ProtectedInputExceedsBudget,
            ));
        };
        let allowance = policy.retained_raw_tokens().min(available);
        let mut retained_optional = 0usize;
        let mut retained_optional_units = 0usize;
        for (range, cost, protected) in units.iter().rev() {
            if *protected {
                continue;
            }
            let next = retained_optional
                .checked_add(*cost)
                .ok_or_else(|| AgentContextError::new("raw-tail retained estimate exhausted"))?;
            if next > allowance
                || retained_optional_units >= policy.optional_unit_limit.unwrap_or(usize::MAX)
            {
                // Stop at the first non-fitting optional unit. In particular,
                // never backfill an older small unit past a newer large one.
                break;
            }
            retained_optional = next;
            retained_optional_units += 1;
            for event in &raw[range.clone()] {
                omitted.remove(&event.record().occurrence_id());
            }
        }
        if omitted.is_empty() && self.head.is_none() {
            return Ok(ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::RetainedSourceUnchanged,
            ));
        }
        let retained_raw = protected_raw
            .checked_add(retained_optional)
            .ok_or_else(|| AgentContextError::new("raw-tail retained estimate exhausted"))?;
        let selected_raw = raw_total
            .checked_sub(retained_raw)
            .ok_or_else(|| AgentContextError::new("raw-tail selection estimate drifted"))?;
        let tail_start = raw
            .iter()
            .rposition(|event| omitted.contains(&event.record().occurrence_id()))
            .map_or(offset, |index| offset + index + 1);
        Ok(ReplayCompactionPreparation::Ready(ReplayCompactionPlan {
            owner_identity: self.owner_identity.clone(),
            conversation: self.conversation.clone(),
            parent: self.revision,
            source_policy: self.policy,
            policy: ReplayHeadPolicy::IntentionalOmission,
            head: self.head.clone(),
            frozen: self.context.chronology().to_vec(),
            members: self.members.clone(),
            selection: Box::new(ReplayCompactionSelection::RawTail {
                omitted,
                tail_start,
                retained_optional_units,
            }),
            input_estimates: ReplayCompactionInputEstimates {
                selected_raw,
                prior_summary: estimates.summary_input_tokens(),
                retained_raw,
            },
            provider_projection: self.provider_projection().cloned().map(Box::new),
            // Raw selection naturally becomes NoWork on unchanged retained
            // membership. Only an explicitly smaller real-rejection frontier
            // or fresh optional source can make another omission progress.
            reentry: Box::new(ReplayCompactionReentryInput::capture(
                policy,
                self.provider_projection(),
                &[],
            )),
        }))
    }

    /// Checks metadata against the sole actual chronology. Summary ordering
    /// anchors are deliberately excluded from original raw membership.
    pub(super) fn validate_membership(&self) -> AgentContextResult<()> {
        self.context
            .validate_durable()
            .map_err(|_| AgentContextError::new("replay canonical source validation failed"))?;
        let mut owned = BTreeSet::new();
        for (generation, sequences) in &self.instruction_owners {
            if *generation >= self.next_instruction_owner
                || sequences.iter().any(|sequence| {
                    !owned.insert(*sequence)
                        || !self.context.chronology().iter().any(|event| {
                            event.sequence() == *sequence
                                && instruction_event_can_complete(event)
                                && event.retention() == ContextRetention::Exact
                        })
                })
            {
                return Err(AgentContextError::new(
                    "active instruction ownership drifted from canonical source",
                ));
            }
        }
        if self.context.chronology().iter().any(|event| {
            instruction_event_can_complete(event)
                && event.retention() != ContextRetention::Exact
                && !self
                    .context
                    .historical_instruction_events
                    .contains(&event.sequence())
        }) {
            return Err(AgentContextError::new(
                "task instruction lacks a typed completion witness",
            ));
        }
        let offset = usize::from(self.head.is_some());
        let events = self.context.chronology();
        let head_valid = match &self.head {
            Some(head) => {
                head.conversation == self.conversation
                    && head.revision == self.revision
                    && events.first().is_some_and(|event| {
                        event.block().source == ContextSourceKind::CompactionPrefix
                            && event.record().occurrence_id() == head.occurrence_id
                    })
            }
            None => true,
        };
        if !head_valid
            || events.len() != self.members.len() + offset
            || events
                .iter()
                .skip(offset)
                .any(|event| context_block_is_compaction_summary(event.block()))
            || self
                .members
                .iter()
                .zip(events.iter().skip(offset))
                .any(|(member, event)| {
                    member.origin != event.sequence()
                        || member.born_in > self.revision
                        || member.occurrence_id != event.record().occurrence_id()
                })
        {
            return Err(AgentContextError::new(
                "replay head or raw membership drifted from canonical context",
            ));
        }
        if let Some(reentry) = &self.compaction_reentry {
            reentry.validate(self)?;
        }
        self.validate_mcp_retrievals()?;
        self.validate_accounting()
    }

    /// Requires explicit runtime retirement before selecting protected source.
    pub(super) fn event_is_replaceable(&self, event: &ConversationEvent) -> bool {
        event.retention() != ContextRetention::Exact
            && event.recoverable_for_compaction()
            && !self.context.peer_message_is_pending(event.sequence())
            && !event
                .execution_group_id()
                .is_some_and(|group| self.context.execution_group_is_pending(group))
    }

    /// One common closed-prefix boundary for tail selection and reentry. Whole
    /// ownership spans stop at the first protected event; later units cannot
    /// bypass that barrier in either path.
    pub(super) fn closed_prefix_unit_count(
        &self,
        raw: &[ConversationEvent],
        units: &[std::ops::Range<usize>],
    ) -> usize {
        units
            .iter()
            .position(|unit| {
                raw[unit.clone()]
                    .iter()
                    .any(|event| !self.event_is_replaceable(event))
            })
            .unwrap_or(units.len())
    }
}

/// Partitions raw chronology into disjoint whole ownership spans and singleton
/// ungrouped occurrences. Overlapping/noncontiguous groups merge in O(n log n)
/// work rather than repeatedly rescanning every group at each boundary.
pub(super) fn execution_units(raw: &[ConversationEvent]) -> Vec<std::ops::Range<usize>> {
    let mut bounds = BTreeMap::<ContextExecutionGroupId, (usize, usize)>::new();
    for (index, event) in raw.iter().enumerate() {
        if let Some(group) = event.execution_group_id() {
            bounds
                .entry(group.clone())
                .and_modify(|range| range.1 = index + 1)
                .or_insert((index, index + 1));
        }
    }
    let mut ordered = bounds.into_values().collect::<Vec<_>>();
    ordered.sort_unstable();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ordered {
        if let Some(previous) = spans.last_mut()
            && start < previous.1
        {
            previous.1 = previous.1.max(end);
        } else {
            spans.push((start, end));
        }
    }
    let mut units = Vec::new();
    let mut next = 0usize;
    for (start, end) in spans {
        while next < start {
            units.push(next..next + 1);
            next += 1;
        }
        units.push(start..end);
        next = end;
    }
    while next < raw.len() {
        units.push(next..next + 1);
        next += 1;
    }
    units
}
