//! Shared frozen-prefix/raw-tail policy. No trigger-specific retry ladder or
//! advisory summary-size target participates in source eligibility.

use super::*;

impl ReplayEpochContext {
    /// Freezes one eligible closed prefix and the configured raw tail. Exact
    /// instructions, pending peer/native groups and unrecoverable evidence push
    /// the boundary earlier; complete groups are never split or skipped around.
    /// Empty/irreducible source returns NoWork without creating model activity.
    /// A tail reservation that covers all closed source still permits one oldest
    /// complete prefix unit, so requested progress needs no trigger-specific bypass.
    pub fn prepare_compaction(
        &self,
        policy: ReplayTailPolicy,
    ) -> AgentContextResult<ReplayCompactionPreparation> {
        self.validate_membership()?;
        if self.members.is_empty() {
            return Ok(ReplayCompactionPreparation::NoWork(
                ReplayCompactionNoWork::NoRawMembers,
            ));
        }
        let offset = usize::from(self.head.is_some());
        let raw = &self.context.chronology()[offset..];
        let units = execution_units(raw);
        let protected = units
            .iter()
            .position(|unit| {
                raw[unit.clone()]
                    .iter()
                    .any(|event| !self.event_is_replaceable(event))
            })
            .unwrap_or(units.len());
        let target = policy.retained_raw_tokens();
        let mut selected = units.len();
        let mut retained = 0usize;
        for (index, unit) in units.iter().enumerate().rev() {
            let cost = raw[unit.clone()]
                .iter()
                .map(|event| event.metadata().estimated_input_tokens())
                .fold(0usize, usize::saturating_add);
            let Some(total) = retained.checked_add(cost).filter(|total| *total <= target) else {
                break;
            };
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
        Ok(ReplayCompactionPreparation::Ready(ReplayCompactionPlan {
            owner_identity: self.owner_identity.clone(),
            conversation: self.conversation.clone(),
            parent: self.revision,
            policy: self.policy,
            head: self.head.clone(),
            frozen: self.context.chronology().to_vec(),
            members: self.members.clone(),
            prefix_members: cut,
            prefix_events: offset + cut,
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
                                && instruction_source_can_complete(event.block())
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
            instruction_source_can_complete(event.block())
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
                    member.origin != event.sequence() || member.born_in > self.revision
                })
        {
            return Err(AgentContextError::new(
                "replay head or raw membership drifted from canonical context",
            ));
        }
        Ok(())
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
}

/// Partitions raw chronology into disjoint whole ownership spans and singleton
/// ungrouped occurrences. Overlapping/noncontiguous groups merge in O(n log n)
/// work rather than repeatedly rescanning every group at each boundary.
fn execution_units(raw: &[ConversationEvent]) -> Vec<std::ops::Range<usize>> {
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
