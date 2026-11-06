//! Causal source qualification against an already committed replay parent.
//!
//! A valid standalone checkpoint can describe a different branch. Publication
//! must additionally preserve the parent's actual head and complete raw prefix,
//! including records, order and birth revisions. Only genuine arrivals and
//! controller-minted lifecycle transitions may extend that source. Ephemeral
//! witnesses below are created by existing typed completion/handoff methods,
//! cloned with their owner, pruned with covered membership and never serialized.
//! Thus descriptive checkpoint metadata cannot itself retire protected source.

use super::*;

/// Small source-lifecycle witnesses, not process, tool, task or ACK authority.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct ReplaySourceLifecycle {
    pub(super) completed_instructions: BTreeSet<ContextBlockOccurrenceId>,
    /// Actual task admission binds its root to the first original suffix member.
    /// A descriptive checkpoint cannot recreate this producer witness.
    pub(super) task_admissions: BTreeMap<ContextBlockOccurrenceId, ContextBlockOccurrenceId>,
    pub(super) handed_off_peers: BTreeSet<ContextBlockOccurrenceId>,
    pub(super) closed_groups: BTreeMap<ContextExecutionGroupId, ReplayExecutionClosure>,
    /// Actual prompt-boundary insertions, qualified before owner reacquisition.
    pub(super) inserted_preludes: BTreeSet<ContextBlockOccurrenceId>,
    /// Actual arrivals carried across preparation/adoption, retaining their
    /// earlier birth revision. Cold descriptive state cannot mint this proof.
    pub(super) carried_arrivals: BTreeMap<ContextBlockOccurrenceId, ReplayEpochMember>,
}

/// Exact completed protection interval, never rebuilt from descriptive bytes.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ReplayExecutionClosure {
    pub(super) frontier: ContextEventSequence,
    pub(super) protection: ContextExecutionProtection,
}

impl ReplayEpochContext {
    /// Qualifies this pre-publication source against the actual committed parent.
    /// Stable instructions, request metadata and provider views remain live
    /// caller inputs. The head, existing raw bytes/framing/records/order/births,
    /// conversation and epoch must match. New events must start at the parent's
    /// reserved frontier, except actual controller-witnessed late task preludes,
    /// and be born in this epoch unless actual prepared-head adoption retained
    /// their earlier birth with a private arrival witness; reservations never rewind.
    ///
    /// Existing exact instructions may become historical only through this
    /// owner's typed task completion. Peer protection may end only through its
    /// local handoff method, and a pending group may close only with its exact
    /// source frontier and validated native closure. Restoring descriptive state
    /// creates none of these transition witnesses. This method performs no I/O,
    /// mutation, effect replay or transport acknowledgment. Any unsupported
    /// rewrite or unqualified lifecycle transition returns a content-free error.
    pub fn validate_source_extension(&self, previous: &Self) -> AgentContextResult<()> {
        previous.validate_membership()?;
        self.validate_membership()?;
        if self.conversation != previous.conversation
            || self.revision != previous.revision
            || self.policy != previous.policy
            || self.head != previous.head
            || !self.members.starts_with(&previous.members)
            || self.context.next_event_sequence < previous.context.next_event_sequence
            || !previous
                .context
                .retired_mcp_retrievals
                .is_subset(&self.context.retired_mcp_retrievals)
            || !previous
                .context
                .historical_instruction_events
                .is_subset(&self.context.historical_instruction_events)
        {
            return Err(invalid());
        }
        let old_events = previous.context.chronology();
        let events = self.context.chronology();
        if events.len() < old_events.len()
            || self.members[previous.members.len()..].iter().any(|member| {
                member.born_in != self.revision
                    && self
                        .source_lifecycle
                        .carried_arrivals
                        .get(&member.occurrence_id)
                        != Some(member)
            })
            || events.iter().skip(old_events.len()).any(|arrival| {
                arrival.sequence().get() < previous.context.next_event_sequence
                    && !self.is_witnessed_late_prelude(arrival)
            })
        {
            return Err(invalid());
        }
        for (old, current) in old_events.iter().zip(events) {
            if previous.context.peer_message_is_pending(old.sequence())
                && !self.context.peer_message_is_pending(old.sequence())
                && !self
                    .source_lifecycle
                    .handed_off_peers
                    .contains(&old.record().occurrence_id())
            {
                return Err(invalid());
            }
            if old == current {
                continue;
            }
            let mut completed = old.clone();
            if old.retention == ContextRetention::Exact
                && current.retention == ContextRetention::Summarizable
                && instruction_source_can_complete(old.block())
                && self
                    .source_lifecycle
                    .completed_instructions
                    .contains(&old.record().occurrence_id())
                && self
                    .context
                    .historical_instruction_events
                    .contains(&old.sequence())
            {
                completed.retention = ContextRetention::Summarizable;
            }
            if &completed != current {
                return Err(invalid());
            }
        }
        let frontiers = group_frontiers(events);
        for group in previous.context.pending_execution_groups.keys() {
            if !self.context.execution_group_is_pending(group)
                && frontiers.get(group).is_none_or(|frontier| {
                    self.source_lifecycle
                        .closed_groups
                        .get(group)
                        .is_none_or(|closed| &closed.frontier != frontier)
                })
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Actual insertion remains qualified after this owner's actual task
    /// completion. Both private witnesses and historical membership are needed;
    /// deserializing a summarizable prelude cannot mint either transition.
    fn is_witnessed_late_prelude(&self, event: &ConversationEvent) -> bool {
        event.semantic_kind() == super::super::ContextSemanticKind::TaskPrelude
            && self
                .source_lifecycle
                .inserted_preludes
                .contains(&event.record().occurrence_id())
            && (event.retention() == ContextRetention::Exact
                || event.retention() == ContextRetention::Summarizable
                    && self
                        .source_lifecycle
                        .completed_instructions
                        .contains(&event.record().occurrence_id())
                    && self
                        .context
                        .historical_instruction_events
                        .contains(&event.sequence()))
    }

    /// Keeps ephemeral source proofs bounded by actual retained occurrences.
    pub(super) fn prune_source_lifecycle(&mut self) {
        let members = self
            .members
            .iter()
            .map(|member| member.occurrence_id)
            .collect::<BTreeSet<_>>();
        self.source_lifecycle
            .completed_instructions
            .retain(|id| members.contains(id));
        self.source_lifecycle
            .task_admissions
            .retain(|id, _| members.contains(id));
        self.source_lifecycle
            .handed_off_peers
            .retain(|id| members.contains(id));
        self.source_lifecycle
            .inserted_preludes
            .retain(|id| members.contains(id));
        self.source_lifecycle
            .carried_arrivals
            .retain(|id, _| members.contains(id));
        let frontiers = group_frontiers(self.context.chronology());
        let pending = &self.context.pending_execution_groups;
        self.source_lifecycle.closed_groups.retain(|group, closed| {
            !pending.contains_key(group) && frontiers.get(group) == Some(&closed.frontier)
        });
    }
}

/// Indexes actual last group occurrences once, without native payload decoding.
fn group_frontiers(
    events: &[ConversationEvent],
) -> BTreeMap<ContextExecutionGroupId, ContextEventSequence> {
    events
        .iter()
        .filter_map(|event| {
            event
                .execution_group_id()
                .map(|group| (group.clone(), event.sequence()))
        })
        .collect()
}

/// Avoids disclosing original head, raw bytes or producer identifiers.
fn invalid() -> AgentContextError {
    AgentContextError::new("replay source is not a qualified extension of its committed parent")
}
